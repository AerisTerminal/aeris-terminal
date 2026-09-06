//! Resident engine process boundary and authenticated local sessions.

mod account_market_gate;
pub mod account_service;
mod hyperliquid_history;
mod hyperliquid_realtime;
mod market_service;
mod rithmic_history;
mod rithmic_realtime;
mod workspace_layout;

pub use account_market_gate::start_account_market_gate;
pub use account_service::{AccountService, AccountServiceConfig};
pub use market_service::{MarketService, MarketServiceStatus};

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_engine_protocol::{
    ClientHello, ClientKind, ConsumerResourceClass as IpcConsumerResourceClass, EngineFaultCode,
    EngineLifetimeMode, EngineReady, EngineShutdownState, EngineStatus, Envelope, EnvelopeDecoder,
    Fault, Goodbye, HotSeries, InstallProviderInstrument, LIFECYCLE_CONTRACT_REVISION,
    MAX_FRAME_BYTES, PROTOCOL_VERSION, ProviderInstrumentInstalled, RegisterConsumer,
    RemoveConsumer, ResourceMode, SeriesCadence, SeriesDemand, SeriesKey, SetEngineLifecycle,
    SetSelection, SetViewport, SetWatchlist, SetWorkspaceLayout, StreamRole, ViewportDemand,
    VisibilityDemand, WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState, encode_envelope, envelope,
};
use axiusflow_local_engine_client::INSTALLATION_TOKEN_BYTES;
use axiusflow_market_data::{BarPeriod, BarSeriesKey};
use axiusflow_market_engine::{HotSetDescriptor, HotSetEntry, HotSetManager, WorkspaceId};
use axiusflow_platform_runtime::{BackgroundService, current_release_identity};
use axiusflow_rithmic_protocol_adapter::RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};
const WORKSPACE_SCHEMA_REVISION: u32 = 5;
const CACHE_MANIFEST_REVISION: u32 = 1;
const MAXIMUM_HOT_SERIES: usize = 32;
const WORKSPACE_SHUTTING_DOWN: &str = "engine workspace is shutting down";
const IPC_OUTBOX_CAPACITY: usize = 256;
const IPC_WRITE_BATCH_MESSAGES: usize = 64;
const IPC_WRITE_BATCH_BYTES: usize = 4 * 1_048_576;

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

    /// Requests process shutdown from an authenticated or native lifecycle owner.
    pub fn request(&self) {
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
    background_service: Arc<Mutex<Option<BackgroundService>>>,
    hot_set: Arc<Mutex<HotSetState>>,
    account: AccountService,
}

#[derive(Clone)]
struct HotConsumer {
    client_id: u64,
    workspace_id: WorkspaceId,
    series: Option<BarSeriesKey>,
}

#[derive(Clone)]
struct InstalledInstrument {
    account_id: String,
    provider_symbol: String,
    venue_id: String,
    display_symbol: String,
    price_scale: u8,
    quantity_scale: u8,
    entitlement_id: String,
}

struct HotSetState {
    manager: HotSetManager,
    consumers: BTreeMap<u64, HotConsumer>,
    instruments: BTreeMap<(String, String), InstalledInstrument>,
}

impl Default for EngineState {
    fn default() -> Self {
        let workspace = default_workspace();
        Self {
            hot_set: Arc::new(Mutex::new(
                hot_set_state(&workspace).unwrap_or_else(|_| empty_hot_set_state()),
            )),
            workspace: Arc::new(Mutex::new(workspace)),
            workspace_root: None,
            selection_generation: Arc::new(AtomicU64::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            background_service: Arc::new(Mutex::new(None)),
            account: AccountService::new(AccountServiceConfig::from_environment()),
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
        let hot_set = hot_set_state(&workspace)?;
        #[cfg(test)]
        let account = AccountService::new(AccountServiceConfig::from_environment());
        #[cfg(not(test))]
        let account = AccountService::new_restoring(AccountServiceConfig::from_environment());
        let state = Self {
            workspace: Arc::new(Mutex::new(workspace)),
            workspace_root: Some(Arc::new(workspace_root)),
            selection_generation: Arc::new(AtomicU64::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            background_service: Arc::new(Mutex::new(None)),
            hot_set: Arc::new(Mutex::new(hot_set)),
            // Startup may restore only through an online refresh grant whose
            // ID token and canonical link are reverified. Cached lease state
            // alone never authenticates or starts market work.
            account,
        };
        if state.workspace().workspace_revision == 0 || migrated {
            state.persist(&state.workspace())?;
        }
        Ok(state)
    }

    /// Returns the engine-owned account session shared by all desktop windows.
    #[must_use]
    pub fn account(&self) -> &AccountService {
        &self.account
    }

    /// Returns a consistent copy of the current workspace state.
    #[must_use]
    pub fn workspace(&self) -> WorkspaceState {
        self.workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Installs the native process/autostart boundary owned by the engine process shell.
    pub fn install_background_service(&self, service: BackgroundService) {
        *self
            .background_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(service);
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

    fn record_consumer(&self, client_id: u64, workspace_id: u64, consumer_id: u64) {
        let Some(workspace_id) = NonZeroU64::new(workspace_id).map(WorkspaceId) else {
            return;
        };
        self.hot_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .consumers
            .insert(
                consumer_id,
                HotConsumer {
                    client_id,
                    workspace_id,
                    series: None,
                },
            );
    }

    fn record_consumer_removal(&self, consumer_id: u64) {
        self.hot_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .consumers
            .remove(&consumer_id);
    }

    fn record_client_detach(&self, client_id: u64) {
        self.hot_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .consumers
            .retain(|_, consumer| consumer.client_id != client_id);
    }

    fn record_installed_instrument(&self, instrument: &InstallProviderInstrument) {
        let account_id = if instrument.provider == "hyperliquid" {
            crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ACCOUNT_ID
        } else if instrument.provider == "rithmic" {
            RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID
        } else {
            return;
        };
        let Ok(price_scale) = u8::try_from(instrument.price_scale) else {
            return;
        };
        let Ok(quantity_scale) = u8::try_from(instrument.quantity_scale) else {
            return;
        };
        self.hot_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .instruments
            .insert(
                (
                    instrument.provider.clone(),
                    instrument.instrument_id.clone(),
                ),
                InstalledInstrument {
                    account_id: account_id.to_string(),
                    provider_symbol: instrument.provider_symbol.clone(),
                    venue_id: instrument.venue_id.clone(),
                    display_symbol: instrument.display_symbol.clone(),
                    price_scale,
                    quantity_scale,
                    entitlement_id: instrument.entitlement_id.clone(),
                },
            );
    }

    fn record_series_demand(&self, consumer_id: u64, series: &SeriesKey) -> Result<(), String> {
        let canonical = canonical_series(series)?;
        let mut hot = self
            .hot_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let consumer = hot
            .consumers
            .get(&consumer_id)
            .cloned()
            .ok_or_else(|| "hot-set consumer is unavailable".to_string())?;
        let instrument = hot
            .instruments
            .get(&(series.provider.clone(), series.instrument_id.clone()))
            .cloned()
            .ok_or_else(|| "hot-set instrument metadata is unavailable".to_string())?;
        if instrument.entitlement_id != series.entitlement_id {
            return Err("hot-set instrument entitlement is stale".to_string());
        }
        let descriptor = HotSetDescriptor {
            series: canonical.clone(),
            account_id: instrument.account_id,
            provider_symbol: instrument.provider_symbol,
            venue_id: instrument.venue_id,
            display_symbol: instrument.display_symbol,
            price_scale: instrument.price_scale,
            quantity_scale: instrument.quantity_scale,
        };
        let previous = hot.manager.clone();
        hot.manager
            .touch(consumer.workspace_id, descriptor, unix_seconds())
            .map_err(|error| error.to_string())?;
        if let Some(current) = hot.consumers.get_mut(&consumer_id) {
            current.series = Some(canonical);
        }
        if let Err(error) = self.persist_hot_set_manager(&hot.manager) {
            hot.manager = previous;
            return Err(error);
        }
        Ok(())
    }

    fn record_series_viewport(
        &self,
        consumer_id: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        let mut hot = self
            .hot_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let series = hot
            .consumers
            .get(&consumer_id)
            .and_then(|consumer| consumer.series.clone())
            .ok_or_else(|| "hot-set consumer has no current series".to_string())?;
        let previous = hot.manager.clone();
        hot.manager
            .set_viewport(&series, start_unix_nanos, end_unix_nanos)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.persist_hot_set_manager(&hot.manager) {
            hot.manager = previous;
            return Err(error);
        }
        Ok(())
    }

    fn record_published_market_event(&self, payload: &envelope::Payload) -> Result<(), String> {
        if let envelope::Payload::ProviderInstrumentSelection(selection) = payload {
            if let Some(instrument) = selection.instrument.as_ref() {
                self.record_installed_instrument(instrument);
            }
            return Ok(());
        }
        let envelope::Payload::SeriesSnapshot(snapshot) = payload else {
            return Ok(());
        };
        let Some(series) = snapshot.series.as_ref() else {
            return Ok(());
        };
        let Some(first) = snapshot.bars.first() else {
            return Ok(());
        };
        let Some(last) = snapshot.bars.last() else {
            return Ok(());
        };
        let canonical = canonical_series(series)?;
        let end = last
            .exchange_timestamp_unix_nanos
            .checked_add(1)
            .ok_or_else(|| "hot-set coverage end overflowed".to_string())?;
        let mut hot = self
            .hot_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = hot.manager.clone();
        hot.manager
            .update_coverage(
                &canonical,
                (first.exchange_timestamp_unix_nanos, end),
                snapshot.provider_generation,
                snapshot.publication_generation,
            )
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.persist_hot_set_manager(&hot.manager) {
            hot.manager = previous;
            return Err(error);
        }
        Ok(())
    }

    fn persist_hot_set_manager(&self, manager: &HotSetManager) -> Result<(), String> {
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut candidate = workspace.clone();
        candidate.hot_series = manager
            .ranked()
            .into_iter()
            .map(hot_entry_to_protocol)
            .collect();
        candidate.cache_manifest_revision = candidate.cache_manifest_revision.saturating_add(1);
        validate_workspace(&candidate)?;
        if let Some(root) = &self.workspace_root {
            persist_hot_set(root, &candidate)?;
        }
        *workspace = candidate;
        Ok(())
    }

    fn apply_lifecycle_preferences(
        &self,
        command: SetEngineLifecycle,
    ) -> Result<WorkspaceState, String> {
        let lifetime_mode = EngineLifetimeMode::try_from(command.lifetime_mode)
            .map_err(|_| "engine lifetime mode is invalid".to_string())?;
        if lifetime_mode == EngineLifetimeMode::KeepMarketsLive && !command.markets_live_permitted {
            return Err("markets-live mode requires explicit permission".to_string());
        }
        self.ensure_mutable()?;
        let current = self.workspace();
        if command.workspace_revision != current.workspace_revision {
            return Err("workspace revision is stale".to_string());
        }
        let mut candidate = current.clone();
        candidate.lifetime_mode = lifetime_mode as i32;
        candidate.warm_mode_enabled = lifetime_mode != EngineLifetimeMode::ExitCompletely;
        candidate.autostart_enabled = command.autostart_enabled;
        candidate.markets_live_permitted = command.markets_live_permitted;
        candidate.workspace_revision = candidate.workspace_revision.saturating_add(1);
        validate_workspace(&candidate)?;

        let autostart_changed = current.autostart_enabled != candidate.autostart_enabled;
        let service = autostart_changed
            .then(|| {
                self.background_service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .ok_or_else(|| {
                        "engine background service integration is unavailable".to_string()
                    })
            })
            .transpose()?;
        if let Some(service) = &service {
            service
                .set_autostart(candidate.autostart_enabled)
                .map_err(|error| error.to_string())?;
        }

        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if workspace.workspace_revision != current.workspace_revision {
            if let Some(service) = &service {
                let _ = service.set_autostart(current.autostart_enabled);
            }
            return Err("workspace revision is stale".to_string());
        }
        if let Err(error) = self.persist(&candidate) {
            if let Some(service) = &service {
                let _ = service.set_autostart(current.autostart_enabled);
            }
            return Err(error);
        }
        *workspace = candidate.clone();
        Ok(candidate)
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

    /// Fences persistent workspace mutation for process shutdown.
    pub fn begin_shutdown(&self) {
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

    fn apply_workspace_layout(&self, layout: SetWorkspaceLayout) -> Result<WorkspaceState, String> {
        let mut workspace = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_mutable()?;
        if layout.workspace_revision != workspace.workspace_revision {
            return Err("workspace revision is stale".to_string());
        }
        if layout.layout_generation <= workspace.layout_generation {
            return Err("workspace layout generation is stale".to_string());
        }
        let mut candidate = workspace.clone();
        candidate.layout_generation = layout.layout_generation;
        candidate.active_workspace_id = layout.active_workspace_id;
        candidate.workspace_tabs = layout.workspace_tabs;
        candidate.workspace_revision = candidate.workspace_revision.saturating_add(1);
        sync_layout_hot_series(&mut candidate);
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
    let primary = hyperliquid_hot_series(1, unix_seconds());
    WorkspaceState {
        provider: "hyperliquid".to_string(),
        market: "BTC-PERP".to_string(),
        interval_seconds: 60,
        watchlist: vec!["BTC-PERP".to_string()],
        workspace_revision: 0,
        warm_mode_enabled: true,
        resource_mode: ResourceMode::Warm as i32,
        schema_revision: WORKSPACE_SCHEMA_REVISION,
        cache_manifest_revision: CACHE_MANIFEST_REVISION,
        hot_series: vec![primary.clone()],
        lifetime_mode: EngineLifetimeMode::KeepEngineWarm as i32,
        autostart_enabled: false,
        markets_live_permitted: false,
        layout_generation: 1,
        active_workspace_id: 1,
        workspace_tabs: vec![default_workspace_tab(&primary)],
    }
}

fn default_workspace_tab(series: &HotSeries) -> WorkspaceTabState {
    WorkspaceTabState {
        workspace_id: 1,
        label: "Workspace 1".to_string(),
        split_axis: WorkspaceSplitAxis::Horizontal as i32,
        panes: vec![WorkspacePaneState {
            pane_id: 1,
            consumer_id: 1,
            kind: WorkspacePaneKind::Chart as i32,
            instrument: Some(InstallProviderInstrument {
                provider: series.provider.clone(),
                session_generation: series.provider_watermark.max(1),
                selection_generation: 1,
                instrument_id: series.instrument_id.clone(),
                provider_symbol: series.provider_symbol.clone(),
                display_symbol: series.display_symbol.clone(),
                venue_id: series.venue_id.clone(),
                price_scale: series.price_scale,
                quantity_scale: series.quantity_scale,
                entitlement_id: series.entitlement_id.clone(),
            }),
            series: Some(SeriesKey {
                provider: series.provider.clone(),
                instrument_id: series.instrument_id.clone(),
                cadence_value: series.cadence_value,
                definition_revision: series.definition_revision,
                entitlement_id: series.entitlement_id.clone(),
                cadence: series.cadence,
            }),
            viewport_start_unix_nanos: series.viewport_start_unix_nanos,
            viewport_end_unix_nanos: series.viewport_end_unix_nanos,
            size_basis_points: 10_000,
            generation: 1,
        }],
        active_pane_id: 1,
        generation: 1,
        layout: Some(WorkspaceLayoutState {
            pane_id: 1,
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            ratio_basis_points: 0,
            first: None,
            second: None,
        }),
    }
}

fn validate_workspace(workspace: &WorkspaceState) -> Result<(), String> {
    const MAXIMUM_MARKET_BYTES: usize = 128;
    const MAXIMUM_WATCHLIST_ITEMS: usize = 256;
    let mut hot_identities = BTreeSet::new();
    let mut workspace_ids = BTreeSet::new();
    let mut pane_ids = BTreeSet::new();
    let mut consumer_ids = BTreeSet::new();

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
                || !valid_hot_identity(workspace.schema_revision, series, &mut hot_identities)
                || !valid_viewport(series)
                || !valid_coverage(series)
                || workspace.schema_revision >= 3 && !valid_reconstructable_hot_series(series)
        })
    {
        return Err("workspace state is invalid".to_string());
    }
    if !matches!(workspace.schema_revision, 0..=WORKSPACE_SCHEMA_REVISION)
        || workspace.cache_manifest_revision == u32::MAX
    {
        return Err("workspace revision is unsupported".to_string());
    }
    if ResourceMode::try_from(workspace.resource_mode).is_err() {
        return Err("workspace resource mode is invalid".to_string());
    }
    let lifetime_mode = EngineLifetimeMode::try_from(workspace.lifetime_mode)
        .map_err(|_| "workspace lifetime mode is invalid".to_string())?;
    if lifetime_mode == EngineLifetimeMode::KeepMarketsLive && !workspace.markets_live_permitted {
        return Err("markets-live mode requires explicit permission".to_string());
    }
    if workspace.schema_revision >= 4
        && (workspace.layout_generation == 0
            || workspace.workspace_tabs.is_empty()
            || workspace.workspace_tabs.len() > 8
            || workspace.active_workspace_id == 0
            || workspace.workspace_tabs.iter().any(|tab| {
                tab.workspace_id == 0
                    || !workspace_ids.insert(tab.workspace_id)
                    || tab.label.trim().is_empty()
                    || tab.label.len() > 128
                    || WorkspaceSplitAxis::try_from(tab.split_axis).is_err()
                    || tab.panes.is_empty()
                    || tab.panes.len() > 4
                    || tab.active_pane_id == 0
                    || tab.generation == 0
                    || workspace.schema_revision >= 5
                        && !workspace_layout::layout_matches_panes(tab)
                    || tab
                        .panes
                        .iter()
                        .map(|pane| pane.size_basis_points)
                        .sum::<u32>()
                        != 10_000
                    || !tab
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == tab.active_pane_id)
                    || tab.panes.iter().any(|pane| {
                        pane.pane_id == 0
                            || pane.consumer_id == 0
                            || !pane_ids.insert(pane.pane_id)
                            || !consumer_ids.insert(pane.consumer_id)
                            || pane.generation == 0
                            || pane.size_basis_points == 0
                            || WorkspacePaneKind::try_from(pane.kind).is_err()
                            || !valid_workspace_pane(pane)
                    })
            })
            || !workspace_ids.contains(&workspace.active_workspace_id))
    {
        return Err("workspace layout is invalid".to_string());
    }
    Ok(())
}

fn valid_workspace_pane(pane: &WorkspacePaneState) -> bool {
    let viewport_valid = match (pane.viewport_start_unix_nanos, pane.viewport_end_unix_nanos) {
        (None, None) => true,
        (Some(start), Some(end)) => start < end,
        _ => false,
    };
    let Some(instrument) = pane.instrument.as_ref() else {
        return false;
    };
    let Some(series) = pane.series.as_ref() else {
        return false;
    };
    viewport_valid
        && !instrument.provider.trim().is_empty()
        && instrument.provider == series.provider
        && instrument.instrument_id == series.instrument_id
        && instrument.entitlement_id == series.entitlement_id
        && SeriesCadence::try_from(series.cadence)
            .is_ok_and(|cadence| cadence != SeriesCadence::Unspecified)
        && series.cadence_value > 0
        && series.definition_revision > 0
        && instrument.price_scale <= 18
        && instrument.quantity_scale <= 18
}

fn sync_layout_hot_series(workspace: &mut WorkspaceState) {
    for hot in &mut workspace.hot_series {
        hot.workspace_ids.clear();
    }
    let mut next_score = workspace
        .hot_series
        .iter()
        .map(|series| series.score)
        .max()
        .unwrap_or(0);
    for tab in &workspace.workspace_tabs {
        for pane in &tab.panes {
            let (Some(instrument), Some(series)) = (&pane.instrument, &pane.series) else {
                continue;
            };
            if let Some(hot) = workspace.hot_series.iter_mut().find(|hot| {
                hot.provider == series.provider
                    && hot.instrument_id == series.instrument_id
                    && hot.entitlement_id == series.entitlement_id
                    && hot.cadence == series.cadence
                    && hot.cadence_value == series.cadence_value
                    && hot.definition_revision == series.definition_revision
            }) {
                if !hot.workspace_ids.contains(&tab.workspace_id) {
                    hot.workspace_ids.push(tab.workspace_id);
                }
                continue;
            }
            next_score = next_score.saturating_add(1).max(1);
            workspace.hot_series.push(HotSeries {
                provider: series.provider.clone(),
                market: instrument.provider_symbol.clone(),
                interval_seconds: series.cadence_value,
                score: next_score,
                last_used_unix_seconds: unix_seconds(),
                provider_watermark: instrument.session_generation,
                series_watermark: 0,
                viewport_start_unix_nanos: pane.viewport_start_unix_nanos,
                viewport_end_unix_nanos: pane.viewport_end_unix_nanos,
                account_id: provider_account_id(&series.provider).to_string(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                cadence: series.cadence,
                cadence_value: series.cadence_value,
                definition_revision: series.definition_revision,
                pinned: false,
                workspace_ids: vec![tab.workspace_id],
                coverage_start_unix_nanos: None,
                coverage_end_unix_nanos: None,
                provider_symbol: instrument.provider_symbol.clone(),
                venue_id: instrument.venue_id.clone(),
                display_symbol: instrument.display_symbol.clone(),
                price_scale: instrument.price_scale,
                quantity_scale: instrument.quantity_scale,
            });
        }
    }
    workspace.hot_series.sort_unstable_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.score.cmp(&left.score))
    });
    workspace.hot_series.truncate(MAXIMUM_HOT_SERIES);
}
fn provider_account_id(provider: &str) -> &str {
    if provider == "rithmic" {
        RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID
    } else if provider == "hyperliquid" {
        crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ACCOUNT_ID
    } else {
        "provider-account-unavailable"
    }
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

fn valid_coverage(series: &HotSeries) -> bool {
    match (
        series.coverage_start_unix_nanos,
        series.coverage_end_unix_nanos,
    ) {
        (None, None) => true,
        (Some(start), Some(end)) => start < end,
        _ => false,
    }
}

fn valid_hot_identity(
    schema_revision: u32,
    series: &HotSeries,
    identities: &mut BTreeSet<(String, String, String, i32, u32, u32)>,
) -> bool {
    if schema_revision < 3 {
        return identities.insert((
            series.provider.clone(),
            series.market.clone(),
            String::new(),
            SeriesCadence::FixedSeconds as i32,
            series.interval_seconds,
            0,
        ));
    }
    identities.insert((
        series.provider.clone(),
        series.instrument_id.clone(),
        series.entitlement_id.clone(),
        series.cadence,
        series.cadence_value,
        series.definition_revision,
    ))
}

fn valid_reconstructable_hot_series(series: &HotSeries) -> bool {
    !series.account_id.trim().is_empty()
        && !series.instrument_id.trim().is_empty()
        && !series.entitlement_id.trim().is_empty()
        && !series.provider_symbol.trim().is_empty()
        && !series.venue_id.trim().is_empty()
        && !series.display_symbol.trim().is_empty()
        && SeriesCadence::try_from(series.cadence)
            .is_ok_and(|cadence| cadence != SeriesCadence::Unspecified && series.cadence_value > 0)
        && series.definition_revision > 0
        && !series.workspace_ids.is_empty()
        && series.workspace_ids.len() <= 256
        && series
            .workspace_ids
            .iter()
            .all(|workspace_id| *workspace_id > 0)
        && series.price_scale <= 18
        && series.quantity_scale <= 18
}
fn migrate_workspace(workspace: &mut WorkspaceState) -> bool {
    // Both live providers survive migration untouched; anything else was
    // never resolvable and restarts from the Hyperliquid default.
    if workspace.provider != "rithmic" && workspace.provider != "hyperliquid"
        || workspace
            .hot_series
            .iter()
            .any(|series| series.provider != "rithmic" && series.provider != "hyperliquid")
    {
        *workspace = default_workspace();
        return true;
    }
    let mut migrated = false;
    if workspace.schema_revision < 2 {
        workspace.lifetime_mode = if workspace.warm_mode_enabled {
            EngineLifetimeMode::KeepEngineWarm as i32
        } else {
            EngineLifetimeMode::ExitCompletely as i32
        };
        workspace.autostart_enabled = false;
        workspace.markets_live_permitted = false;
        migrated = true;
    }
    if workspace.schema_revision < 4 {
        let primary = workspace.hot_series.first().cloned().unwrap_or_else(|| {
            pending_hot_series(
                &workspace.provider,
                &workspace.market,
                workspace.interval_seconds,
            )
        });
        workspace.layout_generation = 1;
        workspace.active_workspace_id = 1;
        workspace.workspace_tabs = vec![default_workspace_tab(&primary)];
        migrated = true;
    }
    if workspace.schema_revision < 5 {
        workspace_layout::add_native_layouts(&mut workspace.workspace_tabs);
        migrated = true;
    }
    workspace.schema_revision = WORKSPACE_SCHEMA_REVISION;
    if workspace.cache_manifest_revision == 0 {
        workspace.cache_manifest_revision = CACHE_MANIFEST_REVISION;
        migrated = true;
    }
    if workspace.hot_series.is_empty() {
        workspace.hot_series.push(pending_hot_series(
            &workspace.provider,
            &workspace.market,
            workspace.interval_seconds,
        ));
        migrated = true;
    }
    migrated
}

/// Builds one unresolved hot-series entry for a freshly selected market.
///
/// The entry carries the selection identity with pending instrument metadata;
/// catalog resolution replaces it with the installed instrument before any
/// demand is accepted.
fn pending_hot_series(provider: &str, market: &str, interval_seconds: u32) -> HotSeries {
    if provider == "hyperliquid" {
        hyperliquid_hot_series(1, unix_seconds())
    } else {
        rithmic_hot_series(market, interval_seconds, 1, unix_seconds())
    }
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
    } else if workspace.provider == "rithmic" {
        workspace.hot_series.push(rithmic_hot_series(
            &workspace.market,
            workspace.interval_seconds,
            next_score,
            now,
        ));
    } else if workspace.provider == "hyperliquid" {
        workspace
            .hot_series
            .push(hyperliquid_hot_series(next_score, now));
    }
    workspace.hot_series.sort_unstable_by(|left, right| {
        right
            .last_used_unix_seconds
            .cmp(&left.last_used_unix_seconds)
            .then_with(|| right.score.cmp(&left.score))
    });
    workspace.hot_series.truncate(MAXIMUM_HOT_SERIES);
}
fn rithmic_hot_series(
    market: &str,
    interval_seconds: u32,
    score: u32,
    last_used_unix_seconds: u64,
) -> HotSeries {
    let symbol = market.trim().to_ascii_uppercase();
    HotSeries {
        provider: "rithmic".to_string(),
        market: symbol.clone(),
        interval_seconds,
        score,
        last_used_unix_seconds,
        provider_watermark: 0,
        series_watermark: 0,
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID.to_string(),
        instrument_id: format!("rithmic-pending:{symbol}"),
        entitlement_id: format!("rithmic-test:pending:{symbol}"),
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: interval_seconds,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: None,
        coverage_end_unix_nanos: None,
        provider_symbol: symbol.clone(),
        venue_id: "CME".to_string(),
        display_symbol: symbol,
        price_scale: 2,
        quantity_scale: 0,
    }
}

/// Pending fresh-install entry: Hyperliquid BTC perpetual, one-minute
/// candles, resolved against the public catalog before demand is accepted.
fn hyperliquid_hot_series(score: u32, last_used_unix_seconds: u64) -> HotSeries {
    HotSeries {
        provider: "hyperliquid".to_string(),
        market: "BTC-PERP".to_string(),
        interval_seconds: 60,
        score,
        last_used_unix_seconds,
        provider_watermark: 0,
        series_watermark: 0,
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        account_id: crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ACCOUNT_ID.to_string(),
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        entitlement_id: crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ENTITLEMENT_ID.to_string(),
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: 60,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: None,
        coverage_end_unix_nanos: None,
        provider_symbol: "BTC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        display_symbol: "BTC-PERP".to_string(),
        price_scale: 8,
        quantity_scale: 8,
    }
}

fn empty_hot_set_state() -> HotSetState {
    HotSetState {
        manager: HotSetManager::new(
            NonZeroUsize::new(MAXIMUM_HOT_SERIES).unwrap_or(NonZeroUsize::MIN),
        ),
        consumers: BTreeMap::new(),
        instruments: BTreeMap::new(),
    }
}

fn hot_set_state(workspace: &WorkspaceState) -> Result<HotSetState, String> {
    let mut state = empty_hot_set_state();
    let entries = workspace
        .hot_series
        .iter()
        .map(protocol_hot_entry)
        .collect::<Result<Vec<_>, _>>()?;
    state
        .manager
        .restore(entries)
        .map_err(|error| error.to_string())?;
    for series in &workspace.hot_series {
        let price_scale = u8::try_from(series.price_scale)
            .map_err(|_| "hot-set price precision is invalid".to_string())?;
        let quantity_scale = u8::try_from(series.quantity_scale)
            .map_err(|_| "hot-set quantity precision is invalid".to_string())?;
        state.instruments.insert(
            (series.provider.clone(), series.instrument_id.clone()),
            InstalledInstrument {
                account_id: series.account_id.clone(),
                provider_symbol: series.provider_symbol.clone(),
                venue_id: series.venue_id.clone(),
                display_symbol: series.display_symbol.clone(),
                price_scale,
                quantity_scale,
                entitlement_id: series.entitlement_id.clone(),
            },
        );
    }
    Ok(state)
}

fn protocol_hot_entry(series: &HotSeries) -> Result<HotSetEntry, String> {
    let canonical = canonical_series(&SeriesKey {
        provider: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        cadence_value: series.cadence_value,
        definition_revision: series.definition_revision,
        entitlement_id: series.entitlement_id.clone(),
        cadence: series.cadence,
    })?;
    let workspaces = series
        .workspace_ids
        .iter()
        .map(|workspace_id| {
            NonZeroU64::new(*workspace_id)
                .map(WorkspaceId)
                .ok_or_else(|| "hot-set workspace identity is invalid".to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let price_scale = u8::try_from(series.price_scale)
        .map_err(|_| "hot-set price precision is invalid".to_string())?;
    let quantity_scale = u8::try_from(series.quantity_scale)
        .map_err(|_| "hot-set quantity precision is invalid".to_string())?;
    Ok(HotSetEntry {
        descriptor: HotSetDescriptor {
            series: canonical,
            account_id: series.account_id.clone(),
            provider_symbol: series.provider_symbol.clone(),
            venue_id: series.venue_id.clone(),
            display_symbol: series.display_symbol.clone(),
            price_scale,
            quantity_scale,
        },
        workspaces,
        pinned: series.pinned,
        score: u64::from(series.score),
        last_used_unix_seconds: series.last_used_unix_seconds,
        provider_watermark: series.provider_watermark,
        series_watermark: series.series_watermark,
        viewport: series
            .viewport_start_unix_nanos
            .zip(series.viewport_end_unix_nanos),
        coverage: series
            .coverage_start_unix_nanos
            .zip(series.coverage_end_unix_nanos),
    })
}

fn hot_entry_to_protocol(entry: HotSetEntry) -> HotSeries {
    let (cadence, cadence_value) = protocol_period(entry.descriptor.series.period);
    HotSeries {
        provider: entry.descriptor.series.provider_id,
        market: entry.descriptor.display_symbol.clone(),
        interval_seconds: cadence_value,
        score: u32::try_from(entry.score).unwrap_or(u32::MAX),
        last_used_unix_seconds: entry.last_used_unix_seconds,
        provider_watermark: entry.provider_watermark,
        series_watermark: entry.series_watermark,
        viewport_start_unix_nanos: entry.viewport.map(|viewport| viewport.0),
        viewport_end_unix_nanos: entry.viewport.map(|viewport| viewport.1),
        account_id: entry.descriptor.account_id,
        instrument_id: entry.descriptor.series.instrument_id,
        entitlement_id: entry.descriptor.series.entitlement_id,
        cadence: cadence as i32,
        cadence_value,
        definition_revision: entry.descriptor.series.definition_version,
        pinned: entry.pinned,
        workspace_ids: entry
            .workspaces
            .into_iter()
            .map(|workspace| workspace.0.get())
            .collect(),
        coverage_start_unix_nanos: entry.coverage.map(|coverage| coverage.0),
        coverage_end_unix_nanos: entry.coverage.map(|coverage| coverage.1),
        provider_symbol: entry.descriptor.provider_symbol,
        venue_id: entry.descriptor.venue_id,
        display_symbol: entry.descriptor.display_symbol,
        price_scale: u32::from(entry.descriptor.price_scale),
        quantity_scale: u32::from(entry.descriptor.quantity_scale),
    }
}

fn canonical_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    if series.provider.trim().is_empty()
        || series.instrument_id.trim().is_empty()
        || series.entitlement_id.trim().is_empty()
        || series.definition_revision == 0
    {
        return Err("hot-set series identity is invalid".to_string());
    }
    let period = match SeriesCadence::try_from(series.cadence)
        .map_err(|_| "hot-set series cadence is invalid".to_string())?
    {
        SeriesCadence::FixedSeconds => BarPeriod::time(series.cadence_value),
        SeriesCadence::Trades => BarPeriod::tick(series.cadence_value),
        SeriesCadence::SessionDays => BarPeriod::session(series.cadence_value),
        SeriesCadence::CalendarWeeks => BarPeriod::week(series.cadence_value),
        SeriesCadence::CalendarMonths => BarPeriod::month(series.cadence_value),
        SeriesCadence::Unspecified => {
            Err(axiusflow_market_data::MarketDataValidationError::InvalidPeriod)
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: series.entitlement_id.clone(),
        period,
        definition_version: series.definition_revision,
    })
}

const fn protocol_period(period: BarPeriod) -> (SeriesCadence, u32) {
    match period {
        BarPeriod::Time { seconds } => (SeriesCadence::FixedSeconds, seconds),
        BarPeriod::Tick { trades } => (SeriesCadence::Trades, trades),
        BarPeriod::Session { days } => (SeriesCadence::SessionDays, days),
        BarPeriod::Week { weeks } => (SeriesCadence::CalendarWeeks, weeks),
        BarPeriod::Month { months } => (SeriesCadence::CalendarMonths, months),
    }
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
        target_consumer_id: 0,
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
        target_consumer_id: 0,
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
    axiusflow_platform_runtime::native_data_root()
        .map(|root| root.join("engine"))
        .map_err(|error| error.to_string())
}

/// One fully paired client session: a write-only command stream is never
/// read and a read-only event stream is never written.
///
/// Neither stream is ever split: each transport handle has exactly one owner
/// and one direction, so a blocking read can never stall a concurrent write
/// on any platform.
pub struct SessionStreams {
    /// Client-to-engine commands, owned by the serving thread.
    pub command: LocalSocketStream,
    /// Engine-to-client replies and pushed events, owned by the writer thread.
    pub event: LocalSocketStream,
}

struct PendingStream {
    stream: LocalSocketStream,
    first_seen: Instant,
}

struct PendingPair {
    command: Option<PendingStream>,
    event: Option<PendingStream>,
}

/// Pairs the two authenticated streams of each client session by nonce.
///
/// Every accepted connection carries one hello; the first arrival pends until
/// its sibling arrives. The map stays bounded by entry count and age.
#[derive(Clone, Default)]
pub struct SessionPairer {
    pending: Arc<Mutex<BTreeMap<u64, PendingPair>>>,
}

/// Maximum half-open sessions retained while waiting for a sibling stream.
const MAX_PENDING_PAIRS: usize = 16;
/// Maximum time one half-open session waits for its sibling stream.
const MAX_PENDING_AGE: Duration = Duration::from_secs(30);
/// Maximum time one accepted connection may take to send its hello. A live
/// local client answers in milliseconds; expiry drops slow-loris connections
/// before they can hold a session thread.
const SESSION_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

impl SessionPairer {
    /// Creates an empty session pairing registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Authenticates one accepted connection and pairs it by session nonce.
    ///
    /// Returns the completed pair when this arrival completes a session, or
    /// `None` when this half pends for its sibling. Authentication and
    /// pairing faults are reported to the offending stream before returning
    /// an error.
    ///
    /// # Errors
    /// Returns an error for I/O, framing, authentication, pairing, or
    /// malformed-message failures.
    pub fn accept_one(
        &self,
        mut stream: LocalSocketStream,
        installation_token: &[u8],
    ) -> Result<Option<SessionStreams>, String> {
        if installation_token.len() != INSTALLATION_TOKEN_BYTES {
            return Err("installation credential has an invalid length".to_string());
        }
        // The hello poll never shares this handle: pairing has not seen it
        // yet, and it returns to blocking before the session threads own it.
        stream
            .set_nonblocking(true)
            .map_err(|_| "ipc_receive failed: local transport is unavailable".to_string())?;
        let hello = read_session_hello(&mut stream)?;
        stream
            .set_nonblocking(false)
            .map_err(|_| "ipc_receive failed: local transport is unavailable".to_string())?;
        if ClientKind::try_from(hello.client_kind).is_err() {
            reject_session_stream(stream);
            return Err("client kind is invalid".to_string());
        }
        if !matches!(
            StreamRole::try_from(hello.stream_role),
            Ok(StreamRole::Command | StreamRole::Event)
        ) {
            reject_session_stream(stream);
            return Err("stream role is invalid".to_string());
        }
        let role = hello.stream_role;
        if hello.session_nonce == 0 {
            reject_session_stream(stream);
            return Err("session nonce is invalid".to_string());
        }
        let release = current_release_identity();
        if hello.release_identity != release.release_identity
            || hello.install_generation != release.install_generation
        {
            send_session_fault(
                &mut stream,
                EngineFaultCode::VersionMismatch,
                "desktop and resident engine release identities do not match",
            )?;
            return Err("session release identity does not match".to_string());
        }
        if !constant_time_equals(&hello.installation_token, installation_token) {
            send_session_fault(
                &mut stream,
                EngineFaultCode::Unauthenticated,
                "local engine authentication failed",
            )?;
            return Err("session authentication failed".to_string());
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reap_stale_pairs(&mut pending);
        if pending.len() >= MAX_PENDING_PAIRS {
            evict_oldest_pair(&mut pending);
        }
        let entry = pending
            .entry(hello.session_nonce)
            .or_insert_with(|| PendingPair {
                command: None,
                event: None,
            });
        let slot = if role == StreamRole::Command as i32 {
            &mut entry.command
        } else {
            &mut entry.event
        };
        if slot.is_some() {
            reject_session_stream(stream);
            return Err("duplicate session stream".to_string());
        }
        *slot = Some(PendingStream {
            stream,
            first_seen: Instant::now(),
        });
        let complete = entry.command.is_some() && entry.event.is_some();
        if !complete {
            return Ok(None);
        }
        let Some(pair) = pending.remove(&hello.session_nonce) else {
            return Err("paired session vanished during pairing".to_string());
        };
        let Some(command) = pair.command else {
            return Err("paired session has no command stream".to_string());
        };
        let Some(event) = pair.event else {
            return Err("paired session has no event stream".to_string());
        };
        // Session streams are blocking from here on: the command stream is
        // read by exactly one serving thread and the event stream is written
        // by exactly one writer thread, so neither handle is ever shared.
        let pair = SessionStreams {
            command: command.stream,
            event: event.stream,
        };
        pair.command
            .set_nonblocking(false)
            .map_err(|_| "ipc_receive failed: local transport is unavailable".to_string())?;
        pair.event
            .set_nonblocking(false)
            .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())?;
        Ok(Some(pair))
    }
}

fn reap_stale_pairs(pending: &mut BTreeMap<u64, PendingPair>) {
    let now = Instant::now();
    pending.retain(|_, pair| {
        let newest = pair
            .command
            .as_ref()
            .map(|stream| stream.first_seen)
            .into_iter()
            .chain(pair.event.as_ref().map(|stream| stream.first_seen))
            .max();
        newest.is_some_and(|seen| now.duration_since(seen) < MAX_PENDING_AGE)
    });
}

fn evict_oldest_pair(pending: &mut BTreeMap<u64, PendingPair>) {
    let oldest = pending
        .iter()
        .min_by_key(|(_, pair)| {
            pair.command
                .as_ref()
                .map(|stream| stream.first_seen)
                .into_iter()
                .chain(pair.event.as_ref().map(|stream| stream.first_seen))
                .min()
        })
        .map(|(nonce, _)| *nonce);
    if let Some(nonce) = oldest {
        pending.remove(&nonce);
    }
}

fn read_session_hello(stream: &mut LocalSocketStream) -> Result<ClientHello, String> {
    let deadline = Instant::now()
        .checked_add(SESSION_HELLO_TIMEOUT)
        .ok_or_else(|| "ipc_receive failed: local engine connection closed".to_string())?;
    let mut decoder = EnvelopeDecoder::try_new().map_err(|error| error.to_string())?;
    loop {
        let mut chunk = [0_u8; 16 * 1024];
        match stream.read(&mut chunk) {
            Ok(0) => {}
            Ok(count) => {
                let mut envelopes = decoder
                    .push(&chunk[..count])
                    .map_err(|_| "ipc_receive failed: local message is invalid".to_string())?;
                if let Some(envelope) = envelopes.pop() {
                    return match envelope.payload {
                        Some(envelope::Payload::ClientHello(hello)) => Ok(hello),
                        _ => Err("client hello must be the first engine message".to_string()),
                    };
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err("ipc_receive failed: local engine connection closed".to_string());
            }
        }
        if Instant::now() >= deadline {
            return Err("ipc_receive failed: local engine connection closed".to_string());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn send_session_fault(
    stream: &mut LocalSocketStream,
    code: EngineFaultCode,
    redacted_detail: &str,
) -> Result<(), String> {
    let frame = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::Fault(Fault {
            code: code as i32,
            redacted_detail: redacted_detail.to_string(),
        })),
    })
    .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
    // Faulted streams never join a session, so restoring blocking mode here
    // keeps the fault write reliable without sharing the handle.
    stream
        .set_nonblocking(false)
        .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())?;
    stream
        .write_all(&frame)
        .and_then(|()| stream.flush())
        .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())
}

fn reject_session_stream(stream: LocalSocketStream) {
    drop(stream);
}

struct FramedConnection {
    command: LocalSocketStream,
    outgoing: Option<SyncSender<(u64, envelope::Payload)>>,
    writer: Option<thread::JoinHandle<()>>,
    decoder: EnvelopeDecoder,
    pending: VecDeque<Envelope>,
}

impl FramedConnection {
    fn new(pair: SessionStreams, state: EngineState) -> Result<Self, String> {
        let (outgoing, messages) = mpsc::sync_channel(IPC_OUTBOX_CAPACITY);
        let writer = thread::Builder::new()
            .name("axiusflow-engine-ipc-writer".to_string())
            .spawn(move || write_ipc_messages(pair.event, &messages, &state))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            command: pair.command,
            outgoing: Some(outgoing),
            writer: Some(writer),
            decoder: EnvelopeDecoder::try_new().map_err(|error| error.to_string())?,
            pending: VecDeque::new(),
        })
    }

    fn send_ready(&mut self, ready: envelope::Payload) -> Result<(), String> {
        let frame = encode_envelope(&Envelope {
            protocol_version: PROTOCOL_VERSION,
            target_consumer_id: 0,
            payload: Some(ready),
        })
        .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
        self.command
            .write_all(&frame)
            .and_then(|()| self.command.flush())
            .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())
    }

    fn send(&mut self, payload: envelope::Payload) -> Result<(), String> {
        self.outgoing
            .as_ref()
            .ok_or_else(|| "ipc_send failed: local writer is stopping".to_string())?
            .send((0, payload))
            .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())
    }

    fn event_sender(&self) -> Result<SyncSender<(u64, envelope::Payload)>, String> {
        self.outgoing
            .clone()
            .ok_or_else(|| "ipc_send failed: local writer is stopping".to_string())
    }

    fn receive(&mut self) -> Result<envelope::Payload, String> {
        loop {
            if let Some(envelope) = self.pending.pop_front() {
                return envelope.payload.ok_or_else(|| {
                    "ipc_receive failed: engine message has no payload".to_string()
                });
            }
            let mut chunk = [0_u8; 16 * 1024];
            let count = self
                .command
                .read(&mut chunk)
                .map_err(|error| match error.kind() {
                    io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::NotConnected => {
                        "ipc_receive failed: local engine connection closed".to_string()
                    }
                    _ => "ipc_receive failed: local transport is unavailable".to_string(),
                })?;
            if count == 0 {
                return Err("ipc_receive failed: local engine connection closed".to_string());
            }
            self.pending.extend(
                self.decoder
                    .push(&chunk[..count])
                    .map_err(|_| "ipc_receive failed: local message is invalid".to_string())?,
            );
        }
    }
}

impl Drop for FramedConnection {
    fn drop(&mut self) {
        self.outgoing.take();
        if let Some(writer) = self.writer.take()
            && writer.join().is_err()
        {
            eprintln!("Axiusflow engine IPC writer stopped unexpectedly");
        }
    }
}

fn write_ipc_messages(
    mut writer: LocalSocketStream,
    messages: &Receiver<(u64, envelope::Payload)>,
    state: &EngineState,
) {
    while let Ok(first) = messages.recv() {
        let mut pending = VecDeque::from([first]);
        for _ in 1..IPC_WRITE_BATCH_MESSAGES {
            match messages.try_recv() {
                Ok(message) => pending.push_back(message),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let mut bytes = Vec::new();
        while let Some((target_consumer_id, payload)) = pending.pop_front() {
            if target_consumer_id != 0
                && let Err(error) = state.record_published_market_event(&payload)
            {
                eprintln!("Axiusflow engine hot-set coverage update degraded: {error}");
            }
            let Ok(frame) = encode_envelope(&Envelope {
                protocol_version: PROTOCOL_VERSION,
                target_consumer_id,
                payload: Some(payload),
            }) else {
                return;
            };
            if !bytes.is_empty() && bytes.len().saturating_add(frame.len()) > IPC_WRITE_BATCH_BYTES
            {
                if writer.write_all(&bytes).is_err() {
                    return;
                }
                bytes.clear();
            }
            if frame.len() > MAX_FRAME_BYTES.saturating_add(4) {
                return;
            }
            bytes.extend_from_slice(&frame);
        }
        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
            return;
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

/// Handles one paired client session against isolated default state.
/// Intended for probes and tests.
///
/// The pair must already be authenticated (see [`SessionPairer`]).
///
/// # Errors
/// Returns an error for I/O, framing, or malformed-message failures.
pub fn serve_client(pair: SessionStreams, engine_epoch: u64) -> Result<(), String> {
    serve_client_with_state(pair, engine_epoch, &EngineState::default())
}

/// Serves one paired client session against shared resident-engine state.
///
/// The pair must already be authenticated (see [`SessionPairer`]).
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_state(
    pair: SessionStreams,
    engine_epoch: u64,
    state: &EngineState,
) -> Result<(), String> {
    serve_client_with_services(pair, engine_epoch, state, None, None, false)
}

/// Serves one paired client session with workspace and resident market ownership.
///
/// The pair must already be authenticated (see [`SessionPairer`]).
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_market(
    pair: SessionStreams,
    engine_epoch: u64,
    state: &EngineState,
    market: &MarketService,
) -> Result<(), String> {
    serve_client_with_services(pair, engine_epoch, state, Some(market), None, false)
}

/// Serves one paired client session with market ownership and process shutdown control.
///
/// The pair must already be authenticated (see [`SessionPairer`]).
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_market_and_shutdown(
    pair: SessionStreams,
    engine_epoch: u64,
    state: &EngineState,
    market: &MarketService,
    shutdown: &EngineShutdown,
) -> Result<(), String> {
    serve_client_with_services(
        pair,
        engine_epoch,
        state,
        Some(market),
        Some(shutdown),
        false,
    )
}

/// Serves the production desktop boundary with mandatory cloud-account
/// authentication in addition to the native installation credential.
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_account_gate(
    pair: SessionStreams,
    engine_epoch: u64,
    state: &EngineState,
    market: &MarketService,
    shutdown: &EngineShutdown,
) -> Result<(), String> {
    serve_client_with_services(
        pair,
        engine_epoch,
        state,
        Some(market),
        Some(shutdown),
        true,
    )
}

fn serve_client_with_services(
    pair: SessionStreams,
    engine_epoch: u64,
    state: &EngineState,
    market: Option<&MarketService>,
    shutdown: Option<&EngineShutdown>,
    account_gate_required: bool,
) -> Result<(), String> {
    let mut connection = FramedConnection::new(pair, state.clone())?;
    let release = current_release_identity();
    // Readiness is published on the command stream only; the event stream
    // carries replies and pushed market events.
    connection.send_ready(envelope::Payload::EngineReady(EngineReady {
        protocol_version: PROTOCOL_VERSION,
        engine_epoch,
        workspace_revision: state.workspace().workspace_revision,
        lifecycle_contract_revision: LIFECYCLE_CONTRACT_REVISION,
        release_identity: release.release_identity,
        install_generation: release.install_generation,
    }))?;
    serve_authenticated_session(
        &mut connection,
        state,
        market,
        shutdown,
        account_gate_required,
    )
}

fn serve_authenticated_session(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    shutdown: Option<&EngineShutdown>,
    account_gate_required: bool,
) -> Result<(), String> {
    let mut attached_client = None;
    let result = serve_authenticated_messages(
        connection,
        state,
        market,
        shutdown,
        account_gate_required,
        &mut attached_client,
    );
    if let (Some(market), Some(client_id)) = (market, attached_client) {
        let _ = market.detach(client_id);
        state.record_client_detach(client_id);
    }
    result
}

fn serve_authenticated_messages(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    shutdown: Option<&EngineShutdown>,
    account_gate_required: bool,
    attached_client: &mut Option<u64>,
) -> Result<(), String> {
    loop {
        let payload = match connection.receive() {
            Ok(payload) => payload,
            Err(error) if error == "ipc_receive failed: local engine connection closed" => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if shutdown.is_some_and(EngineShutdown::is_requested) {
            connection.send(cancelled_mutation_fault("engine is shutting down"))?;
            return Ok(());
        }
        if account_gate_required
            && platform_access_requires_account(&payload)
            && !state.account().is_authenticated()
        {
            connection.send(account_required_fault())?;
            continue;
        }
        match payload {
            payload @ (envelope::Payload::RestoreWorkspace(_)
            | envelope::Payload::SetSelection(_)
            | envelope::Payload::SetWatchlist(_)
            | envelope::Payload::SetViewport(_)
            | envelope::Payload::SetWorkspaceLayout(_)) => {
                handle_workspace_message(connection, state, payload)?;
            }
            payload @ (envelope::Payload::SetEngineResourceMode(_)
            | envelope::Payload::SetEngineLifecycle(_)
            | envelope::Payload::GetEngineStatus(_)
            | envelope::Payload::ShutdownEngine(_)
            | envelope::Payload::InstallProviderInstrument(_)) => {
                if handle_engine_control_message(connection, state, market, shutdown, payload)? {
                    return Ok(());
                }
            }
            payload @ (envelope::Payload::BeginLogin(_)
            | envelope::Payload::CancelLogin(_)
            | envelope::Payload::GetAccountStatus(_)
            | envelope::Payload::SignOut(_)) => {
                handle_account_message(connection, state, market, &payload)?;
            }
            envelope::Payload::Goodbye(_) => {
                connection.send(envelope::Payload::Goodbye(Goodbye {
                    reason: "client session closed".to_string(),
                }))?;
                return Ok(());
            }
            payload => {
                if !handle_market_message(connection, state, market, attached_client, payload)? {
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

fn handle_workspace_message(
    connection: &mut FramedConnection,
    state: &EngineState,
    payload: envelope::Payload,
) -> Result<(), String> {
    match payload {
        envelope::Payload::RestoreWorkspace(_) => {
            connection.send(envelope::Payload::WorkspaceState(state.workspace()))
        }
        envelope::Payload::SetSelection(selection) => apply_selection(state, selection, connection),
        envelope::Payload::SetWatchlist(watchlist) => apply_watchlist(state, watchlist, connection),
        envelope::Payload::SetViewport(viewport) => apply_viewport(state, viewport, connection),
        envelope::Payload::SetWorkspaceLayout(layout) => {
            apply_workspace_layout(state, layout, connection)
        }
        _ => unreachable!("only workspace messages reach workspace dispatch"),
    }
}

fn handle_engine_control_message(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    shutdown: Option<&EngineShutdown>,
    payload: envelope::Payload,
) -> Result<bool, String> {
    match payload {
        envelope::Payload::SetEngineResourceMode(command) => {
            let Ok(mode) = ResourceMode::try_from(command.resource_mode) else {
                connection.send(envelope::Payload::Fault(Fault {
                    code: EngineFaultCode::MalformedMessage as i32,
                    redacted_detail: "engine resource mode is invalid".to_string(),
                }))?;
                return Ok(false);
            };
            if let Some(market) = market
                && let Err(error) = market.set_resource_mode(mode)
            {
                send_market_fault(connection, error)?;
                return Ok(false);
            }
            connection.send(envelope::Payload::WorkspaceState(
                state.set_resource_mode(mode),
            ))?;
        }
        envelope::Payload::SetEngineLifecycle(command) => {
            apply_engine_lifecycle(connection, state, market, command)?;
        }
        envelope::Payload::GetEngineStatus(_) => {
            let market_status = require_market(market)?.status()?;
            let workspace = state.workspace();
            connection.send(envelope::Payload::EngineStatus(engine_status(
                &workspace,
                &market_status,
                shutdown.is_some_and(EngineShutdown::is_requested),
            )))?;
        }
        envelope::Payload::ShutdownEngine(_) => {
            let Some(shutdown) = shutdown else {
                connection.send(cancelled_mutation_fault(
                    "engine shutdown is unavailable in this session",
                ))?;
                return Ok(false);
            };
            state.begin_shutdown();
            state.set_resource_mode(ResourceMode::OfflineSuspended);
            shutdown.request();
            connection.send(envelope::Payload::Goodbye(Goodbye {
                reason: "engine shutdown accepted".to_string(),
            }))?;
            return Ok(true);
        }
        envelope::Payload::InstallProviderInstrument(instrument) => {
            let market = require_market(market)?;
            match market.install_provider_instrument(&instrument) {
                Ok(()) => {
                    state.record_installed_instrument(&instrument);
                    connection.send(envelope::Payload::ProviderInstrumentInstalled(
                        ProviderInstrumentInstalled {
                            provider: instrument.provider,
                            session_generation: instrument.session_generation,
                            selection_generation: instrument.selection_generation,
                            instrument_id: instrument.instrument_id,
                        },
                    ))
                }
                Err(error) => send_market_fault(connection, error),
            }?;
        }
        _ => unreachable!("only engine control messages reach engine control dispatch"),
    }
    Ok(false)
}

fn handle_account_message(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    payload: &envelope::Payload,
) -> Result<(), String> {
    match payload {
        envelope::Payload::BeginLogin(command) => {
            match state.account().begin_login(command.request_generation) {
                Ok(authorization) => {
                    connection.send(envelope::Payload::LoginAuthorization(authorization))
                }
                Err(error) => send_market_fault(connection, error),
            }
        }
        envelope::Payload::CancelLogin(command) => {
            match state.account().cancel_login(command.request_generation) {
                Ok(()) => connection.send(envelope::Payload::AccountView(
                    state.account().account_status(),
                )),
                Err(error) => send_market_fault(connection, error),
            }
        }
        envelope::Payload::GetAccountStatus(_) => connection.send(envelope::Payload::AccountView(
            state.account().account_status(),
        )),
        envelope::Payload::SignOut(_) => {
            if let Some(market) = market {
                market.set_resource_mode(ResourceMode::OfflineSuspended)?;
            }
            connection.send(envelope::Payload::AccountView(state.account().sign_out()))
        }
        _ => unreachable!("only account messages reach account dispatch"),
    }
}

fn platform_access_requires_account(payload: &envelope::Payload) -> bool {
    !matches!(
        payload,
        envelope::Payload::BeginLogin(_)
            | envelope::Payload::CancelLogin(_)
            | envelope::Payload::GetAccountStatus(_)
            | envelope::Payload::SignOut(_)
            | envelope::Payload::GetEngineStatus(_)
            | envelope::Payload::ShutdownEngine(_)
            | envelope::Payload::Goodbye(_)
    )
}

fn apply_engine_lifecycle(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    command: SetEngineLifecycle,
) -> Result<(), String> {
    match state.apply_lifecycle_preferences(command) {
        Ok(workspace) => {
            if let Some(market) = market {
                let mode = match EngineLifetimeMode::try_from(workspace.lifetime_mode) {
                    Ok(EngineLifetimeMode::KeepMarketsLive) => ResourceMode::MarketsLive,
                    Ok(EngineLifetimeMode::ExitCompletely | EngineLifetimeMode::KeepEngineWarm)
                    | Err(_) => ResourceMode::Warm,
                };
                market.set_resource_mode(mode)?;
            }
            connection.send(envelope::Payload::WorkspaceState(workspace))
        }
        Err(error) if error == WORKSPACE_SHUTTING_DOWN => {
            connection.send(cancelled_mutation_fault(error))
        }
        Err(error) if error == "workspace revision is stale" => {
            connection.send(stale_workspace_fault())
        }
        Err(error) => send_market_fault(connection, error),
    }
}

fn handle_market_message(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: Option<&MarketService>,
    attached_client: &mut Option<u64>,
    payload: envelope::Payload,
) -> Result<bool, String> {
    let market = match payload {
        envelope::Payload::AttachClient(attachment) => {
            let market = require_market(market)?;
            if attached_client.is_some() {
                send_market_fault(connection, "client is already attached")?;
            } else if let Err(error) =
                market.attach_stream(attachment.client_id, connection.event_sender()?)
            {
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
                state.record_client_detach(detachment.client_id);
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
        | envelope::Payload::SelectProviderInstrument(_)) => (require_market(market)?, payload),
        _ => return Ok(false),
    };
    dispatch_market_command(connection, state, market.0, *attached_client, market.1)?;
    Ok(true)
}

fn dispatch_market_command(
    connection: &mut FramedConnection,
    state: &EngineState,
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
            dispatch_register_consumer(connection, state, market, attached_client, registration)?;
        }
        envelope::Payload::SeriesDemand(demand) => {
            dispatch_series_demand(connection, state, market, attached_client, demand)?;
        }
        envelope::Payload::ViewportDemand(viewport) => {
            dispatch_viewport_demand(connection, state, market, attached_client, viewport)?;
        }
        envelope::Payload::VisibilityDemand(visibility) => {
            dispatch_visibility_demand(connection, market, attached_client, visibility)?;
        }
        envelope::Payload::RemoveConsumer(removal) => {
            dispatch_remove_consumer(connection, state, market, attached_client, removal)?;
        }
        _ => unreachable!("market payloads were filtered above"),
    }
    Ok(())
}

fn dispatch_register_consumer(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: &MarketService,
    attached_client: Option<u64>,
    registration: RegisterConsumer,
) -> Result<(), String> {
    if attached_client != Some(registration.client_id) {
        send_market_fault(connection, "consumer owner is not attached")
    } else if let Err(error) = market.register_consumer(
        registration.client_id,
        registration.workspace_id,
        registration.consumer_id,
    ) {
        send_market_fault(connection, error)
    } else {
        state.record_consumer(
            registration.client_id,
            registration.workspace_id,
            registration.consumer_id,
        );
        Ok(())
    }
}

fn dispatch_series_demand(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: &MarketService,
    attached_client: Option<u64>,
    demand: SeriesDemand,
) -> Result<(), String> {
    let Some(client_id) = attached_client else {
        return send_market_fault(connection, "client must attach before setting demand");
    };
    let Some(series) = demand.series else {
        return send_market_fault(connection, "series demand has no identity");
    };
    if let Err(error) = market.set_demand(client_id, demand.consumer_id, demand.generation, &series)
    {
        send_market_fault(connection, error)?;
    } else if let Err(error) = state.record_series_demand(demand.consumer_id, &series) {
        eprintln!("Axiusflow engine hot-set update degraded: {error}");
    }
    Ok(())
}

fn dispatch_viewport_demand(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: &MarketService,
    attached_client: Option<u64>,
    viewport: ViewportDemand,
) -> Result<(), String> {
    let Some(client_id) = attached_client else {
        return send_market_fault(connection, "client must attach before setting viewport");
    };
    if let Err(error) = market.set_viewport(
        client_id,
        viewport.consumer_id,
        viewport.generation,
        viewport.start_unix_nanos,
        viewport.end_unix_nanos,
    ) {
        send_market_fault(connection, error)?;
    } else if let Err(error) = state.record_series_viewport(
        viewport.consumer_id,
        viewport.start_unix_nanos,
        viewport.end_unix_nanos,
    ) {
        eprintln!("Axiusflow engine hot-set viewport update degraded: {error}");
    }
    Ok(())
}

fn dispatch_visibility_demand(
    connection: &mut FramedConnection,
    market: &MarketService,
    attached_client: Option<u64>,
    visibility: VisibilityDemand,
) -> Result<(), String> {
    let Some(client_id) = attached_client else {
        return send_market_fault(connection, "client must attach before setting visibility");
    };
    let resource_class = IpcConsumerResourceClass::try_from(visibility.resource_class)
        .map_err(|_| "consumer resource class is invalid".to_string())?;
    let resource_class = match resource_class {
        IpcConsumerResourceClass::Foreground => {
            axiusflow_market_engine::ConsumerResourceClass::Foreground
        }
        IpcConsumerResourceClass::Background => {
            axiusflow_market_engine::ConsumerResourceClass::Background
        }
        IpcConsumerResourceClass::Warm => axiusflow_market_engine::ConsumerResourceClass::Warm,
        IpcConsumerResourceClass::Detached => {
            axiusflow_market_engine::ConsumerResourceClass::Detached
        }
    };
    if let Err(error) = market.set_resource_class(client_id, visibility.consumer_id, resource_class)
    {
        send_market_fault(connection, error)?;
    }
    Ok(())
}

fn dispatch_remove_consumer(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: &MarketService,
    attached_client: Option<u64>,
    removal: RemoveConsumer,
) -> Result<(), String> {
    let Some(client_id) = attached_client else {
        return send_market_fault(connection, "client must attach before removing a consumer");
    };
    if let Err(error) = market.remove_consumer(client_id, removal.consumer_id) {
        send_market_fault(connection, error)?;
    } else {
        state.record_consumer_removal(removal.consumer_id);
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

fn apply_workspace_layout(
    state: &EngineState,
    layout: SetWorkspaceLayout,
    connection: &mut FramedConnection,
) -> Result<(), String> {
    match state.apply_workspace_layout(layout) {
        Ok(workspace) => connection.send(envelope::Payload::WorkspaceState(workspace)),
        Err(error) if error == WORKSPACE_SHUTTING_DOWN => {
            connection.send(cancelled_mutation_fault(error))
        }
        Err(error)
            if matches!(
                error.as_str(),
                "workspace revision is stale" | "workspace layout generation is stale"
            ) =>
        {
            connection.send(stale_workspace_fault())
        }
        Err(error) => Err(error),
    }
}

fn stale_workspace_fault() -> envelope::Payload {
    cancelled_mutation_fault("workspace revision is stale")
}

fn engine_status(
    workspace: &WorkspaceState,
    market: &MarketServiceStatus,
    shutting_down: bool,
) -> EngineStatus {
    EngineStatus {
        process_id: std::process::id(),
        lifetime_mode: workspace.lifetime_mode,
        resource_mode: market.resource_mode as i32,
        connected_desktop_clients: u32::try_from(market.connected_desktop_clients)
            .unwrap_or(u32::MAX),
        providers: market.providers.clone(),
        retained_series: u32::try_from(market.retained_series).unwrap_or(u32::MAX),
        retained_bars: u64::try_from(market.retained_bars).unwrap_or(u64::MAX),
        approximate_series_bytes: u64::try_from(market.approximate_series_bytes)
            .unwrap_or(u64::MAX),
        shutdown_state: if shutting_down {
            EngineShutdownState::ShuttingDown
        } else {
            EngineShutdownState::Running
        } as i32,
        autostart_enabled: workspace.autostart_enabled,
        markets_live_permitted: workspace.markets_live_permitted,
    }
}

fn cancelled_mutation_fault(detail: impl Into<String>) -> envelope::Payload {
    envelope::Payload::Fault(Fault {
        code: EngineFaultCode::Cancelled as i32,
        redacted_detail: detail.into(),
    })
}

fn account_required_fault() -> envelope::Payload {
    envelope::Payload::Fault(Fault {
        code: EngineFaultCode::Unauthenticated as i32,
        redacted_detail: "sign in before using the Axiusflow platform".to_string(),
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
    use axiusflow_engine_protocol::{ResourceMode, WorkspacePaneKind};

    use super::{EngineShutdown, default_workspace, migrate_workspace};

    #[test]
    fn default_workspace_is_hyperliquid_btc_perp() {
        let workspace = default_workspace();
        assert_eq!(workspace.resource_mode, ResourceMode::Warm as i32);
        assert_eq!(workspace.provider, "hyperliquid");
        assert_eq!(workspace.market, "BTC-PERP");
        assert_eq!(workspace.interval_seconds, 60);
        let primary = workspace.hot_series.first().expect("default hot series");
        assert_eq!(primary.provider, "hyperliquid");
        assert_eq!(primary.instrument_id, "hyperliquid:perp:BTC");
        assert_eq!(primary.provider_symbol, "BTC");
        assert_eq!(primary.price_scale, 8);
        assert_eq!(primary.quantity_scale, 8);
        assert!(
            workspace
                .workspace_tabs
                .iter()
                .flat_map(|tab| &tab.panes)
                .all(|pane| {
                    WorkspacePaneKind::try_from(pane.kind).ok() != Some(WorkspacePaneKind::Chart)
                        || pane
                            .instrument
                            .as_ref()
                            .is_some_and(|instrument| instrument.provider == "hyperliquid")
                })
        );
    }

    #[test]
    fn unsupported_persisted_workspace_is_replaced_during_migration() {
        let mut workspace = default_workspace();
        workspace.hot_series[0].provider = "retired-provider".to_string();

        assert!(migrate_workspace(&mut workspace));
        assert!(
            workspace
                .hot_series
                .iter()
                .all(|series| series.provider == "hyperliquid")
        );
    }

    #[test]
    fn migration_preserves_valid_rithmic_and_hyperliquid_selections() {
        let mut workspace = default_workspace();
        workspace.hot_series[0].provider = "rithmic".to_string();
        workspace.hot_series[0].market = "MNQ".to_string();

        assert!(!migrate_workspace(&mut workspace));
        assert!(
            workspace
                .hot_series
                .iter()
                .any(|series| series.provider == "rithmic")
        );
    }

    #[test]
    fn shutdown_request_is_monotonic() {
        let shutdown = EngineShutdown::default();
        assert!(!shutdown.is_requested());
        shutdown.request();
        assert!(shutdown.is_requested());
        shutdown.request();
        assert!(shutdown.is_requested());
    }
}
