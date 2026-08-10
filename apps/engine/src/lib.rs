//! Resident engine process boundary and authenticated local sessions.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{CoinbaseSpotProduct, coinbase_instrument_id};
use axiusflow_local_engine_protocol::{
    CatalogEntry, CatalogReassembler, ClientHello, ClientKind, EngineFaultCode, EngineHeartbeat,
    EngineReady, Envelope, EnvelopeDecoder, Fault, Goodbye, HotSeries, PROTOCOL_VERSION,
    ResourceMode, RestoreWorkspace, SetSelection, SetViewport, SetWatchlist, SubscribeView,
    ViewKind, WorkspaceState, encode_envelope, envelope, split_catalog,
};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};
use zeroize::Zeroizing;

/// Stable per-user local socket name for protocol version one.
pub const ENGINE_SOCKET_NAME: &str = "axiusflow-engine-v1";
/// Exact entropy required for the installation credential.
pub const INSTALLATION_TOKEN_BYTES: usize = 32;

/// Maximum time allowed for a newly spawned engine to publish readiness.
pub const ENGINE_START_TIMEOUT: Duration = Duration::from_secs(3);

const ENGINE_VAULT_SERVICE: &str = "com.axiusflow.engine";
const ENGINE_TOKEN_KEY: &str = "local-ipc-token-v1";
const PUBLICATION_SUBSCRIBER_CAPACITY: usize = 128;
const SUBSCRIBER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const WORKSPACE_SCHEMA_REVISION: u32 = 1;
const CACHE_MANIFEST_REVISION: u32 = 1;
const MAXIMUM_HOT_SERIES: usize = 32;
type SelectionCallback = Arc<dyn Fn(WorkspaceState) + Send + Sync>;
type ProviderCommandCallback = Arc<dyn Fn(envelope::Payload) + Send + Sync>;

/// Bounded latest-state fan-out shared by the engine runtime and authenticated clients.
#[derive(Clone, Default)]
pub struct EnginePublicationHub {
    inner: Arc<Mutex<PublicationHubState>>,
}

#[derive(Default)]
struct PublicationHubState {
    retained: BTreeMap<i32, Vec<envelope::Payload>>,
    subscribers: Vec<(ViewKind, SyncSender<envelope::Payload>)>,
}

impl EnginePublicationHub {
    /// Publishes a view update and retains the latest covering state for instant attachment.
    pub fn publish(&self, view: ViewKind, payload: &envelope::Payload) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retained.insert(view as i32, vec![payload.clone()]);
        state.subscribers.retain(|(subscribed_view, sender)| {
            if *subscribed_view != view && *subscribed_view != ViewKind::Session {
                return true;
            }
            match sender.try_send(payload.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Fans out an incremental update without replacing the retained covering state.
    pub fn publish_transient(&self, view: ViewKind, payload: &envelope::Payload) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.subscribers.retain(|(subscribed_view, sender)| {
            if *subscribed_view != view && *subscribed_view != ViewKind::Session {
                return true;
            }
            match sender.try_send(payload.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Atomically replaces and publishes a multi-frame covering state such as a catalog.
    pub fn publish_covering(&self, view: ViewKind, payloads: &[envelope::Payload]) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retained.insert(view as i32, payloads.to_owned());
        state.subscribers.retain(|(subscribed_view, sender)| {
            if *subscribed_view != view && *subscribed_view != ViewKind::Session {
                return true;
            }
            payloads
                .iter()
                .all(|payload| match sender.try_send(payload.clone()) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
                })
        });
    }

    fn subscribe(&self, view: ViewKind) -> Receiver<envelope::Payload> {
        let (sender, receiver) = mpsc::sync_channel(PUBLICATION_SUBSCRIBER_CAPACITY);
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if view == ViewKind::Session {
            for retained_view in [
                ViewKind::Catalog,
                ViewKind::Diagnostics,
                ViewKind::Chart,
                ViewKind::Dom,
            ] {
                if let Some(retained) = state.retained.get(&(retained_view as i32)) {
                    for payload in retained {
                        let _ = sender.try_send(payload.clone());
                    }
                }
            }
        } else if let Some(retained) = state.retained.get(&(view as i32)) {
            for payload in retained {
                let _ = sender.try_send(payload.clone());
            }
        }
        state.subscribers.push((view, sender));
        receiver
    }
}

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
    selection_callback: Arc<Mutex<Option<SelectionCallback>>>,
    provider_command_callback: Arc<Mutex<Option<ProviderCommandCallback>>>,
    selection_generation: Arc<AtomicU64>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            workspace: Arc::new(Mutex::new(default_workspace())),
            workspace_root: None,
            selection_callback: Arc::new(Mutex::new(None)),
            provider_command_callback: Arc::new(Mutex::new(None)),
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
            selection_callback: Arc::new(Mutex::new(None)),
            provider_command_callback: Arc::new(Mutex::new(None)),
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

    /// Installs the resident market runtime's nonblocking selection callback.
    pub fn set_selection_callback(&self, callback: SelectionCallback) {
        *self
            .selection_callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(callback);
    }

    /// Installs the resident provider runtime's nonblocking command callback.
    pub fn set_provider_command_callback(&self, callback: ProviderCommandCallback) {
        *self
            .provider_command_callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(callback);
    }

    fn dispatch_provider_command(&self, command: envelope::Payload) {
        if let Some(callback) = self
            .provider_command_callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            callback(command);
        }
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
        drop(workspace);
        if let Some(callback) = self
            .selection_callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            callback(candidate.clone());
        }
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

/// Loads the newest valid resident Coinbase catalog, quarantining corrupt revisions.
///
/// # Errors
/// Returns an error when the engine state directory cannot be inspected or quarantine fails.
pub fn load_coinbase_catalog(root: &Path) -> Result<Option<Vec<CoinbaseSpotProduct>>, String> {
    for (_, path) in catalog_files(root)? {
        match decode_catalog_file(&path) {
            Ok(products) => return Ok(Some(products)),
            Err(()) => quarantine_workspace_file(&path)?,
        }
    }
    Ok(None)
}

/// Persists a validated immutable Coinbase catalog revision and bounds old revisions.
///
/// # Errors
/// Returns an error when validation, encoding, or durable storage fails.
pub fn persist_coinbase_catalog(
    root: &Path,
    products: &[CoinbaseSpotProduct],
) -> Result<(), String> {
    let entries = products
        .iter()
        .map(catalog_entry)
        .collect::<Result<Vec<_>, _>>()?;
    if entries.is_empty() || entries.len() > 4_096 {
        return Err("Coinbase catalog is outside its item bound".to_string());
    }
    fs::create_dir_all(root).map_err(redacted_workspace_error)?;
    if load_coinbase_catalog(root)?.as_deref() == Some(products) {
        return Ok(());
    }
    let files = catalog_files(root)?;
    let revision = files
        .first()
        .map_or(1, |(revision, _)| revision.saturating_add(1));
    let path = root.join(format!("coinbase-catalog-{revision:020}.frame"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(redacted_workspace_error)?;
    for snapshot in split_catalog(entries, revision) {
        let frame = encode_envelope(&Envelope {
            protocol_version: PROTOCOL_VERSION,
            payload: Some(envelope::Payload::CatalogSnapshot(snapshot)),
        })
        .map_err(|_| "Coinbase catalog could not be encoded".to_string())?;
        file.write_all(&frame).map_err(redacted_workspace_error)?;
    }
    file.sync_all().map_err(redacted_workspace_error)?;
    for (_, stale) in catalog_files(root)?.into_iter().skip(2) {
        fs::remove_file(stale).map_err(redacted_workspace_error)?;
    }
    Ok(())
}

fn catalog_entry(product: &CoinbaseSpotProduct) -> Result<CatalogEntry, String> {
    validate_catalog_product(product)?;
    Ok(CatalogEntry {
        product_id: product.product_id.clone(),
        base_currency: product.base_currency.clone(),
        quote_currency: product.quote_currency.clone(),
        price_scale: u32::from(product.price_scale),
        quantity_scale: u32::from(product.quantity_scale),
    })
}

fn catalog_files(root: &Path) -> Result<Vec<(u64, PathBuf)>, String> {
    let mut files = fs::read_dir(root)
        .map_err(redacted_workspace_error)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let revision = name
                .strip_prefix("coinbase-catalog-")?
                .strip_suffix(".frame")?
                .parse::<u64>()
                .ok()?;
            Some((revision, entry.path()))
        })
        .collect::<Vec<_>>();
    files.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    Ok(files)
}

fn decode_catalog_file(path: &Path) -> Result<Vec<CoinbaseSpotProduct>, ()> {
    let bytes = fs::read(path).map_err(|_| ())?;
    let mut decoder = EnvelopeDecoder::try_new().map_err(|_| ())?;
    let envelopes = decoder.push(&bytes).map_err(|_| ())?;
    let mut reassembler = CatalogReassembler::new();
    let mut completed = None;
    for envelope in envelopes {
        let Some(envelope::Payload::CatalogSnapshot(snapshot)) = envelope.payload else {
            return Err(());
        };
        if let Some(entries) = reassembler.push(snapshot).map_err(|_| ())? {
            if completed.is_some() {
                return Err(());
            }
            completed = Some(entries);
        }
    }
    completed
        .ok_or(())?
        .into_iter()
        .map(catalog_product)
        .collect()
}

fn catalog_product(entry: CatalogEntry) -> Result<CoinbaseSpotProduct, ()> {
    let product = CoinbaseSpotProduct {
        instrument_id: coinbase_instrument_id(&entry.product_id).map_err(|_| ())?,
        display_symbol: format!("{}/{}", entry.base_currency, entry.quote_currency),
        product_id: entry.product_id,
        base_currency: entry.base_currency,
        quote_currency: entry.quote_currency,
        price_scale: u8::try_from(entry.price_scale).map_err(|_| ())?,
        quantity_scale: u8::try_from(entry.quantity_scale).map_err(|_| ())?,
    };
    validate_catalog_product(&product).map_err(|_| ())?;
    Ok(product)
}

fn validate_catalog_product(product: &CoinbaseSpotProduct) -> Result<(), String> {
    let expected = format!("{}-{}", product.base_currency, product.quote_currency);
    if product.product_id != expected
        || product.instrument_id
            != coinbase_instrument_id(&product.product_id)
                .map_err(|_| "Coinbase catalog contains an invalid product identity".to_string())?
        || product.price_scale > 18
        || product.quantity_scale > 18
    {
        return Err("Coinbase catalog contains an invalid product".to_string());
    }
    Ok(())
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

    /// Sends one validated Rithmic search intent to the resident provider owner.
    ///
    /// # Errors
    /// Returns an error when the authenticated command connection fails.
    pub fn search_rithmic(
        &mut self,
        search: axiusflow_local_engine_protocol::RithmicSearch,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::RithmicSearch(search))
    }

    /// Sends one Rithmic read-only selection to the resident provider owner.
    ///
    /// # Errors
    /// Returns an error when the authenticated command connection fails.
    pub fn select_rithmic(
        &mut self,
        selection: axiusflow_local_engine_protocol::RithmicSelect,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::RithmicSelect(selection))
    }

    /// Requests one Rithmic chart series from the resident provider owner.
    ///
    /// # Errors
    /// Returns an error when the authenticated command connection fails.
    pub fn request_rithmic_history(
        &mut self,
        request: axiusflow_local_engine_protocol::RithmicHistory,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::RithmicHistory(request))
    }

    /// Converts this authenticated connection into a blocking view stream.
    ///
    /// Use a separate client connection for commands so live publications can never
    /// head-of-line block workspace mutations.
    ///
    /// # Errors
    /// Returns an error when the subscription request cannot be sent.
    pub fn subscribe_view(mut self, view: ViewKind) -> Result<EngineViewStream, String> {
        self.connection
            .send(envelope::Payload::SubscribeView(SubscribeView {
                view: view as i32,
            }))?;
        Ok(EngineViewStream {
            connection: self.connection,
        })
    }

    fn receive_workspace(&mut self) -> Result<WorkspaceState, String> {
        match self.connection.receive()? {
            envelope::Payload::WorkspaceState(workspace) => Ok(workspace),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected workspace reply".to_string()),
        }
    }
}

fn reached_failure(detail: String) -> EngineConnectionFailure {
    EngineConnectionFailure {
        detail,
        endpoint_reached: true,
    }
}

/// Blocking authenticated stream for one bounded engine view.
pub struct EngineViewStream {
    connection: FramedConnection,
}

impl EngineViewStream {
    /// Receives the next publication for the subscribed view.
    ///
    /// # Errors
    /// Returns an error when the engine disconnects or sends an invalid frame.
    pub fn receive(&mut self) -> Result<envelope::Payload, String> {
        self.connection.receive()
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
    serve_authenticated_session(&mut connection, state)
}

/// Serves one client with workspace commands and bounded view subscriptions.
///
/// # Errors
/// Returns an error for authentication, transport, framing, or malformed requests.
pub fn serve_client_with_publications(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
    state: &EngineState,
    publications: &EnginePublicationHub,
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
    serve_authenticated_session_with_publications(
        &mut connection,
        engine_epoch,
        state,
        publications,
    )
}

fn serve_authenticated_session_with_publications(
    connection: &mut FramedConnection,
    engine_epoch: u64,
    state: &EngineState,
    publications: &EnginePublicationHub,
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
            command @ (envelope::Payload::RithmicSearch(_)
            | envelope::Payload::RithmicSelect(_)
            | envelope::Payload::RithmicHistory(_)) => {
                state.dispatch_provider_command(command);
            }
            envelope::Payload::SubscribeView(subscription) => {
                let view = ViewKind::try_from(subscription.view)
                    .map_err(|_| "engine view subscription is invalid".to_string())?;
                let receiver = publications.subscribe(view);
                loop {
                    match receiver.recv_timeout(SUBSCRIBER_HEARTBEAT_INTERVAL) {
                        Ok(publication) => connection.send(publication)?,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            connection.send(envelope::Payload::EngineHeartbeat(
                                EngineHeartbeat { engine_epoch },
                            ))?;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    }
                }
            }
            envelope::Payload::Goodbye(_) => {
                connection.send(envelope::Payload::Goodbye(Goodbye {
                    reason: "client session closed".to_string(),
                }))?;
                return Ok(());
            }
            _ => connection.send(envelope::Payload::Fault(Fault {
                code: EngineFaultCode::MalformedMessage as i32,
                redacted_detail: "message is invalid in the current engine state".to_string(),
            }))?,
        }
    }
}

fn serve_authenticated_session(
    connection: &mut FramedConnection,
    state: &EngineState,
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
            _ => connection.send(envelope::Payload::Fault(Fault {
                code: EngineFaultCode::MalformedMessage as i32,
                redacted_detail: "message is invalid in the current engine state".to_string(),
            }))?,
        }
    }
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

    use super::{bind_listener, connect_or_start_engine_named};

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn reached_endpoint_is_retried_without_spawning_another_engine() {
        let socket_name = format!(
            "axiusflow-engine-reached-test-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
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
