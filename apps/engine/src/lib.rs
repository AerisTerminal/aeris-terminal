//! Resident engine process boundary and authenticated local sessions.

mod market_service;

pub use market_service::MarketService;

use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_local_engine_protocol::{
    AttachClient, ClientHello, ClientKind, DetachClient, EngineFaultCode, EngineReady, Envelope,
    EnvelopeDecoder, Fault, Goodbye, HotSeries, MarketEventIdle, PROTOCOL_VERSION, PollMarketEvent,
    RegisterConsumer, RemoveConsumer, ResourceMode, RestoreWorkspace, SeriesDemand, SeriesKey,
    SetSelection, SetViewport, SetWatchlist, ViewportDemand, WorkspaceState, encode_envelope,
    envelope,
};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};
use zeroize::Zeroizing;

/// Stable per-user local socket name for protocol version five.
pub const ENGINE_SOCKET_NAME: &str = "axiusflow-engine-v5";
/// Exact entropy required for the installation credential.
pub const INSTALLATION_TOKEN_BYTES: usize = 32;

/// Maximum time allowed for a newly spawned engine to publish readiness.
pub const ENGINE_START_TIMEOUT: Duration = Duration::from_secs(3);

const ENGINE_VAULT_SERVICE: &str = "com.axiusflow.engine";
const ENGINE_TOKEN_KEY: &str = "local-ipc-token-v1";
const WORKSPACE_SCHEMA_REVISION: u32 = 1;
const CACHE_MANIFEST_REVISION: u32 = 1;
const MAXIMUM_HOT_SERIES: usize = 32;

/// Loads the installation credential from the native vault, creating it once.
///
/// # Errors
/// Returns an error when the native credential store is unavailable or malformed.
pub fn native_installation_token() -> Result<Zeroizing<Vec<u8>>, String> {
    let vault = NativeCredentialVault::new(ENGINE_VAULT_SERVICE).map_err(redacted_vault_error)?;
    load_or_create_installation_token(&vault)
}

/// Loads or creates the installation credential in a supplied credential vault.
///
/// # Errors
/// Returns an error when vault access fails or a stored credential has the wrong length.
pub fn load_or_create_installation_token<V>(vault: &V) -> Result<Zeroizing<Vec<u8>>, String>
where
    V: CredentialVault,
{
    if let Some(token) = vault.load(ENGINE_TOKEN_KEY).map_err(redacted_vault_error)? {
        if token.len() != INSTALLATION_TOKEN_BYTES {
            return Err("native engine credential has an invalid length".to_string());
        }
        return Ok(Zeroizing::new(token));
    }
    let mut token = Zeroizing::new(vec![0_u8; INSTALLATION_TOKEN_BYTES]);
    getrandom::fill(token.as_mut_slice()).map_err(|_| "system CSPRNG unavailable".to_string())?;
    vault
        .store(ENGINE_TOKEN_KEY, token.as_slice())
        .map_err(redacted_vault_error)?;
    Ok(token)
}

fn redacted_vault_error<E>(_error: E) -> String {
    "native engine credential store is unavailable".to_string()
}

/// Shared resident-engine state visible to authenticated clients.
#[derive(Clone)]
pub struct EngineState {
    workspace: Arc<Mutex<WorkspaceState>>,
    workspace_root: Option<Arc<PathBuf>>,
    selection_generation: Arc<AtomicU64>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            workspace: Arc::new(Mutex::new(default_workspace())),
            workspace_root: None,
            selection_generation: Arc::new(AtomicU64::new(0)),
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

    fn apply_selection(&self, selection: SetSelection) -> Result<WorkspaceState, String> {
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// Authenticated local-engine client connection.
pub struct EngineClient {
    connection: FramedConnection,
    ready: EngineReady,
}

struct EngineConnectionFailure {
    detail: String,
    endpoint_reached: bool,
}

impl EngineClient {
    /// Connects and authenticates against a named engine endpoint.
    ///
    /// # Errors
    /// Returns an error when connection, framing, authentication, or negotiation fails.
    pub fn connect(name: &str, installation_token: &[u8]) -> Result<Self, String> {
        Self::connect_with_reachability(name, installation_token).map_err(|failure| failure.detail)
    }

    fn connect_with_reachability(
        name: &str,
        installation_token: &[u8],
    ) -> Result<Self, EngineConnectionFailure> {
        let name =
            name.to_ns_name::<GenericNamespaced>()
                .map_err(|error| EngineConnectionFailure {
                    detail: error.to_string(),
                    endpoint_reached: false,
                })?;
        let stream = LocalSocketStream::connect(name).map_err(|error| EngineConnectionFailure {
            detail: error.to_string(),
            endpoint_reached: false,
        })?;
        let mut connection = FramedConnection::new(stream).map_err(reached_failure)?;
        connection
            .send(envelope::Payload::ClientHello(ClientHello {
                protocol_version: PROTOCOL_VERSION,
                installation_token: installation_token.to_vec(),
                client_kind: ClientKind::Ui as i32,
            }))
            .map_err(reached_failure)?;
        let ready = match connection.receive().map_err(reached_failure)? {
            envelope::Payload::EngineReady(ready) => ready,
            envelope::Payload::Fault(fault) => {
                return Err(EngineConnectionFailure {
                    detail: fault.redacted_detail,
                    endpoint_reached: true,
                });
            }
            _ => {
                return Err(EngineConnectionFailure {
                    detail: "engine did not complete readiness negotiation".to_string(),
                    endpoint_reached: true,
                });
            }
        };
        Ok(Self { connection, ready })
    }

    /// Returns the negotiated engine readiness state.
    #[must_use]
    pub const fn ready(&self) -> &EngineReady {
        &self.ready
    }

    /// Requests the engine-owned workspace state.
    ///
    /// # Errors
    /// Returns an error when the connection fails or the reply is unexpected.
    pub fn restore_workspace(&mut self) -> Result<WorkspaceState, String> {
        self.connection.send(envelope::Payload::RestoreWorkspace(
            RestoreWorkspace::default(),
        ))?;
        match self.connection.receive()? {
            envelope::Payload::WorkspaceState(workspace) => Ok(workspace),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected workspace reply".to_string()),
        }
    }

    /// Applies a revision-fenced market selection.
    ///
    /// # Errors
    /// Returns an error when the revision is stale, transport fails, or the reply is invalid.
    pub fn set_selection(
        &mut self,
        market: String,
        interval_seconds: u32,
        workspace_revision: u64,
        selection_generation: u64,
    ) -> Result<WorkspaceState, String> {
        self.set_provider_selection(
            String::new(),
            market,
            interval_seconds,
            workspace_revision,
            selection_generation,
        )
    }

    /// Applies a revision-fenced provider and market selection.
    ///
    /// # Errors
    /// Returns an error when the revision is stale, transport fails, or the reply is invalid.
    pub fn set_provider_selection(
        &mut self,
        provider: String,
        market: String,
        interval_seconds: u32,
        workspace_revision: u64,
        selection_generation: u64,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetSelection(SetSelection {
                market,
                interval_seconds,
                workspace_revision,
                selection_generation,
                provider,
            }))?;
        self.receive_workspace()
    }

    /// Persists the stable viewport for the active selection without changing its revision.
    ///
    /// # Errors
    /// Returns an error when the selection generation is stale or persistence fails.
    pub fn set_viewport(
        &mut self,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
        selection_generation: u64,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetViewport(SetViewport {
                start_unix_nanos,
                end_unix_nanos,
                selection_generation,
            }))?;
        self.receive_workspace()
    }

    fn receive_workspace(&mut self) -> Result<WorkspaceState, String> {
        match self.connection.receive()? {
            envelope::Payload::WorkspaceState(workspace) => Ok(workspace),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected workspace reply".to_string()),
        }
    }

    /// Attaches one stable desktop lifetime to engine-owned market state.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn attach_client(&mut self, client_id: u64) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::AttachClient(AttachClient { client_id }))
    }

    /// Registers one independently generated chart consumer.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn register_consumer(
        &mut self,
        client_id: u64,
        workspace_id: u64,
        consumer_id: u64,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::RegisterConsumer(RegisterConsumer {
                client_id,
                workspace_id,
                consumer_id,
            }))
    }

    /// Replaces one consumer's authoritative bar-series demand.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn set_series_demand(
        &mut self,
        consumer_id: u64,
        generation: u64,
        series: SeriesKey,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::SeriesDemand(SeriesDemand {
                consumer_id,
                generation,
                series: Some(series),
            }))
    }

    /// Updates the visible range for the exact current consumer generation.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn set_market_viewport(
        &mut self,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::ViewportDemand(ViewportDemand {
                consumer_id,
                generation,
                start_unix_nanos,
                end_unix_nanos,
            }))
    }

    /// Receives the next market response from the authenticated engine session.
    ///
    /// # Errors
    /// Returns an error for connection, framing, protocol-version, or payload failure.
    pub fn receive_market_event(&mut self) -> Result<envelope::Payload, String> {
        self.connection.receive()
    }

    /// Polls at most one bounded publication for an attached market consumer.
    ///
    /// # Errors
    /// Returns an error when the authenticated exchange cannot complete.
    pub fn poll_market_event(
        &mut self,
        consumer_id: u64,
    ) -> Result<Option<envelope::Payload>, String> {
        self.connection
            .send(envelope::Payload::PollMarketEvent(PollMarketEvent {
                consumer_id,
            }))?;
        match self.connection.receive()? {
            envelope::Payload::MarketEventIdle(idle) if idle.consumer_id == consumer_id => Ok(None),
            envelope::Payload::MarketEventIdle(_) => {
                Err("engine returned market idle for another consumer".to_string())
            }
            payload => Ok(Some(payload)),
        }
    }

    /// Removes one consumer without affecting shared engine state.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn remove_market_consumer(&mut self, consumer_id: u64) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::RemoveConsumer(RemoveConsumer {
                consumer_id,
            }))
    }

    /// Releases all market demand owned by one desktop lifetime.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn detach_client(&mut self, client_id: u64) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::DetachClient(DetachClient { client_id }))
    }
}

fn reached_failure(detail: String) -> EngineConnectionFailure {
    EngineConnectionFailure {
        detail,
        endpoint_reached: true,
    }
}

/// Resolves the engine executable installed beside the current desktop binary.
///
/// # Errors
/// Returns an error when the current executable path has no parent directory.
pub fn sibling_engine_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let parent = current
        .parent()
        .ok_or_else(|| "current executable has no installation directory".to_string())?;
    Ok(parent.join(format!("axiusflow_engine{}", std::env::consts::EXE_SUFFIX)))
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

/// Attaches to the resident engine or starts the installed sibling and retries readiness.
///
/// This function is blocking and must run away from the UI thread.
///
/// # Errors
/// Returns an error when credentials, process launch, or readiness negotiation fail.
pub fn connect_or_start_engine(engine_executable: &Path) -> Result<EngineClient, String> {
    let token = native_installation_token()?;
    connect_or_start_engine_named(
        ENGINE_SOCKET_NAME,
        engine_executable,
        token.as_slice(),
        ENGINE_START_TIMEOUT,
    )
}

fn connect_or_start_engine_named(
    socket_name: &str,
    engine_executable: &Path,
    installation_token: &[u8],
    start_timeout: Duration,
) -> Result<EngineClient, String> {
    let mut engine_started = false;
    let mut last_error =
        match EngineClient::connect_with_reachability(socket_name, installation_token) {
            Ok(client) => return Ok(client),
            Err(failure) => {
                if !failure.endpoint_reached {
                    start_engine_process(engine_executable)?;
                    engine_started = true;
                }
                failure.detail
            }
        };
    let deadline = Instant::now() + start_timeout;
    while Instant::now() < deadline {
        match EngineClient::connect_with_reachability(socket_name, installation_token) {
            Ok(client) => return Ok(client),
            Err(failure) => {
                if !failure.endpoint_reached && !engine_started {
                    start_engine_process(engine_executable)?;
                    engine_started = true;
                }
                last_error = failure.detail;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(last_error)
}

fn start_engine_process(engine_executable: &Path) -> Result<(), String> {
    let mut command = Command::new(engine_executable);
    configure_background_process(&mut command);
    command.spawn().map_err(|_| {
        "resident engine could not be started from the installation directory".to_string()
    })?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_background_process(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_background_process(_command: &mut Command) {}

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
    serve_client_with_services(stream, installation_token, engine_epoch, state, None)
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
    )
}

fn serve_client_with_services(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
    state: &EngineState,
    market: Option<&MarketService>,
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
    serve_authenticated_session(&mut connection, state, market)
}

fn serve_authenticated_session(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
) -> Result<(), String> {
    let mut attached_client = None;
    let result = serve_authenticated_messages(connection, state, market, &mut attached_client);
    if let (Some(market), Some(client_id)) = (market, attached_client) {
        let _ = market.detach(client_id);
    }
    result
}

fn serve_authenticated_messages(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    attached_client: &mut Option<u64>,
) -> Result<(), String> {
    loop {
        let payload = match connection.receive() {
            Ok(payload) => payload,
            Err(error) if error == "local engine connection closed" => return Ok(()),
            Err(error) => return Err(error),
        };
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
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use interprocess::local_socket::{GenericNamespaced, ToNsName as _, prelude::*};

    use axiusflow_local_engine_protocol::{SeriesKey, envelope};
    use axiusflow_market_data::MarketBar;

    use super::{
        EngineClient, EngineState, MarketService, bind_listener, connect_or_start_engine_named,
        serve_client_with_market,
    };

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

    fn socket_name(label: &str) -> String {
        format!(
            "axiusflow-engine-{label}-test-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn authenticated_market_demand_crosses_ipc_and_returns_engine_snapshot() {
        let socket_name = socket_name("market");
        let listener = bind_listener(&socket_name).expect("bind market endpoint");
        let token = [7_u8; 32];
        let market = MarketService::start_fixture(vec![MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
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
        client.attach_client(1).expect("attach client");
        client
            .register_consumer(1, 1, 1)
            .expect("register consumer");
        client
            .set_series_demand(
                1,
                1,
                SeriesKey {
                    provider: "coinbase".to_string(),
                    instrument_id: "instrument:coinbase:btc:usd".to_string(),
                    interval_seconds: 60,
                    definition_revision: 1,
                },
            )
            .expect("send demand");
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut snapshot_received = false;
        let mut ready_received = false;
        while !snapshot_received || !ready_received {
            if let Some(event) = client.poll_market_event(1).expect("poll market event") {
                match event {
                    envelope::Payload::SeriesSnapshot(snapshot) => {
                        snapshot_received = snapshot.bars.len() == 1
                            && snapshot.price_scale == 2
                            && snapshot.quantity_scale == 8;
                    }
                    envelope::Payload::SeriesState(state) => {
                        ready_received = state.generation == 1
                            && state.state
                                == axiusflow_local_engine_protocol::SeriesLoadState::Ready as i32;
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

    #[test]
    fn reached_endpoint_is_retried_without_spawning_another_engine() {
        let socket_name = socket_name("reached");
        let listener = bind_listener(&socket_name).expect("bind occupied endpoint");
        let accepting = Arc::new(AtomicBool::new(true));
        let server_accepting = Arc::clone(&accepting);
        let server = thread::spawn(move || {
            while server_accepting.load(Ordering::Acquire) {
                let stream = listener.accept().expect("accept probe connection");
                drop(stream);
            }
        });

        let timeout = Duration::from_millis(80);
        let started = Instant::now();
        let error = connect_or_start_engine_named(
            &socket_name,
            Path::new("engine-executable-that-does-not-exist"),
            &[7_u8; 32],
            timeout,
        )
        .err()
        .expect("closed occupied endpoint cannot become ready");

        assert!(started.elapsed() >= timeout);
        assert_ne!(
            error,
            "resident engine could not be started from the installation directory"
        );
        accepting.store(false, Ordering::Release);
        let name = socket_name
            .to_ns_name::<GenericNamespaced>()
            .expect("create socket name");
        drop(LocalSocketStream::connect(name).expect("wake accepting server"));
        server.join().expect("join accepting server");
    }
}
