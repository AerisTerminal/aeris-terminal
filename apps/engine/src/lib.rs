//! Resident engine process boundary and authenticated local sessions.

mod market_service;
mod rithmic_history;
mod rithmic_realtime;
mod workspace_layout;

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
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{COINBASE_PUBLIC_ACCOUNT_ID, ENTITLEMENT_CLASS};
use axiusflow_engine_protocol::{
    ClientKind, ConsumerResourceClass as IpcConsumerResourceClass, EngineFaultCode,
    EngineLifetimeMode, EngineReady, EngineShutdownState, EngineStatus, Envelope, EnvelopeDecoder,
    Fault, Goodbye, HotSeries, InstallProviderInstrument, LIFECYCLE_CONTRACT_REVISION,
    MarketEventIdle, PROTOCOL_VERSION, PollMarketEvent, ProviderInstrumentInstalled,
    RegisterConsumer, RemoveConsumer, ResourceMode, SeriesCadence, SeriesDemand, SeriesKey,
    SetEngineLifecycle, SetSelection, SetViewport, SetWatchlist, SetWorkspaceLayout,
    ViewportDemand, VisibilityDemand, WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState, encode_envelope, envelope,
};
use axiusflow_local_engine_client::INSTALLATION_TOKEN_BYTES;
use axiusflow_market_data::{BarPeriod, BarSeriesKey};
use axiusflow_market_engine::{HotSetDescriptor, HotSetEntry, HotSetManager, WorkspaceId};
use axiusflow_platform_runtime::BackgroundService;
use axiusflow_rithmic_protocol_adapter::RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};
const WORKSPACE_SCHEMA_REVISION: u32 = 5;
const CACHE_MANIFEST_REVISION: u32 = 1;
const MAXIMUM_HOT_SERIES: usize = 32;
const COINBASE_PRICE_SCALE: u32 = 2;
const COINBASE_QUANTITY_SCALE: u32 = 8;
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
        let state = Self {
            workspace: Arc::new(Mutex::new(workspace)),
            workspace_root: Some(Arc::new(workspace_root)),
            selection_generation: Arc::new(AtomicU64::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            background_service: Arc::new(Mutex::new(None)),
            hot_set: Arc::new(Mutex::new(hot_set)),
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
        let account_id = match instrument.provider.as_str() {
            "coinbase" => COINBASE_PUBLIC_ACCOUNT_ID,
            "rithmic" => RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
            _ => return,
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

    fn record_market_event(&self, payload: &envelope::Payload) -> Result<(), String> {
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
    let primary = coinbase_hot_series("BTC-USD", 60, 1, unix_seconds());
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
    match provider {
        "coinbase" => COINBASE_PUBLIC_ACCOUNT_ID,
        "rithmic" => RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        _ => "provider-account-unavailable",
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
    if workspace.schema_revision < 3 {
        workspace.hot_series.retain_mut(|series| {
            if series.provider != "coinbase"
                || !matches!(series.market.as_str(), "BTC-USD" | "ETH-USD")
                || !matches!(series.interval_seconds, 60 | 300 | 900 | 3_600)
            {
                return false;
            }
            let migrated = coinbase_hot_series(
                &series.market,
                series.interval_seconds,
                series.score,
                series.last_used_unix_seconds,
            );
            let viewport_start = series.viewport_start_unix_nanos;
            let viewport_end = series.viewport_end_unix_nanos;
            let provider_watermark = series.provider_watermark;
            let series_watermark = series.series_watermark;
            *series = HotSeries {
                viewport_start_unix_nanos: viewport_start,
                viewport_end_unix_nanos: viewport_end,
                provider_watermark,
                series_watermark,
                ..migrated
            };
            true
        });
        workspace.schema_revision = 3;
        migrated = true;
    }
    if workspace.schema_revision < 4 {
        let primary = workspace
            .hot_series
            .iter()
            .find(|series| {
                series.provider == workspace.provider
                    && series.market == workspace.market
                    && series.interval_seconds == workspace.interval_seconds
            })
            .cloned()
            .or_else(|| workspace.hot_series.first().cloned())
            .or_else(|| {
                (workspace.provider == "coinbase").then(|| {
                    coinbase_hot_series(
                        &workspace.market,
                        workspace.interval_seconds,
                        1,
                        unix_seconds(),
                    )
                })
            })
            .unwrap_or_else(|| coinbase_hot_series("BTC-USD", 60, 1, unix_seconds()));
        workspace.layout_generation = 1;
        workspace.active_workspace_id = 1;
        workspace.workspace_tabs = vec![default_workspace_tab(&primary)];
        workspace.schema_revision = WORKSPACE_SCHEMA_REVISION;
        sync_layout_hot_series(workspace);
        migrated = true;
    }
    if workspace.schema_revision < 5 {
        workspace_layout::add_native_layouts(&mut workspace.workspace_tabs);
        workspace.schema_revision = 5;
        migrated = true;
    }
    if workspace.cache_manifest_revision == 0 {
        workspace.cache_manifest_revision = CACHE_MANIFEST_REVISION;
        migrated = true;
    }
    if workspace.hot_series.is_empty() && workspace.provider == "coinbase" {
        touch_hot_series(workspace);
        migrated = true;
    }
    if repair_supported_coinbase_precision(workspace) {
        migrated = true;
    }
    migrated
}

fn repair_supported_coinbase_precision(workspace: &mut WorkspaceState) -> bool {
    let mut repaired = false;
    for series in &mut workspace.hot_series {
        if series.provider == "coinbase"
            && matches!(
                series.instrument_id.as_str(),
                "instrument:coinbase:btc:usd" | "instrument:coinbase:eth:usd"
            )
            && (series.price_scale != COINBASE_PRICE_SCALE
                || series.quantity_scale != COINBASE_QUANTITY_SCALE)
        {
            series.price_scale = COINBASE_PRICE_SCALE;
            series.quantity_scale = COINBASE_QUANTITY_SCALE;
            repaired = true;
        }
    }
    for instrument in workspace
        .workspace_tabs
        .iter_mut()
        .flat_map(|tab| &mut tab.panes)
        .filter_map(|pane| pane.instrument.as_mut())
    {
        if instrument.provider == "coinbase"
            && matches!(
                instrument.instrument_id.as_str(),
                "instrument:coinbase:btc:usd" | "instrument:coinbase:eth:usd"
            )
            && (instrument.price_scale != COINBASE_PRICE_SCALE
                || instrument.quantity_scale != COINBASE_QUANTITY_SCALE)
        {
            instrument.price_scale = COINBASE_PRICE_SCALE;
            instrument.quantity_scale = COINBASE_QUANTITY_SCALE;
            repaired = true;
        }
    }
    repaired
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
    } else if workspace.provider == "coinbase" {
        workspace.hot_series.push(coinbase_hot_series(
            &workspace.market,
            workspace.interval_seconds,
            next_score,
            now,
        ));
    }
    workspace.hot_series.sort_unstable_by(|left, right| {
        right
            .last_used_unix_seconds
            .cmp(&left.last_used_unix_seconds)
            .then_with(|| right.score.cmp(&left.score))
    });
    workspace.hot_series.truncate(MAXIMUM_HOT_SERIES);
}

fn coinbase_hot_series(
    market: &str,
    interval_seconds: u32,
    score: u32,
    last_used_unix_seconds: u64,
) -> HotSeries {
    HotSeries {
        provider: "coinbase".to_string(),
        market: market.to_string(),
        interval_seconds,
        score,
        last_used_unix_seconds,
        provider_watermark: 0,
        series_watermark: 0,
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        instrument_id: format!(
            "instrument:coinbase:{}:{}",
            market_base(market),
            market_quote(market)
        ),
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: interval_seconds,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: None,
        coverage_end_unix_nanos: None,
        provider_symbol: market.to_string(),
        venue_id: "coinbase".to_string(),
        display_symbol: market.replace('-', "/"),
        price_scale: COINBASE_PRICE_SCALE,
        quantity_scale: COINBASE_QUANTITY_SCALE,
    }
}

fn market_base(market: &str) -> String {
    market.split_once('-').map_or_else(
        || market.to_ascii_lowercase(),
        |(base, _)| base.to_ascii_lowercase(),
    )
}

fn market_quote(market: &str) -> String {
    market.split_once('-').map_or_else(
        || "usd".to_string(),
        |(_, quote)| quote.to_ascii_lowercase(),
    )
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
        .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
        self.stream
            .write_all(&frame)
            .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())?;
        self.stream
            .flush()
            .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())
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
                .stream
                .read(&mut chunk)
                .map_err(|_| "ipc_receive failed: local transport is unavailable".to_string())?;
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
        lifecycle_contract_revision: LIFECYCLE_CONTRACT_REVISION,
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
        state.record_client_detach(client_id);
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
            Err(error) if error == "ipc_receive failed: local engine connection closed" => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if shutdown.is_some_and(EngineShutdown::is_requested) {
            connection.send(cancelled_mutation_fault("engine is shutting down"))?;
            return Ok(());
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
        | envelope::Payload::SelectProviderInstrument(_)
        | envelope::Payload::PollMarketEvent(_)) => (require_market(market)?, payload),
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
        envelope::Payload::PollMarketEvent(poll) => {
            dispatch_poll_market_event(connection, state, market, attached_client, poll)?;
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

fn dispatch_poll_market_event(
    connection: &mut FramedConnection,
    state: &EngineState,
    market: &MarketService,
    attached_client: Option<u64>,
    poll: PollMarketEvent,
) -> Result<(), String> {
    let Some(client_id) = attached_client else {
        return send_market_fault(
            connection,
            "client must attach before polling market events",
        );
    };
    match market.poll_event(client_id, poll.consumer_id) {
        Ok(Some(event)) => {
            if let Err(error) = state.record_market_event(&event) {
                eprintln!("Axiusflow engine hot-set coverage update degraded: {error}");
            }
            connection.send(event)
        }
        Ok(None) => connection.send(envelope::Payload::MarketEventIdle(MarketEventIdle {
            consumer_id: poll.consumer_id,
        })),
        Err(error) => send_market_fault(connection, error),
    }
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
    use axiusflow_engine_protocol::{
        EngineLifetimeMode, EngineShutdownState, InstallProviderInstrument, ResourceMode,
        SeriesCadence, SeriesKey, envelope,
    };
    use axiusflow_local_engine_client::EngineClient;
    use axiusflow_market_data::MarketBar;

    use super::{
        EngineShutdown, EngineState, MarketService, RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        bind_listener, default_workspace, serve_client_with_market,
        serve_client_with_market_and_shutdown, sync_layout_hot_series,
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

    fn assert_rithmic_hot_metadata(state: &EngineState) {
        let hot = state
            .workspace()
            .hot_series
            .into_iter()
            .find(|series| series.provider == "rithmic")
            .expect("Rithmic demand becomes durable hot metadata");
        assert_eq!(hot.instrument_id, "instrument:rithmic:CME:MNQU6");
        assert_eq!(hot.account_id, RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID);
        assert_eq!(hot.provider_symbol, "MNQU6");
        assert_eq!(hot.venue_id, "CME");
        assert_eq!(hot.entitlement_id, "rithmic-test:CME:MNQU6");
        assert_eq!(hot.cadence, SeriesCadence::Trades as i32);
        assert_eq!(hot.cadence_value, 100);
        assert_eq!(hot.workspace_ids, vec![1]);
    }

    fn rithmic_fixture_instrument() -> InstallProviderInstrument {
        InstallProviderInstrument {
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
        let lifecycle = client
            .set_engine_lifecycle(
                interactive.workspace_revision,
                EngineLifetimeMode::KeepMarketsLive,
                false,
                true,
            )
            .expect("persist markets-live lifecycle policy");
        assert_eq!(
            lifecycle.lifetime_mode,
            EngineLifetimeMode::KeepMarketsLive as i32
        );
        assert!(lifecycle.markets_live_permitted);
        let status = client.engine_status().expect("read bounded engine status");
        assert_eq!(status.process_id, std::process::id());
        assert_eq!(status.resource_mode, ResourceMode::MarketsLive as i32);
        assert_eq!(status.connected_desktop_clients, 0);
        assert_eq!(status.shutdown_state, EngineShutdownState::Running as i32);
        client.shutdown_engine().expect("request engine shutdown");
        server.join().expect("join lifecycle server");

        assert!(shutdown.is_requested());
        assert_eq!(
            state.workspace().resource_mode,
            ResourceMode::OfflineSuspended as i32
        );
    }

    #[test]
    fn authenticated_engine_attach_precedes_market_provider_readiness() {
        let socket_name = socket_name("attach-before-provider");
        let listener = bind_listener(&socket_name).expect("bind pre-provider endpoint");
        let token = [29_u8; 32];
        let market = MarketService::start_fixture(fixture_history())
            .expect("engine market owner starts without a realtime provider");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept pre-provider client");
            serve_client_with_market(stream, &token, 41, &EngineState::default(), &market)
                .expect("serve pre-provider client");
        });

        let mut client =
            EngineClient::connect(&socket_name, &token).expect("engine IPC is ready independently");
        assert_eq!(client.ready().engine_epoch, 41);
        client
            .attach_client(41)
            .expect("desktop attaches before provider readiness");
        client
            .restore_workspace()
            .expect("attached client remains responsive without provider readiness");

        drop(client);
        server.join().expect("join pre-provider server");
    }

    #[test]
    fn authenticated_remove_consumer_keeps_another_chart_live() {
        let socket_name = socket_name("remove-consumer");
        let listener = bind_listener(&socket_name).expect("bind consumer cleanup endpoint");
        let token = [17_u8; 32];
        let market =
            MarketService::start_fixture(fixture_history()).expect("fixture market starts");
        let server_market = market.clone();
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept consumer cleanup client");
            serve_client_with_market(stream, &token, 21, &EngineState::default(), &server_market)
                .expect("serve consumer cleanup client");
        });

        let mut client =
            EngineClient::connect(&socket_name, &token).expect("connect consumer cleanup client");
        let series = cached_series(BTC_INSTRUMENT, 60);
        client.attach_client(21).expect("IPC client attaches");
        for (workspace_id, consumer_id) in [(1, 101), (2, 102)] {
            client
                .register_consumer(21, workspace_id, consumer_id)
                .expect("chart consumer registers");
            client
                .set_series_demand(consumer_id, 1, series.clone())
                .expect("chart demand succeeds");
            poll_ipc_snapshot(&mut client, consumer_id, 1, &series);
        }
        client
            .set_market_visibility(101, false)
            .expect("first workspace chart becomes inactive");
        client
            .set_market_visibility(102, true)
            .expect("second workspace chart remains active");

        client
            .remove_market_consumer(101)
            .expect("first chart consumer removes");
        client
            .restore_workspace()
            .expect("consumer removal synchronization fence");
        assert!(
            market.poll_event(21, 101).is_err(),
            "removed chart publication is unavailable"
        );

        client
            .set_series_demand(102, 2, series.clone())
            .expect("remaining chart demand succeeds");
        poll_ipc_snapshot(&mut client, 102, 2, &series);

        drop(client);
        server.join().expect("join consumer cleanup server");
    }

    #[test]
    fn authenticated_multi_consumer_polling_realigns_after_a_command_fault() {
        let socket_name = socket_name("market-response-realignment");
        let listener = bind_listener(&socket_name).expect("bind realignment endpoint");
        let token = [31_u8; 32];
        let market =
            MarketService::start_fixture(fixture_history()).expect("fixture market starts");
        let server_market = market.clone();
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept realignment client");
            serve_client_with_market(stream, &token, 23, &EngineState::default(), &server_market)
                .expect("serve realignment client");
        });

        let mut client =
            EngineClient::connect(&socket_name, &token).expect("connect realignment client");
        let series = cached_series(BTC_INSTRUMENT, 60);
        client.attach_client(23).expect("IPC client attaches");
        for consumer_id in [101, 102] {
            client
                .register_consumer(23, 1, consumer_id)
                .expect("consumer registers");
            client
                .set_series_demand(consumer_id, 1, series.clone())
                .expect("consumer demand succeeds");
            poll_ipc_snapshot(&mut client, consumer_id, 1, &series);
            while client
                .poll_market_event(consumer_id)
                .expect("consumer drains initial publications")
                .is_some()
            {}
        }

        client
            .set_market_visibility(999, false)
            .expect("unknown-consumer visibility command crosses IPC");
        assert!(matches!(
            client
                .poll_market_event(101)
                .expect("command fault is delivered"),
            Some(envelope::Payload::Fault(_))
        ));
        assert_eq!(
            client
                .poll_market_event(102)
                .expect("second consumer remains aligned"),
            None
        );
        assert_eq!(
            client
                .poll_market_event(101)
                .expect("first consumer receives its buffered idle"),
            None
        );

        drop(client);
        server.join().expect("join realignment server");
    }

    #[test]
    fn authenticated_connection_drop_retires_detached_client_consumers() {
        let socket_name = socket_name("disconnect-cleanup");
        let listener = bind_listener(&socket_name).expect("bind disconnect cleanup endpoint");
        let token = [19_u8; 32];
        let market =
            MarketService::start_fixture(fixture_history()).expect("fixture market starts");
        let server_market = market.clone();
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept disconnect cleanup client");
            serve_client_with_market(stream, &token, 23, &EngineState::default(), &server_market)
                .expect("serve disconnect cleanup client");
        });

        let mut client =
            EngineClient::connect(&socket_name, &token).expect("connect disconnect cleanup client");
        let series = cached_series(BTC_INSTRUMENT, 60);
        client.attach_client(23).expect("IPC client attaches");
        client
            .register_consumer(23, 1, 103)
            .expect("chart consumer registers");
        client
            .set_series_demand(103, 1, series.clone())
            .expect("chart demand succeeds");
        poll_ipc_snapshot(&mut client, 103, 1, &series);

        drop(client);
        server.join().expect("join disconnected client server");

        assert!(
            market.poll_event(23, 103).is_err(),
            "disconnect removes detached publication state"
        );
        market.attach(23).expect("client identity can reattach");
        market
            .register_consumer(23, 1, 103)
            .expect("detached consumer identity was fully retired");
        market.detach(23).expect("reattached client detaches");
    }

    #[test]
    fn dormant_market_client_does_not_starve_another_clients_control() {
        let socket_name = socket_name("independent-control");
        let listener = bind_listener(&socket_name).expect("bind independent control endpoint");
        let token = [23_u8; 32];
        let state = EngineState::default();
        let server_state = state.clone();
        let market =
            MarketService::start_fixture(fixture_history()).expect("fixture market starts");
        let server_market = market.clone();
        let server = thread::spawn(move || {
            let mut sessions = Vec::new();
            for _ in 0..2 {
                let stream = listener.accept().expect("accept local client");
                let state = server_state.clone();
                let market = server_market.clone();
                sessions.push(thread::spawn(move || {
                    serve_client_with_market(stream, &token, 29, &state, &market)
                        .expect("serve local client");
                }));
            }
            for session in sessions {
                session.join().expect("join local client session");
            }
        });

        let series = cached_series(BTC_INSTRUMENT, 60);
        let mut dormant =
            EngineClient::connect(&socket_name, &token).expect("connect dormant market client");
        dormant.attach_client(31).expect("dormant client attaches");
        dormant
            .register_consumer(31, 1, 201)
            .expect("dormant consumer registers");
        dormant
            .set_series_demand(201, 1, series.clone())
            .expect("dormant demand succeeds");
        dormant
            .restore_workspace()
            .expect("dormant demand synchronization fence");

        let mut control =
            EngineClient::connect(&socket_name, &token).expect("connect independent control");
        let interactive = control
            .set_engine_resource_mode(ResourceMode::Interactive)
            .expect("independent control remains responsive");
        assert_eq!(interactive.resource_mode, ResourceMode::Interactive as i32);
        control.attach_client(32).expect("second client attaches");
        control
            .register_consumer(32, 1, 202)
            .expect("second consumer registers");
        control
            .set_series_demand(202, 1, series.clone())
            .expect("second demand succeeds");
        poll_ipc_snapshot(&mut control, 202, 1, &series);

        drop(control);
        drop(dormant);
        server.join().expect("join independent control server");
        assert_eq!(
            state.workspace().resource_mode,
            ResourceMode::Interactive as i32
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
        let state = EngineState::default();
        let server_state = state.clone();
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept market client");
            serve_client_with_market(stream, &token, 9, &server_state, &market)
                .expect("serve market client");
        });
        let mut client =
            EngineClient::connect(&socket_name, &token).expect("connect market client");
        let installed = client
            .install_provider_instrument(rithmic_fixture_instrument())
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
                    cadence: axiusflow_engine_protocol::SeriesCadence::Trades as i32,
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
                                == axiusflow_engine_protocol::SeriesLoadState::Ready as i32;
                    }
                    envelope::Payload::OrderBookSnapshot(snapshot) => {
                        order_book_received = snapshot.consumer_id == 1
                            && snapshot.generation == 1
                            && snapshot.provider_generation == 7
                            && snapshot.selection_generation == 9
                            && snapshot.state
                                == axiusflow_engine_protocol::OrderBookState::AwaitingSnapshot
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
        assert_rithmic_hot_metadata(&state);
    }

    #[test]
    fn closing_a_workspace_preserves_bounded_recency_metadata() {
        let mut workspace = default_workspace();
        let mut recent = workspace.hot_series[0].clone();
        recent.workspace_ids.clear();
        workspace.workspace_tabs.clear();

        sync_layout_hot_series(&mut workspace);

        assert_eq!(workspace.hot_series, vec![recent]);
        assert!(workspace.hot_series[0].workspace_ids.is_empty());
    }
}
