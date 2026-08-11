//! Resident engine process boundary and authenticated local sessions.

mod local_history;
mod market_service;
mod rithmic_history;
mod rithmic_realtime;

pub use market_service::MarketService;

use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_local_engine_client::INSTALLATION_TOKEN_BYTES;
use axiusflow_local_engine_protocol::{
    ClientKind, EngineFaultCode, EngineReady, Envelope, EnvelopeDecoder, Fault, Goodbye, HotSeries,
    MarketEventIdle, PROTOCOL_VERSION, ProviderInstrumentInstalled, ResourceMode, SetSelection,
    SetViewport, SetWatchlist, WorkspaceState, encode_envelope, envelope,
};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};
const WORKSPACE_SCHEMA_REVISION: u32 = 1;
const CACHE_MANIFEST_REVISION: u32 = 1;
const MAXIMUM_HOT_SERIES: usize = 32;
const WORKSPACE_SHUTTING_DOWN: &str = "engine workspace is shutting down";

/// Process-wide resident-engine shutdown state shared with authenticated sessions.
#[derive(Clone, Default)]
pub struct EngineShutdown {
    requested: Arc<AtomicBool>,
}

impl EngineShutdown {
    /// Returns whether complete engine shutdown has been authenticated and accepted.
    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }
}

/// Shared resident-engine state visible to authenticated clients.
#[derive(Clone)]
pub struct EngineState {
    workspace: Arc<Mutex<WorkspaceState>>,
    workspace_root: Option<Arc<PathBuf>>,
    selection_generation: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            workspace: Arc::new(Mutex::new(default_workspace())),
            workspace_root: None,
            selection_generation: Arc::new(AtomicU64::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl EngineState {
    /// Opens immutable revisioned workspace state below an application-owned directory.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be created or durable state cannot be written.
    pub fn open(workspace_root: impl Into<PathBuf>) -> Result<Self, String> {
        let workspace_root = workspace_root.into();
        fs::create_dir_all(&workspace_root).map_err(redacted_workspace_error)?;
        let mut workspace =
            load_latest_workspace(&workspace_root)?.unwrap_or_else(default_workspace);
        if let Some(hot_set) = load_latest_hot_set(&workspace_root)?
            && hot_set.cache_manifest_revision >= workspace.cache_manifest_revision
        {
            workspace.cache_manifest_revision = hot_set.cache_manifest_revision;
            workspace.hot_series = hot_set.hot_series;
        }
        let migrated = migrate_workspace(&mut workspace);
        if migrated {
            workspace.workspace_revision = workspace.workspace_revision.saturating_add(1);
        }
        let state = Self {
            workspace: Arc::new(Mutex::new(workspace)),
            workspace_root: Some(Arc::new(workspace_root)),
            selection_generation: Arc::new(AtomicU64::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
        };
        if state.workspace().workspace_revision == 0 || migrated {
            state.persist(&state.workspace())?;
        }
        Ok(state)
    }

    /// Returns a consistent copy of the current workspace state.
    #[must_use]
    pub fn workspace(&self) -> WorkspaceState {
        self.workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Updates the current operational resource mode without changing user state.
    pub fn set_resource_mode(&self, mode: ResourceMode) -> WorkspaceState {
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        workspace.resource_mode = mode as i32;
        workspace.clone()
    }

    /// Persists one final revisioned hot-set snapshot after shutdown begins.
    ///
    /// # Errors
    /// Returns an error when the state is invalid or durable publication fails.
    pub fn persist_shutdown_hot_set(&self) -> Result<WorkspaceState, String> {
        self.begin_shutdown();
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut candidate = workspace.clone();
        candidate.cache_manifest_revision = candidate.cache_manifest_revision.saturating_add(1);
        validate_workspace(&candidate)?;
        if let Some(root) = &self.workspace_root {
            persist_hot_set(root, &candidate)?;
        }
        *workspace = candidate.clone();
        Ok(candidate)
    }

    fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
    }

    fn ensure_mutable(&self) -> Result<(), String> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(WORKSPACE_SHUTTING_DOWN.to_string());
        }
        Ok(())
    }

    fn apply_selection(&self, selection: SetSelection) -> Result<WorkspaceState, String> {
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_mutable()?;
        if selection.workspace_revision != workspace.workspace_revision {
            return Err("workspace revision is stale".to_string());
        }
        let mut candidate = workspace.clone();
        if !selection.provider.is_empty() {
            candidate.provider = selection.provider;
        }
        candidate.market = selection.market;
        candidate.interval_seconds = selection.interval_seconds;
        candidate.workspace_revision = candidate.workspace_revision.saturating_add(1);
        touch_hot_series(&mut candidate);
        validate_workspace(&candidate)?;
        self.persist(&candidate)?;
        *workspace = candidate.clone();
        self.selection_generation
            .store(selection.selection_generation, Ordering::Release);
        Ok(candidate)
    }

    fn apply_viewport(&self, viewport: SetViewport) -> Result<WorkspaceState, String> {
        if viewport.start_unix_nanos >= viewport.end_unix_nanos {
            return Err("chart viewport is invalid".to_string());
        }
        if viewport.selection_generation != self.selection_generation.load(Ordering::Acquire) {
            return Err("chart viewport selection is stale".to_string());
        }
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_mutable()?;
        let mut candidate = workspace.clone();
        let provider = candidate.provider.clone();
        let market = candidate.market.clone();
        let interval_seconds = candidate.interval_seconds;
        let Some(series) = candidate.hot_series.iter_mut().find(|series| {
            series.provider == provider
                && series.market == market
                && series.interval_seconds == interval_seconds
        }) else {
            return Err("active chart is absent from the hot set".to_string());
        };
        if series.viewport_start_unix_nanos == Some(viewport.start_unix_nanos)
            && series.viewport_end_unix_nanos == Some(viewport.end_unix_nanos)
        {
            return Ok(workspace.clone());
        }
        series.viewport_start_unix_nanos = Some(viewport.start_unix_nanos);
        series.viewport_end_unix_nanos = Some(viewport.end_unix_nanos);
        candidate.cache_manifest_revision = candidate.cache_manifest_revision.saturating_add(1);
        validate_workspace(&candidate)?;
        if let Some(root) = &self.workspace_root {
            persist_hot_set(root, &candidate)?;
        }
        *workspace = candidate.clone();
        Ok(candidate)
    }

    fn apply_watchlist(&self, watchlist: SetWatchlist) -> Result<WorkspaceState, String> {
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_mutable()?;
        if watchlist.workspace_revision != workspace.workspace_revision {
            return Err("workspace revision is stale".to_string());
        }
        let mut candidate = workspace.clone();
        candidate.watchlist = watchlist.markets;
        candidate.workspace_revision = candidate.workspace_revision.saturating_add(1);
        validate_workspace(&candidate)?;
        self.persist(&candidate)?;
        *workspace = candidate.clone();
        Ok(candidate)
    }

    fn persist(&self, workspace: &WorkspaceState) -> Result<(), String> {
        let Some(root) = &self.workspace_root else {
            return Ok(());
        };
        persist_workspace(root, workspace)
    }
}

fn default_workspace() -> WorkspaceState {
    WorkspaceState {
        provider: "coinbase".to_string(),
        market: "BTC-USD".to_string(),
        interval_seconds: 60,
        watchlist: vec!["BTC-USD".to_string(), "ETH-USD".to_string()],
        workspace_revision: 0,
        warm_mode_enabled: true,
        resource_mode: ResourceMode::Warm as i32,
        schema_revision: WORKSPACE_SCHEMA_REVISION,
        cache_manifest_revision: CACHE_MANIFEST_REVISION,
        hot_series: vec![HotSeries {
            provider: "coinbase".to_string(),
            market: "BTC-USD".to_string(),
            interval_seconds: 60,
            score: 1,
            last_used_unix_seconds: unix_seconds(),
            provider_watermark: 0,
            series_watermark: 0,
            viewport_start_unix_nanos: None,
            viewport_end_unix_nanos: None,
        }],
    }
}

fn validate_workspace(workspace: &WorkspaceState) -> Result<(), String> {
    const MAXIMUM_MARKET_BYTES: usize = 128;
    const MAXIMUM_WATCHLIST_ITEMS: usize = 256;
    let mut hot_identities = BTreeSet::new();

    if workspace.provider.trim().is_empty()
        || workspace.market.trim().is_empty()
        || workspace.market.len() > MAXIMUM_MARKET_BYTES
        || workspace.interval_seconds == 0
        || workspace.watchlist.len() > MAXIMUM_WATCHLIST_ITEMS
        || workspace
            .watchlist
            .iter()
            .any(|market| market.trim().is_empty() || market.len() > MAXIMUM_MARKET_BYTES)
        || workspace.hot_series.len() > MAXIMUM_HOT_SERIES
        || workspace.hot_series.iter().any(|series| {
            series.provider.trim().is_empty()
                || series.market.trim().is_empty()
                || series.market.len() > MAXIMUM_MARKET_BYTES
                || series.interval_seconds == 0
                || series.score == 0
                || !hot_identities.insert((
                    series.provider.clone(),
                    series.market.clone(),
                    series.interval_seconds,
                ))
                || !valid_viewport(series)
        })
    {
        return Err("workspace state is invalid".to_string());
    }
    if !matches!(workspace.schema_revision, 0 | WORKSPACE_SCHEMA_REVISION)
        || workspace.cache_manifest_revision == u32::MAX
    {
        return Err("workspace revision is unsupported".to_string());
    }
    if ResourceMode::try_from(workspace.resource_mode).is_err() {
        return Err("workspace resource mode is invalid".to_string());
    }
    Ok(())
}

fn valid_viewport(series: &HotSeries) -> bool {
    match (
        series.viewport_start_unix_nanos,
        series.viewport_end_unix_nanos,
    ) {
        (None, None) => true,
        (Some(start), Some(end)) => start < end,
        _ => false,
    }
}

fn migrate_workspace(workspace: &mut WorkspaceState) -> bool {
    let mut migrated = false;
    if workspace.schema_revision == 0 {
        workspace.schema_revision = WORKSPACE_SCHEMA_REVISION;
        migrated = true;
    }
    if workspace.cache_manifest_revision == 0 {
        workspace.cache_manifest_revision = CACHE_MANIFEST_REVISION;
        migrated = true;
    }
    if workspace.hot_series.is_empty() {
        touch_hot_series(workspace);
        migrated = true;
    }
    migrated
}

fn touch_hot_series(workspace: &mut WorkspaceState) {
    let identity = (
        &workspace.provider,
        &workspace.market,
        workspace.interval_seconds,
    );
    let now = unix_seconds();
    let next_score = workspace
        .hot_series
        .iter()
        .map(|series| series.score)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    if let Some(series) = workspace.hot_series.iter_mut().find(|series| {
        series.provider == *identity.0
            && series.market == *identity.1
            && series.interval_seconds == identity.2
    }) {
        series.score = next_score;
        series.last_used_unix_seconds = now;
    } else {
        workspace.hot_series.push(HotSeries {
            provider: workspace.provider.clone(),
            market: workspace.market.clone(),
            interval_seconds: workspace.interval_seconds,
            score: next_score,
            last_used_unix_seconds: now,
            provider_watermark: 0,
            series_watermark: 0,
            viewport_start_unix_nanos: None,
            viewport_end_unix_nanos: None,
        });
    }
    workspace.hot_series.sort_unstable_by(|left, right| {
        right
            .last_used_unix_seconds
            .cmp(&left.last_used_unix_seconds)
            .then_with(|| right.score.cmp(&left.score))
    });
    workspace.hot_series.truncate(MAXIMUM_HOT_SERIES);
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn workspace_filename(revision: u64) -> String {
    format!("workspace-{revision:020}.frame")
}

fn hot_set_filename(revision: u32) -> String {
    format!("hot-set-{revision:010}.frame")
}

fn persist_workspace(root: &Path, workspace: &WorkspaceState) -> Result<(), String> {
    validate_workspace(workspace)?;
    let path = root.join(workspace_filename(workspace.workspace_revision));
    if path.exists() {
        return Ok(());
    }
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(envelope::Payload::WorkspaceState(workspace.clone())),
    })
    .map_err(|_| "workspace state could not be encoded".to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(redacted_workspace_error)?;
    file.write_all(&bytes).map_err(redacted_workspace_error)?;
    file.sync_all().map_err(redacted_workspace_error)
}

fn load_latest_workspace(root: &Path) -> Result<Option<WorkspaceState>, String> {
    let mut candidates = fs::read_dir(root)
        .map_err(redacted_workspace_error)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let revision = name
                .strip_prefix("workspace-")?
                .strip_suffix(".frame")?
                .parse::<u64>()
                .ok()?;
            Some((revision, entry.path()))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    for (revision, path) in candidates {
        match decode_workspace_file(&path, revision) {
            Ok(workspace) => return Ok(Some(workspace)),
            Err(()) => quarantine_workspace_file(&path)?,
        }
    }
    Ok(None)
}

fn persist_hot_set(root: &Path, workspace: &WorkspaceState) -> Result<(), String> {
    validate_workspace(workspace)?;
    let path = root.join(hot_set_filename(workspace.cache_manifest_revision));
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(envelope::Payload::WorkspaceState(workspace.clone())),
    })
    .map_err(|_| "workspace hot set could not be encoded".to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(redacted_workspace_error)?;
    file.write_all(&bytes).map_err(redacted_workspace_error)?;
    file.sync_all().map_err(redacted_workspace_error)?;
    for (_, stale) in hot_set_files(root)?.into_iter().skip(2) {
        fs::remove_file(stale).map_err(redacted_workspace_error)?;
    }
    Ok(())
}

fn load_latest_hot_set(root: &Path) -> Result<Option<WorkspaceState>, String> {
    for (revision, path) in hot_set_files(root)? {
        match decode_hot_set_file(&path, revision) {
            Ok(workspace) => return Ok(Some(workspace)),
            Err(()) => quarantine_workspace_file(&path)?,
        }
    }
    Ok(None)
}

fn hot_set_files(root: &Path) -> Result<Vec<(u32, PathBuf)>, String> {
    let mut files = fs::read_dir(root)
        .map_err(redacted_workspace_error)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let revision = name
                .strip_prefix("hot-set-")?
                .strip_suffix(".frame")?
                .parse::<u32>()
                .ok()?;
            Some((revision, entry.path()))
        })
        .collect::<Vec<_>>();
    files.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    Ok(files)
}

fn decode_hot_set_file(path: &Path, revision: u32) -> Result<WorkspaceState, ()> {
    let workspace = decode_workspace_payload(path)?;
    if workspace.cache_manifest_revision != revision || validate_workspace(&workspace).is_err() {
        return Err(());
    }
    Ok(workspace)
}

fn decode_workspace_file(path: &Path, revision: u64) -> Result<WorkspaceState, ()> {
    let workspace = decode_workspace_payload(path)?;
    if workspace.workspace_revision != revision || validate_workspace(&workspace).is_err() {
        return Err(());
    }
    Ok(workspace)
}

fn decode_workspace_payload(path: &Path) -> Result<WorkspaceState, ()> {
    let bytes = fs::read(path).map_err(|_| ())?;
    let mut decoder = EnvelopeDecoder::try_new().map_err(|_| ())?;
    let mut envelopes = decoder.push(&bytes).map_err(|_| ())?;
    if envelopes.len() != 1 {
        return Err(());
    }
    let Some(envelope::Payload::WorkspaceState(workspace)) =
        envelopes.pop().and_then(|item| item.payload)
    else {
        return Err(());
    };
    Ok(workspace)
}

fn quarantine_workspace_file(path: &Path) -> Result<(), String> {
    for suffix in 0_u16..=u16::MAX {
        let quarantine = path.with_extension(format!("corrupt-{suffix}"));
        if !quarantine.exists() {
            return fs::rename(path, quarantine).map_err(redacted_workspace_error);
        }
    }
    Err("corrupt workspace state could not be quarantined".to_string())
}

fn redacted_workspace_error<E>(_error: E) -> String {
    "engine workspace storage is unavailable".to_string()
}

/// Resolves the application-owned resident-engine state directory.
///
/// # Errors
/// Returns an error when neither the native data root nor current directory is available.
pub fn default_engine_state_root() -> Result<PathBuf, String> {
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(root).join("Axiusflow").join("engine"));
    }
    std::env::current_dir()
        .map(|root| root.join("local-data").join("engine"))
        .map_err(|error| error.to_string())
}

struct FramedConnection {
    stream: LocalSocketStream,
    decoder: EnvelopeDecoder,
    pending: VecDeque<Envelope>,
}

impl FramedConnection {
    fn new(stream: LocalSocketStream) -> Result<Self, String> {
        Ok(Self {
            stream,
            decoder: EnvelopeDecoder::try_new().map_err(|error| error.to_string())?,
            pending: VecDeque::new(),
        })
    }

    fn send(&mut self, payload: envelope::Payload) -> Result<(), String> {
        let frame = encode_envelope(&Envelope {
            protocol_version: PROTOCOL_VERSION,
            payload: Some(payload),
        })
        .map_err(|error| error.to_string())?;
        self.stream
            .write_all(&frame)
            .map_err(|error| error.to_string())?;
        self.stream.flush().map_err(|error| error.to_string())
    }

    fn receive(&mut self) -> Result<envelope::Payload, String> {
        loop {
            if let Some(envelope) = self.pending.pop_front() {
                return envelope
                    .payload
                    .ok_or_else(|| "engine message has no payload".to_string());
            }
            let mut chunk = [0_u8; 16 * 1024];
            let count = self
                .stream
                .read(&mut chunk)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                return Err("local engine connection closed".to_string());
            }
            self.pending.extend(
                self.decoder
                    .push(&chunk[..count])
                    .map_err(|error| error.to_string())?,
            );
        }
    }
}

/// Creates the engine's single-instance local listener.
///
/// # Errors
/// Returns an I/O error if another engine owns the name or the OS rejects it.
pub fn bind_listener(name: &str) -> io::Result<LocalSocketListener> {
    let name = name.to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new().name(name).create_sync()
}

/// Handles one client against isolated default state. Intended for probes and tests.
///
/// # Errors
/// Returns an error for I/O, framing, or malformed-message failures.
pub fn serve_client(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
) -> Result<(), String> {
    serve_client_with_state(
        stream,
        installation_token,
        engine_epoch,
        &EngineState::default(),
    )
}

/// Serves one authenticated client against shared resident-engine state.
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_state(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
    state: &EngineState,
) -> Result<(), String> {
    serve_client_with_services(stream, installation_token, engine_epoch, state, None, None)
}

/// Serves one authenticated client with workspace and resident market ownership.
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_market(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
    state: &EngineState,
    market: &MarketService,
) -> Result<(), String> {
    serve_client_with_services(
        stream,
        installation_token,
        engine_epoch,
        state,
        Some(market),
        None,
    )
}

/// Serves one authenticated client with market ownership and process shutdown control.
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_market_and_shutdown(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
    state: &EngineState,
    market: &MarketService,
    shutdown: &EngineShutdown,
) -> Result<(), String> {
    serve_client_with_services(
        stream,
        installation_token,
        engine_epoch,
        state,
        Some(market),
        Some(shutdown),
    )
}

fn serve_client_with_services(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
    state: &EngineState,
    market: Option<&MarketService>,
    shutdown: Option<&EngineShutdown>,
) -> Result<(), String> {
    if installation_token.len() != INSTALLATION_TOKEN_BYTES {
        return Err("installation credential has an invalid length".to_string());
    }
    let mut connection = FramedConnection::new(stream)?;
    let envelope::Payload::ClientHello(hello) = connection.receive()? else {
        return Err("client hello must be the first engine message".to_string());
    };
    if ClientKind::try_from(hello.client_kind).is_err() {
        return Err("client kind is invalid".to_string());
    }
    if !constant_time_equals(&hello.installation_token, installation_token) {
        connection.send(envelope::Payload::Fault(Fault {
            code: EngineFaultCode::Unauthenticated as i32,
            redacted_detail: "local engine authentication failed".to_string(),
        }))?;
        return Ok(());
    }
    connection.send(envelope::Payload::EngineReady(EngineReady {
        protocol_version: PROTOCOL_VERSION,
        engine_epoch,
        workspace_revision: state.workspace().workspace_revision,
    }))?;
    serve_authenticated_session(&mut connection, state, market, shutdown)
}

fn serve_authenticated_session(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    shutdown: Option<&EngineShutdown>,
) -> Result<(), String> {
    let mut attached_client = None;
    let result =
        serve_authenticated_messages(connection, state, market, shutdown, &mut attached_client);
    if let (Some(market), Some(client_id)) = (market, attached_client) {
        let _ = market.detach(client_id);
    }
    result
}

fn serve_authenticated_messages(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    shutdown: Option<&EngineShutdown>,
    attached_client: &mut Option<u64>,
) -> Result<(), String> {
    loop {
        let payload = match connection.receive() {
            Ok(payload) => payload,
            Err(error) if error == "local engine connection closed" => return Ok(()),
            Err(error) => return Err(error),
        };
        if shutdown.is_some_and(EngineShutdown::is_requested) {
            connection.send(cancelled_mutation_fault("engine is shutting down"))?;
            return Ok(());
        }
        match payload {
            envelope::Payload::RestoreWorkspace(_) => {
                connection.send(envelope::Payload::WorkspaceState(state.workspace()))?;
            }
            envelope::Payload::SetSelection(selection) => {
                apply_selection(state, selection, connection)?;
            }
            envelope::Payload::SetWatchlist(watchlist) => {
                apply_watchlist(state, watchlist, connection)?;
            }
            envelope::Payload::SetViewport(viewport) => {
                apply_viewport(state, viewport, connection)?;
            }
            envelope::Payload::SetEngineResourceMode(command) => {
                let Ok(mode) = ResourceMode::try_from(command.resource_mode) else {
                    connection.send(envelope::Payload::Fault(Fault {
                        code: EngineFaultCode::MalformedMessage as i32,
                        redacted_detail: "engine resource mode is invalid".to_string(),
                    }))?;
                    continue;
                };
                if let Some(market) = market
                    && let Err(error) = market.set_resource_mode(mode)
                {
                    send_market_fault(connection, error)?;
                    continue;
                }
                connection.send(envelope::Payload::WorkspaceState(
                    state.set_resource_mode(mode),
                ))?;
            }
            envelope::Payload::ShutdownEngine(_) => {
                let Some(shutdown) = shutdown else {
                    connection.send(cancelled_mutation_fault(
                        "engine shutdown is unavailable in this session",
                    ))?;
                    continue;
                };
                state.begin_shutdown();
                state.set_resource_mode(ResourceMode::OfflineSuspended);
                shutdown.request();
                connection.send(envelope::Payload::Goodbye(Goodbye {
                    reason: "engine shutdown accepted".to_string(),
                }))?;
                return Ok(());
            }
            envelope::Payload::InstallProviderInstrument(instrument) => {
                let market = require_market(market)?;
                match market.install_provider_instrument(&instrument) {
                    Ok(()) => connection.send(envelope::Payload::ProviderInstrumentInstalled(
                        ProviderInstrumentInstalled {
                            provider: instrument.provider,
                            session_generation: instrument.session_generation,
                            selection_generation: instrument.selection_generation,
                            instrument_id: instrument.instrument_id,
                        },
                    )),
                    Err(error) => send_market_fault(connection, error),
                }?;
            }
            envelope::Payload::Goodbye(_) => {
                connection.send(envelope::Payload::Goodbye(Goodbye {
                    reason: "client session closed".to_string(),
                }))?;
                return Ok(());
            }
            payload => {
                if !handle_market_message(connection, market, attached_client, payload)? {
                    connection.send(envelope::Payload::Fault(Fault {
                        code: EngineFaultCode::MalformedMessage as i32,
                        redacted_detail: "message is invalid in the current engine state"
                            .to_string(),
                    }))?;
                }
            }
        }
    }
}

fn handle_market_message(
    connection: &mut FramedConnection,
    market: Option<&MarketService>,
    attached_client: &mut Option<u64>,
    payload: envelope::Payload,
) -> Result<bool, String> {
    let market = match payload {
        envelope::Payload::AttachClient(attachment) => {
            let market = require_market(market)?;
            if attached_client.is_some() {
                send_market_fault(connection, "client is already attached")?;
            } else if let Err(error) = market.attach(attachment.client_id) {
                send_market_fault(connection, error)?;
            } else {
                *attached_client = Some(attachment.client_id);
            }
            return Ok(true);
        }
        envelope::Payload::DetachClient(detachment) => {
            let market = require_market(market)?;
            if *attached_client != Some(detachment.client_id) {
                send_market_fault(connection, "client attachment does not match")?;
            } else if let Err(error) = market.detach(detachment.client_id) {
                send_market_fault(connection, error)?;
            } else {
                *attached_client = None;
            }
            return Ok(true);
        }
        payload @ (envelope::Payload::RegisterConsumer(_)
        | envelope::Payload::SeriesDemand(_)
        | envelope::Payload::ViewportDemand(_)
        | envelope::Payload::VisibilityDemand(_)
        | envelope::Payload::RemoveConsumer(_)
        | envelope::Payload::SearchProviderInstruments(_)
        | envelope::Payload::SelectProviderInstrument(_)
        | envelope::Payload::PollMarketEvent(_)) => (require_market(market)?, payload),
        _ => return Ok(false),
    };
    dispatch_market_command(connection, market.0, *attached_client, market.1)?;
    Ok(true)
}

fn dispatch_market_command(
    connection: &mut FramedConnection,
    market: &MarketService,
    attached_client: Option<u64>,
    payload: envelope::Payload,
) -> Result<(), String> {
    if matches!(
        &payload,
        envelope::Payload::SearchProviderInstruments(_)
            | envelope::Payload::SelectProviderInstrument(_)
    ) {
        return dispatch_provider_catalog_command(connection, market, attached_client, payload);
    }
    match payload {
        envelope::Payload::RegisterConsumer(registration) => {
            if attached_client != Some(registration.client_id) {
                send_market_fault(connection, "consumer owner is not attached")?;
            } else if let Err(error) = market.register_consumer(
                registration.client_id,
                registration.workspace_id,
                registration.consumer_id,
            ) {
                send_market_fault(connection, error)?;
            }
        }
        envelope::Payload::SeriesDemand(demand) => {
            let Some(client_id) = attached_client else {
                send_market_fault(connection, "client must attach before setting demand")?;
                return Ok(());
            };
            let Some(series) = demand.series else {
                send_market_fault(connection, "series demand has no identity")?;
                return Ok(());
            };
            if let Err(error) =
                market.set_demand(client_id, demand.consumer_id, demand.generation, &series)
            {
                send_market_fault(connection, error)?;
            }
        }
        envelope::Payload::ViewportDemand(viewport) => {
            let Some(client_id) = attached_client else {
                send_market_fault(connection, "client must attach before setting viewport")?;
                return Ok(());
            };
            if let Err(error) = market.set_viewport(
                client_id,
                viewport.consumer_id,
                viewport.generation,
                viewport.start_unix_nanos,
                viewport.end_unix_nanos,
            ) {
                send_market_fault(connection, error)?;
            }
        }
        envelope::Payload::VisibilityDemand(visibility) => {
            let Some(client_id) = attached_client else {
                send_market_fault(connection, "client must attach before setting visibility")?;
                return Ok(());
            };
            if let Err(error) =
                market.set_visibility(client_id, visibility.consumer_id, visibility.visible)
            {
                send_market_fault(connection, error)?;
            }
        }
        envelope::Payload::RemoveConsumer(removal) => {
            let Some(client_id) = attached_client else {
                send_market_fault(connection, "client must attach before removing a consumer")?;
                return Ok(());
            };
            if let Err(error) = market.remove_consumer(client_id, removal.consumer_id) {
                send_market_fault(connection, error)?;
            }
        }
        envelope::Payload::PollMarketEvent(poll) => {
            let Some(client_id) = attached_client else {
                send_market_fault(
                    connection,
                    "client must attach before polling market events",
                )?;
                return Ok(());
            };
            match market.poll_event(client_id, poll.consumer_id) {
                Ok(Some(event)) => connection.send(event)?,
                Ok(None) => {
                    connection.send(envelope::Payload::MarketEventIdle(MarketEventIdle {
                        consumer_id: poll.consumer_id,
                    }))?;
                }
                Err(error) => send_market_fault(connection, error)?,
            }
        }
        _ => unreachable!("market payloads were filtered above"),
    }
    Ok(())
}

fn dispatch_provider_catalog_command(
    connection: &mut FramedConnection,
    market: &MarketService,
    attached_client: Option<u64>,
    payload: envelope::Payload,
) -> Result<(), String> {
    let Some(client_id) = attached_client else {
        send_market_fault(
            connection,
            "client must attach before using the provider catalog",
        )?;
        return Ok(());
    };
    let result = match payload {
        envelope::Payload::SearchProviderInstruments(search) => {
            market.search_provider_instruments(client_id, search)
        }
        envelope::Payload::SelectProviderInstrument(selection) => {
            market.select_provider_instrument(client_id, selection)
        }
        _ => unreachable!("provider catalog payloads were filtered above"),
    };
    if let Err(error) = result {
        send_market_fault(connection, error)?;
    }
    Ok(())
}

fn require_market(market: Option<&MarketService>) -> Result<&MarketService, String> {
    market.ok_or_else(|| "market service is unavailable in this engine session".to_string())
}

fn send_market_fault(
    connection: &mut FramedConnection,
    detail: impl Into<String>,
) -> Result<(), String> {
    connection.send(envelope::Payload::Fault(Fault {
        code: EngineFaultCode::Cancelled as i32,
        redacted_detail: detail.into(),
    }))
}

fn apply_selection(
    state: &EngineState,
    selection: SetSelection,
    connection: &mut FramedConnection,
) -> Result<(), String> {
    match state.apply_selection(selection) {
        Ok(workspace) => connection.send(envelope::Payload::WorkspaceState(workspace)),
        Err(error) if error == WORKSPACE_SHUTTING_DOWN => {
            connection.send(cancelled_mutation_fault(error))
        }
        Err(error) if error == "workspace revision is stale" => {
            connection.send(stale_workspace_fault())
        }
        Err(error) => Err(error),
    }
}

fn apply_watchlist(
    state: &EngineState,
    watchlist: SetWatchlist,
    connection: &mut FramedConnection,
) -> Result<(), String> {
    match state.apply_watchlist(watchlist) {
        Ok(workspace) => connection.send(envelope::Payload::WorkspaceState(workspace)),
        Err(error) if error == WORKSPACE_SHUTTING_DOWN => {
            connection.send(cancelled_mutation_fault(error))
        }
        Err(error) if error == "workspace revision is stale" => {
            connection.send(stale_workspace_fault())
        }
        Err(error) => Err(error),
    }
}

fn apply_viewport(
    state: &EngineState,
    viewport: SetViewport,
    connection: &mut FramedConnection,
) -> Result<(), String> {
    match state.apply_viewport(viewport) {
        Ok(workspace) => connection.send(envelope::Payload::WorkspaceState(workspace)),
        Err(error) if error == WORKSPACE_SHUTTING_DOWN => {
            connection.send(cancelled_mutation_fault(error))
        }
        Err(error) if error == "chart viewport selection is stale" => {
            connection.send(cancelled_mutation_fault(error))
        }
        Err(error) => Err(error),
    }
}

fn stale_workspace_fault() -> envelope::Payload {
    cancelled_mutation_fault("workspace revision is stale")
}

fn cancelled_mutation_fault(detail: impl Into<String>) -> envelope::Payload {
    envelope::Payload::Fault(Fault {
        code: EngineFaultCode::Cancelled as i32,
        redacted_detail: detail.into(),
    })
}

fn constant_time_equals(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant},
    };

    use interprocess::local_socket::traits::Listener as _;
    use sysinfo::{Pid, ProcessesToUpdate, System};

    use axiusflow_coinbase_market_adapter::ENTITLEMENT_CLASS;
    use axiusflow_local_engine_client::EngineClient;
    use axiusflow_local_engine_protocol::{
        InstallProviderInstrument, ResourceMode, SeriesCadence, SeriesKey, envelope,
    };
    use axiusflow_market_data::MarketBar;

    use super::{
        EngineShutdown, EngineState, MarketService, bind_listener, serve_client_with_market,
        serve_client_with_market_and_shutdown,
    };

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);
    const WARMUP_SAMPLES: usize = 32;
    const MEASURED_SAMPLES: usize = 128;
    const MULTI_CONSUMERS: u64 = 20;
    const MULTI_WARMUP_SAMPLES: usize = 8;
    const MULTI_MEASURED_SAMPLES: usize = 32;
    const BTC_INSTRUMENT: &str = "instrument:coinbase:btc:usd";
    const ETH_INSTRUMENT: &str = "instrument:coinbase:eth:usd";

    fn socket_name(label: &str) -> String {
        format!(
            "axiusflow-engine-{label}-test-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[derive(Clone, Copy)]
    struct Percentiles {
        p50: u128,
        p95: u128,
        p99: u128,
    }

    fn percentiles(mut samples: Vec<u128>) -> Percentiles {
        assert!(!samples.is_empty(), "performance sample set is not empty");
        samples.sort_unstable();
        let at = |percentile: usize| {
            let rank = samples.len().saturating_mul(percentile).div_ceil(100);
            samples[rank.saturating_sub(1)]
        };
        Percentiles {
            p50: at(50),
            p95: at(95),
            p99: at(99),
        }
    }

    fn measure(warmups: usize, measured: usize, mut operation: impl FnMut(usize)) -> Percentiles {
        let mut timings = Vec::with_capacity(measured);
        for sample in 0..warmups + measured {
            let started = Instant::now();
            operation(sample);
            if sample >= warmups {
                timings.push(started.elapsed().as_nanos());
            }
        }
        percentiles(timings)
    }

    fn process_memory(system: &mut System, pid: Pid) -> u64 {
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]));
        system
            .process(pid)
            .expect("benchmark process remains observable")
            .memory()
    }

    #[cfg(debug_assertions)]
    fn require_release_profile() {
        panic!("run this verifier with cargo test --release");
    }

    #[cfg(not(debug_assertions))]
    fn require_release_profile() {}

    fn cached_series(instrument_id: &str, cadence_value: u32) -> SeriesKey {
        SeriesKey {
            provider: "coinbase".to_string(),
            instrument_id: instrument_id.to_string(),
            cadence_value,
            definition_revision: 1,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
            cadence: SeriesCadence::FixedSeconds as i32,
        }
    }

    fn fixture_history() -> Vec<MarketBar> {
        (1_u64..=350)
            .map(|source_sequence| MarketBar {
                source_sequence,
                exchange_timestamp_seconds: 0,
                exchange_timestamp_unix_nanos: 0,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            })
            .collect()
    }

    fn poll_direct_snapshot(
        market: &MarketService,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &SeriesKey,
    ) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(envelope::Payload::SeriesSnapshot(snapshot)) = market
                .poll_event(client_id, consumer_id)
                .expect("direct market poll succeeds")
                && snapshot.generation == generation
            {
                assert_eq!(snapshot.consumer_id, consumer_id);
                assert_eq!(snapshot.series.as_ref(), Some(series));
                assert_eq!(snapshot.bars.len(), 350);
                return;
            }
            assert!(Instant::now() < deadline, "direct snapshot timed out");
            thread::yield_now();
        }
    }

    fn poll_ipc_snapshot(
        client: &mut EngineClient,
        consumer_id: u64,
        generation: u64,
        series: &SeriesKey,
    ) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(envelope::Payload::SeriesSnapshot(snapshot)) = client
                .poll_market_event(consumer_id)
                .expect("IPC market poll succeeds")
                && snapshot.generation == generation
            {
                assert_eq!(snapshot.consumer_id, consumer_id);
                assert_eq!(snapshot.series.as_ref(), Some(series));
                assert!(!snapshot.bars.is_empty(), "IPC snapshot is usable");
                return;
            }
            assert!(Instant::now() < deadline, "IPC snapshot timed out");
            thread::yield_now();
        }
    }

    fn measure_direct_demand(market: &MarketService, series: &SeriesKey) -> Percentiles {
        market.attach(1).expect("direct client attaches");
        market
            .register_consumer(1, 1, 1)
            .expect("direct consumer registers");
        market
            .set_demand(1, 1, 1, series)
            .expect("initial direct demand succeeds");
        poll_direct_snapshot(market, 1, 1, 1, series);
        measure(WARMUP_SAMPLES, MEASURED_SAMPLES, |sample| {
            let generation = u64::try_from(sample).expect("sample fits") + 2;
            market
                .set_demand(1, 1, generation, series)
                .expect("cached direct demand succeeds");
            poll_direct_snapshot(market, 1, 1, generation, series);
        })
    }

    fn prime_cached_switch_series(market: &MarketService) {
        market.attach(3).expect("switch prime client attaches");
        market
            .register_consumer(3, 1, 3)
            .expect("switch prime consumer registers");
        for (generation, series) in [
            cached_series(BTC_INSTRUMENT, 300),
            cached_series(ETH_INSTRUMENT, 60),
        ]
        .iter()
        .enumerate()
        {
            let generation = u64::try_from(generation).expect("prime generation fits") + 1;
            market
                .set_demand(3, 3, generation, series)
                .expect("switch series primes");
            poll_direct_snapshot(market, 3, 3, generation, series);
        }
        market.detach(3).expect("switch prime client detaches");
    }

    fn measure_ipc_demand(
        market: &MarketService,
        series: &SeriesKey,
    ) -> (Percentiles, Percentiles, Percentiles, Percentiles) {
        let performance_socket_name = socket_name("performance");
        let listener = bind_listener(&performance_socket_name).expect("bind performance endpoint");
        let token = [11_u8; 32];
        let server_market = market.clone();
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept performance client");
            serve_client_with_market(stream, &token, 1, &EngineState::default(), &server_market)
                .expect("serve performance client");
        });
        let mut client = EngineClient::connect(&performance_socket_name, &token)
            .expect("connect performance client");
        client.attach_client(2).expect("IPC client attaches");
        client
            .register_consumer(2, 1, 2)
            .expect("IPC consumer registers");
        client.restore_workspace().expect("IPC registration fence");
        let ipc = measure(WARMUP_SAMPLES, MEASURED_SAMPLES, |sample| {
            let generation = u64::try_from(sample).expect("sample fits") + 1;
            client
                .set_series_demand(2, generation, series.clone())
                .expect("cached IPC demand succeeds");
            poll_ipc_snapshot(&mut client, 2, generation, series);
        });
        let (timeframe, symbol) = measure_cached_switches(&mut client);

        for consumer_id in 100..100 + MULTI_CONSUMERS {
            client
                .register_consumer(2, 1, consumer_id)
                .expect("multi-consumer registration succeeds");
        }
        client
            .restore_workspace()
            .expect("multi-consumer registration fence");
        let multi = measure(MULTI_WARMUP_SAMPLES, MULTI_MEASURED_SAMPLES, |sample| {
            let generation = u64::try_from(sample).expect("sample fits") + 1;
            for consumer_id in 100..100 + MULTI_CONSUMERS {
                client
                    .set_series_demand(consumer_id, generation, series.clone())
                    .expect("multi-consumer demand succeeds");
            }
            for consumer_id in 100..100 + MULTI_CONSUMERS {
                poll_ipc_snapshot(&mut client, consumer_id, generation, series);
            }
        });
        drop(client);
        server.join().expect("join performance server");
        (ipc, timeframe, symbol, multi)
    }

    fn measure_cached_switches(client: &mut EngineClient) -> (Percentiles, Percentiles) {
        let btc_minute = cached_series(BTC_INSTRUMENT, 60);
        let btc_five_minute = cached_series(BTC_INSTRUMENT, 300);
        let eth_minute = cached_series(ETH_INSTRUMENT, 60);
        let measured_end = WARMUP_SAMPLES + MEASURED_SAMPLES;
        let mut generation = u64::try_from(measured_end).expect("sample count fits") + 1;
        let timeframe = measure(WARMUP_SAMPLES, MEASURED_SAMPLES, |sample| {
            let series = if sample % 2 == 0 {
                &btc_minute
            } else {
                &btc_five_minute
            };
            let current = generation + u64::try_from(sample).expect("sample fits");
            client
                .set_series_demand(2, current, series.clone())
                .expect("cached timeframe switch succeeds");
            poll_ipc_snapshot(client, 2, current, series);
        });
        generation += u64::try_from(measured_end).expect("sample count fits");
        let symbol = measure(WARMUP_SAMPLES, MEASURED_SAMPLES, |sample| {
            let series = if sample % 2 == 0 {
                &btc_minute
            } else {
                &eth_minute
            };
            let current = generation + u64::try_from(sample).expect("sample fits");
            client
                .set_series_demand(2, current, series.clone())
                .expect("cached symbol switch succeeds");
            poll_ipc_snapshot(client, 2, current, series);
        });
        (timeframe, symbol)
    }

    fn measure_ipc_attach(market: &MarketService) -> Percentiles {
        let attach_socket_name = socket_name("attach-performance");
        let attach_listener =
            bind_listener(&attach_socket_name).expect("bind attach performance endpoint");
        let token = [11_u8; 32];
        let attach_market = market.clone();
        let attach_samples = WARMUP_SAMPLES + MEASURED_SAMPLES;
        let attach_server = thread::spawn(move || {
            let state = EngineState::default();
            for _ in 0..attach_samples {
                let stream = attach_listener.accept().expect("accept attach client");
                serve_client_with_market(stream, &token, 1, &state, &attach_market)
                    .expect("serve attach client");
            }
        });
        let attach = measure(WARMUP_SAMPLES, MEASURED_SAMPLES, |sample| {
            let mut attached =
                EngineClient::connect(&attach_socket_name, &token).expect("connect attach client");
            attached
                .attach_client(u64::try_from(sample).expect("sample fits") + 10_000)
                .expect("attach command succeeds");
            attached
                .restore_workspace()
                .expect("attach synchronization fence");
            drop(attached);
        });
        attach_server.join().expect("join attach server");
        attach
    }

    #[test]
    #[ignore = "release-only local engine performance evidence"]
    fn release_cached_demand_ipc_and_multi_consumer_performance() {
        require_release_profile();
        let pid = sysinfo::get_current_pid().expect("benchmark process id is available");
        let mut system = System::new();
        let memory_baseline = process_memory(&mut system, pid);
        let market =
            MarketService::start_fixture(fixture_history()).expect("fixture market starts");
        let memory_engine_started = process_memory(&mut system, pid);
        let series = cached_series(BTC_INSTRUMENT, 60);
        prime_cached_switch_series(&market);
        let direct = measure_direct_demand(&market, &series);
        let memory_direct = process_memory(&mut system, pid);
        let (ipc, timeframe, symbol, multi) = measure_ipc_demand(&market, &series);
        let memory_ipc = process_memory(&mut system, pid);
        let attach = measure_ipc_attach(&market);
        let memory_current = process_memory(&mut system, pid);
        let memory_high_water = [
            memory_baseline,
            memory_engine_started,
            memory_direct,
            memory_ipc,
            memory_current,
        ]
        .into_iter()
        .max()
        .expect("memory sample set is not empty");

        println!(
            "AXIUSFLOW_ENGINE_PERFORMANCE schema=3 samples={} warmups={} bars=350 direct_demand_snapshot_p50_ns={} direct_demand_snapshot_p95_ns={} direct_demand_snapshot_p99_ns={} ipc_demand_snapshot_p50_ns={} ipc_demand_snapshot_p95_ns={} ipc_demand_snapshot_p99_ns={} ipc_timeframe_switch_p50_ns={} ipc_timeframe_switch_p95_ns={} ipc_timeframe_switch_p99_ns={} ipc_symbol_switch_p50_ns={} ipc_symbol_switch_p95_ns={} ipc_symbol_switch_p99_ns={} ipc_attach_restore_p50_ns={} ipc_attach_restore_p95_ns={} ipc_attach_restore_p99_ns={} multi_consumers={} multi_samples={} ipc_multi_batch_p50_ns={} ipc_multi_batch_p95_ns={} ipc_multi_batch_p99_ns={} ipc_multi_per_consumer_p50_ns={} process_memory_baseline_bytes={} process_memory_engine_started_bytes={} process_memory_direct_bytes={} process_memory_ipc_multi_bytes={} process_memory_current_bytes={} process_memory_sampled_high_water_bytes={} process_memory_sampled_growth_bytes={} process_memory_workload_growth_bytes={}",
            MEASURED_SAMPLES,
            WARMUP_SAMPLES,
            direct.p50,
            direct.p95,
            direct.p99,
            ipc.p50,
            ipc.p95,
            ipc.p99,
            timeframe.p50,
            timeframe.p95,
            timeframe.p99,
            symbol.p50,
            symbol.p95,
            symbol.p99,
            attach.p50,
            attach.p95,
            attach.p99,
            MULTI_CONSUMERS,
            MULTI_MEASURED_SAMPLES,
            multi.p50,
            multi.p95,
            multi.p99,
            multi.p50 / u128::from(MULTI_CONSUMERS),
            memory_baseline,
            memory_engine_started,
            memory_direct,
            memory_ipc,
            memory_current,
            memory_high_water,
            memory_high_water.saturating_sub(memory_baseline),
            memory_high_water.saturating_sub(memory_engine_started)
        );
        assert!(memory_baseline > 0, "process memory baseline is observable");
        assert!(
            memory_high_water >= memory_current,
            "sampled memory high-water contains the final sample"
        );
        assert_local_interaction_target("cached IPC demand-to-snapshot", ipc);
        assert_local_interaction_target("cached timeframe switch", timeframe);
        assert_local_interaction_target("cached symbol switch", symbol);
    }

    fn assert_local_interaction_target(label: &str, latency: Percentiles) {
        assert!(
            latency.p50 < 20_000_000,
            "{label} p50 exceeded 20 ms: {} ns",
            latency.p50
        );
        assert!(
            latency.p95 < 50_000_000,
            "{label} p95 exceeded 50 ms: {} ns",
            latency.p95
        );
    }

    #[test]
    fn authenticated_lifecycle_commands_update_mode_and_request_shutdown() {
        let socket_name = socket_name("lifecycle");
        let listener = bind_listener(&socket_name).expect("bind lifecycle endpoint");
        let token = [13_u8; 32];
        let state = EngineState::default();
        let server_state = state.clone();
        let shutdown = EngineShutdown::default();
        let server_shutdown = shutdown.clone();
        let market =
            MarketService::start_fixture(fixture_history()).expect("fixture market starts");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept lifecycle client");
            serve_client_with_market_and_shutdown(
                stream,
                &token,
                17,
                &server_state,
                &market,
                &server_shutdown,
            )
            .expect("serve lifecycle client");
        });

        let mut client = EngineClient::connect(&socket_name, &token).expect("connect lifecycle");
        let interactive = client
            .set_engine_resource_mode(ResourceMode::Interactive)
            .expect("set interactive resource mode");
        assert_eq!(interactive.resource_mode, ResourceMode::Interactive as i32);
        client.shutdown_engine().expect("request engine shutdown");
        server.join().expect("join lifecycle server");

        assert!(shutdown.is_requested());
        assert_eq!(
            state.workspace().resource_mode,
            ResourceMode::OfflineSuspended as i32
        );
    }

    #[test]
    fn authenticated_market_demand_crosses_ipc_and_returns_engine_snapshot() {
        let socket_name = socket_name("market");
        let listener = bind_listener(&socket_name).expect("bind market endpoint");
        let token = [7_u8; 32];
        let market = MarketService::start_fixture(vec![MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        }])
        .expect("fixture market starts");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept market client");
            serve_client_with_market(stream, &token, 9, &EngineState::default(), &market)
                .expect("serve market client");
        });
        let mut client =
            EngineClient::connect(&socket_name, &token).expect("connect market client");
        let installed = client
            .install_provider_instrument(InstallProviderInstrument {
                provider: "rithmic".to_string(),
                session_generation: 7,
                selection_generation: 9,
                instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
                provider_symbol: "MNQU6".to_string(),
                display_symbol: "MNQU6".to_string(),
                venue_id: "CME".to_string(),
                price_scale: 2,
                quantity_scale: 0,
                entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            })
            .expect("install provider instrument");
        assert_eq!(installed.session_generation, 7);
        assert_eq!(installed.selection_generation, 9);
        assert_eq!(installed.instrument_id, "instrument:rithmic:CME:MNQU6");
        client.attach_client(1).expect("attach client");
        client
            .register_consumer(1, 1, 1)
            .expect("register consumer");
        client
            .set_series_demand(
                1,
                1,
                SeriesKey {
                    provider: "rithmic".to_string(),
                    instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
                    cadence_value: 100,
                    definition_revision: 1,
                    entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
                    cadence: axiusflow_local_engine_protocol::SeriesCadence::Trades as i32,
                },
            )
            .expect("send demand");
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut snapshot_received = false;
        let mut ready_received = false;
        let mut order_book_received = false;
        while !snapshot_received || !ready_received || !order_book_received {
            if let Some(event) = client.poll_market_event(1).expect("poll market event") {
                match event {
                    envelope::Payload::SeriesSnapshot(snapshot) => {
                        snapshot_received = snapshot.bars.len() == 1
                            && snapshot.provider_generation == 7
                            && snapshot.price_scale == 2
                            && snapshot.quantity_scale == 0;
                    }
                    envelope::Payload::SeriesState(state) => {
                        ready_received = state.generation == 1
                            && state.state
                                == axiusflow_local_engine_protocol::SeriesLoadState::Ready as i32;
                    }
                    envelope::Payload::OrderBookSnapshot(snapshot) => {
                        order_book_received = snapshot.consumer_id == 1
                            && snapshot.generation == 1
                            && snapshot.provider_generation == 7
                            && snapshot.selection_generation == 9
                            && snapshot.state
                                == axiusflow_local_engine_protocol::OrderBookState::AwaitingSnapshot
                                    as i32
                            && snapshot.bids.is_empty()
                            && snapshot.asks.is_empty();
                    }
                    _ => {}
                }
            }
            assert!(
                Instant::now() < deadline,
                "market snapshot polling timed out"
            );
        }
        assert_eq!(
            client.poll_market_event(1).expect("poll market event"),
            None
        );
        drop(client);
        server.join().expect("join market server");
    }
}
