//! Explicit direct-device Coinbase desktop composition.

mod composition;
#[cfg(test)]
mod conformance;
mod diagnostics;
mod history;
mod lifecycle;
mod provenance;
mod publication;

use crate::market_worker::{
    ChartState, MarketDataWorker, MarketWorkerMessage, MarketWorkerSender, MarketWorkerStartup,
    UiDiagnosticsReceiver, market_worker_channel, ui_diagnostics_channel,
};
use axiusflow_application::{
    MarketBarClientModel, ProvenancedMarketBar, ReplayStreamUpdate, StreamDelta,
};
use axiusflow_chart_integration::ReplayRecoveryCommand;
use axiusflow_coinbase_market_adapter::{
    CoinbaseAggregatedBar, CoinbaseDesktopEventError, CoinbaseDesktopMarketEvent,
    CoinbaseHttpsHistoryTransport, CoinbaseInterval, CoinbaseLevel2Book, CoinbaseLevel2Outcome,
    CoinbaseProductCatalog, CoinbaseProviderEvents, aggregate_coinbase_bars, coinbase_depth_limit,
};
use axiusflow_desktop_provider_runtime::{
    DesktopMarketWorkerError, DesktopProviderError, DesktopProviderState, SessionGeneration,
};
use axiusflow_desktop_storage::SegmentEncryptionKey;
use axiusflow_instruments::{InstrumentPrecision, InstrumentRevision};
use axiusflow_market_data::{BarDefinition, MarketEvent};
use axiusflow_platform_runtime::{NetworkEvent, PowerEvent};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, ThreadId},
    time::Instant,
};

use composition::{
    CoinbaseDesktopWorker, OpenedWorker, ProductProfile, bar_definition_for_interval, client_model,
    instrument, loading_startup, nonzero, open_worker, product_profile, product_profile_from_spot,
    unix_nanos, worker_label,
};
use diagnostics::{diagnostics_wait_duration, flush_diagnostics};
use history::{
    DirectHistorySource, HistorySource, InitialHistoryContext, PreparedHistory,
    StreamingSeriesContext, install_ready_history, prepare_initial_history,
};
use lifecycle::{
    EnvironmentalEvent, InboxDrainContext, ReconnectBackoff, WorkerInboxEvent,
    apply_initial_network, drain_worker_inbox, environment_events, forward_commands,
    wait_for_inbox,
};
use provenance::{cached_history_provenance, history_provenance, live_provenance};
use publication::{publish_cached_update, publish_ready_recovery, publish_update};

const HISTORY_BARS: usize = 300;
const MODEL_ITEM_CAPACITY: usize = 350;
const PROVIDER_EVENT_CAPACITY: usize = 16_384;
const PROVIDER_EVENT_BATCH: usize = 1_024;
const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 1;
const PARTITION_ID: u32 = 7;
const SCHEMA_VERSION: u32 = 1;
const INBOX_CAPACITY: usize = 64;
const INBOX_BATCH: usize = 1_024;
const UI_DIAGNOSTICS_CAPACITY: usize = 256;
const SUBSCRIPTION_ID: &str = "desktop_coinbase_one_minute_bars";
const VAULT_SERVICE: &str = "axiusflow-desktop-market-history";
const CATALOG_KEY_ID: &str = "history-catalog-key-v1";
const SEGMENT_KEY_ID: &str = "coinbase-public-bars-key-v1";
const COINBASE_DISK_CACHE_BYTES: u64 = 256 * 1024 * 1024;

struct LiveLoopState {
    prepared: Option<PreparedHistory>,
    streaming_generation: Option<SessionGeneration>,
    retained: VecDeque<ProvenancedMarketBar>,
    reconnect_backoff: ReconnectBackoff,
    recovery_announced: bool,
    pending_recovery: VecDeque<ReplayRecoveryCommand>,
}

struct WorkerThreadInput {
    profile: ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    message_tx: MarketWorkerSender,
    inbox_tx: SyncSender<WorkerInboxEvent>,
    inbox_rx: Receiver<WorkerInboxEvent>,
    provider_wake_pending: Arc<AtomicBool>,
    ui_diagnostics_rx: UiDiagnosticsReceiver,
    detailed_diagnostics: bool,
}

struct RunningWorker<V: axiusflow_platform_runtime::CredentialVault, H: HistorySource> {
    profile: ProductProfile,
    worker: CoinbaseDesktopWorker<V>,
    events: CoinbaseProviderEvents,
    segment_key: SegmentEncryptionKey,
    instrument: InstrumentRevision,
    bar_definition: BarDefinition,
    worker_label: String,
    model: MarketBarClientModel,
    state: LiveLoopState,
    history_source: H,
    level2: Option<CoinbaseLevel2Book>,
    dom: axiusflow_terminal_ui::ReadOnlyDom,
}

impl<V: axiusflow_platform_runtime::CredentialVault, H: HistorySource> RunningWorker<V, H> {
    fn reconcile_recovery(&mut self, message_tx: &MarketWorkerSender) -> Result<(), String> {
        request_recovery_if_required(&mut self.worker, &self.events, &mut self.state, message_tx)
    }
}

struct CoinbaseCallbackContext<'a> {
    streaming_generation: Option<SessionGeneration>,
    retained: &'a mut VecDeque<ProvenancedMarketBar>,
    model: &'a mut MarketBarClientModel,
    worker_label: &'a str,
    profile: &'a ProductProfile,
    instrument: &'a InstrumentRevision,
    bar_definition: &'a BarDefinition,
    level2: &'a mut Option<CoinbaseLevel2Book>,
    dom: &'a mut axiusflow_terminal_ui::ReadOnlyDom,
    message_tx: &'a MarketWorkerSender,
}

pub(crate) fn start(
    product_id: String,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let profile = product_profile(product_id)?;
    start_with_profile(profile, history_root, ui_thread, detailed_diagnostics)
}

pub(crate) fn start_product_interval(
    product: axiusflow_coinbase_market_adapter::CoinbaseSpotProduct,
    interval: axiusflow_market_data::ChartInterval,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    start_with_profile(
        product_profile_from_spot(product, interval),
        history_root,
        ui_thread,
        detailed_diagnostics,
    )
}

fn start_with_profile(
    profile: ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    enforce_coinbase_disk_cache_quota(&history_root, COINBASE_DISK_CACHE_BYTES)?;
    let startup = loading_startup(&profile, history_root.clone(), detailed_diagnostics);
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    let catalog_message_tx = message_tx.clone();
    thread::Builder::new()
        .name("axiusflow-coinbase-product-catalog".to_string())
        .spawn(move || {
            let mut catalog = CoinbaseProductCatalog::with_transport(CoinbaseHttpsHistoryTransport);
            let result = catalog
                .fetch_active_spot_products()
                .map_err(|error| error.to_string());
            let _ = catalog_message_tx.send(MarketWorkerMessage::CoinbaseCatalog(result));
        })
        .map_err(|error| error.to_string())?;
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (inbox_tx, inbox_rx) = mpsc::sync_channel(INBOX_CAPACITY);
    let diagnostics_wake_tx = inbox_tx.clone();
    let diagnostics_wake = Arc::new(move || {
        let _ = diagnostics_wake_tx.try_send(WorkerInboxEvent::UiDiagnosticsReady);
    });
    let (ui_diagnostics_tx, ui_diagnostics_rx) =
        ui_diagnostics_channel(nonzero(UI_DIAGNOSTICS_CAPACITY), diagnostics_wake);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    let provider_wake_pending = Arc::new(AtomicBool::new(false));
    let command_inbox_tx = inbox_tx.clone();
    thread::Builder::new()
        .name("axiusflow-coinbase-command-inbox".to_string())
        .spawn(move || forward_commands(&command_rx, &command_inbox_tx))
        .map_err(|error| error.to_string())?;
    thread::Builder::new()
        .name("axiusflow-coinbase-market-worker".to_string())
        .spawn(move || {
            let error_tx = message_tx.clone();
            if let Err(error) = run_worker(WorkerThreadInput {
                profile,
                history_root,
                ui_thread,
                message_tx,
                inbox_tx,
                inbox_rx,
                provider_wake_pending,
                ui_diagnostics_rx,
                detailed_diagnostics,
            }) {
                let _ = error_tx.send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: error,
                });
            }
            let _ = shutdown_tx.send(());
        })
        .map_err(|error| error.to_string())?;
    Ok((
        startup,
        MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            Some(ui_diagnostics_tx),
        ),
    ))
}

fn run_worker(input: WorkerThreadInput) -> Result<(), String> {
    let WorkerThreadInput {
        profile,
        history_root,
        ui_thread,
        message_tx,
        inbox_tx,
        inbox_rx,
        provider_wake_pending,
        ui_diagnostics_rx,
        detailed_diagnostics,
    } = input;
    let OpenedWorker {
        worker,
        events,
        segment_key,
    } = open_worker(
        &profile,
        history_root,
        ui_thread,
        &inbox_tx,
        &provider_wake_pending,
        detailed_diagnostics,
    )?;
    let (initial_network, monitors_active) = environment_events(inbox_tx.clone());
    let running = prepare_running_worker(
        profile,
        OpenedWorker {
            worker,
            events,
            segment_key,
        },
        initial_network,
        monitors_active,
        DirectHistorySource,
        &message_tx,
    )?;
    run_worker_loop(
        running,
        &message_tx,
        &inbox_rx,
        &provider_wake_pending,
        &ui_diagnostics_rx,
    )
}

fn prepare_running_worker<V: axiusflow_platform_runtime::CredentialVault, H: HistorySource>(
    profile: ProductProfile,
    opened: OpenedWorker<V>,
    initial_network: Option<NetworkEvent>,
    monitors_active: bool,
    history_source: H,
    message_tx: &MarketWorkerSender,
) -> Result<RunningWorker<V, H>, String> {
    let OpenedWorker {
        mut worker,
        events,
        segment_key,
    } = opened;
    apply_initial_network(&mut worker, initial_network)?;
    let instrument = instrument(&profile)?;
    let bar_definition = bar_definition_for_interval(profile.interval);
    let worker_label = worker_label(monitors_active);
    let mut model = client_model();
    let retained = prepare_initial_history(
        &mut worker,
        &history_source,
        &InitialHistoryContext {
            profile: &profile,
            segment_key: &segment_key,
            instrument: &instrument,
            bar_definition: &bar_definition,
            worker_label: &worker_label,
            initial_network,
        },
        &mut model,
        message_tx,
    )?;
    Ok(RunningWorker {
        instrument,
        bar_definition,
        worker_label,
        model,
        state: LiveLoopState {
            prepared: None,
            streaming_generation: None,
            retained,
            reconnect_backoff: ReconnectBackoff::new(),
            recovery_announced: false,
            pending_recovery: VecDeque::with_capacity(COMMAND_CAPACITY),
        },
        history_source,
        level2: None,
        dom: axiusflow_terminal_ui::ReadOnlyDom::new(coinbase_depth_limit()),
        profile,
        worker,
        events,
        segment_key,
    })
}

fn run_worker_loop<V: axiusflow_platform_runtime::CredentialVault, H: HistorySource>(
    mut running: RunningWorker<V, H>,
    message_tx: &MarketWorkerSender,
    inbox_rx: &Receiver<WorkerInboxEvent>,
    provider_wake_pending: &AtomicBool,
    ui_diagnostics_rx: &UiDiagnosticsReceiver,
) -> Result<(), String> {
    let mut ready_event = None;

    loop {
        if drain_worker_inbox(
            inbox_rx,
            &mut ready_event,
            &mut InboxDrainContext {
                worker: &mut running.worker,
                events: &running.events,
                state: &mut running.state,
                message_tx,
                provider_wake_pending,
            },
        )? {
            running.worker.stop().map_err(|error| error.to_string())?;
            return Ok(());
        }
        flush_diagnostics(&mut running.worker, ui_diagnostics_rx, message_tx)?;

        running.reconcile_recovery(message_tx)?;

        establish_coinbase_stream_if_ready(
            &mut running.worker,
            &running.events,
            running.state.streaming_generation,
        )?;

        if install_ready_history(
            &mut running.worker,
            &mut running.history_source,
            &StreamingSeriesContext {
                profile: &running.profile,
                segment_key: &running.segment_key,
                instrument: &running.instrument,
                bar_definition: &running.bar_definition,
                worker_label: &running.worker_label,
            },
            &mut running.state,
            &mut running.model,
            message_tx,
        )? {
            continue;
        }

        if running.state.streaming_generation.is_some() {
            drain_coinbase_callbacks(
                &mut running.worker,
                &running.events,
                CoinbaseCallbackContext {
                    streaming_generation: running.state.streaming_generation,
                    retained: &mut running.state.retained,
                    model: &mut running.model,
                    worker_label: &running.worker_label,
                    profile: &running.profile,
                    instrument: &running.instrument,
                    bar_definition: &running.bar_definition,
                    level2: &mut running.level2,
                    dom: &mut running.dom,
                    message_tx,
                },
            )?;
        }

        running.reconcile_recovery(message_tx)?;

        if !publish_ready_recovery(
            running.state.streaming_generation,
            &mut running.state.pending_recovery,
            message_tx,
            (&running.instrument, &running.bar_definition),
            &running.state.retained,
            &mut running.model,
            &running.worker_label,
        ) {
            return Ok(());
        }
        discard_provider_events(&mut running.worker)?;
        flush_diagnostics(&mut running.worker, ui_diagnostics_rx, message_tx)?;
        if running.events.has_ready() {
            continue;
        }
        let recovery_required = matches!(
            running
                .worker
                .provider_state()
                .map_err(|error| error.to_string())?,
            DesktopProviderState::RecoveryRequired { .. }
        );
        ready_event = wait_for_inbox(
            inbox_rx,
            Some(diagnostics_wait_duration(
                running
                    .state
                    .reconnect_backoff
                    .wait_duration(recovery_required, Instant::now()),
            )),
        )?;
    }
}

fn discard_provider_events<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
) -> Result<(), String> {
    while worker
        .try_recv_provider_event()
        .map_err(|error| error.to_string())?
        .is_some()
    {}
    Ok(())
}

fn fence_failed_history<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    recovery_announced: &mut bool,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    worker.reset_aggregation();
    worker
        .session_invalid(generation)
        .map_err(|failure| failure.to_string())?;
    retained.clear();
    *recovery_announced = true;
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: ChartState::Recovering,
        message: "Coinbase history recovery required; awaiting a fresh covering snapshot"
            .to_string(),
    });
    Ok(())
}

fn apply_environment_event<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    events: &CoinbaseProviderEvents,
    event: EnvironmentalEvent,
    prepared: &mut Option<PreparedHistory>,
    streaming_generation: &mut Option<SessionGeneration>,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    message_tx: &MarketWorkerSender,
) -> Result<bool, String> {
    let next = match event {
        EnvironmentalEvent::Network(NetworkEvent::Unavailable) => {
            let next = worker.handle_network_event(NetworkEvent::Unavailable);
            discard_coinbase_callbacks(events);
            next
        }
        EnvironmentalEvent::Power(PowerEvent::Suspending) => {
            let next = worker.handle_power_event(PowerEvent::Suspending);
            discard_coinbase_callbacks(events);
            next
        }
        EnvironmentalEvent::Network(NetworkEvent::Available) => {
            discard_coinbase_callbacks(events);
            worker.handle_network_event(NetworkEvent::Available)
        }
        EnvironmentalEvent::Power(PowerEvent::Resumed) => {
            discard_coinbase_callbacks(events);
            worker.handle_power_event(PowerEvent::Resumed)
        }
    }
    .map_err(|error| error.to_string())?;
    worker.reset_aggregation();
    if next.is_some() {
        *prepared = None;
    }
    *streaming_generation = None;
    retained.clear();
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: ChartState::Stale,
        message: "direct provider lifecycle changed; a fresh snapshot is required".to_string(),
    });
    Ok(true)
}

fn discard_coinbase_callbacks(events: &CoinbaseProviderEvents) {
    while events.try_recv().is_some() {}
}

fn drain_coinbase_callbacks<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    events: &CoinbaseProviderEvents,
    context: CoinbaseCallbackContext<'_>,
) -> Result<(), String> {
    let CoinbaseCallbackContext {
        streaming_generation,
        retained,
        model,
        worker_label,
        profile,
        instrument,
        bar_definition,
        level2,
        dom,
        message_tx,
    } = context;
    for _ in 0..PROVIDER_EVENT_BATCH {
        if !events.has_ready() {
            break;
        }
        let received = match worker.try_recv_coinbase_market_event(events) {
            Ok(received) => received,
            Err(_error)
                if matches!(
                    worker.provider_state().map_err(|error| error.to_string())?,
                    DesktopProviderState::RecoveryRequired { .. }
                ) =>
            {
                return Ok(());
            }
            Err(error) if is_stale_coinbase_callback(&error) => continue,
            Err(error) => return Err(error.to_string()),
        };
        let Some(received) = received else {
            continue;
        };
        if let CoinbaseDesktopMarketEvent::Level2 {
            generation,
            payload,
        } = received
        {
            publish_coinbase_depth(profile, generation, &payload, level2, dom, message_tx)?;
            continue;
        }
        let CoinbaseDesktopMarketEvent::Bar {
            generation,
            completed,
        } = received
        else {
            unreachable!("Coinbase market event variants are exhaustive")
        };
        if streaming_generation != Some(generation) {
            return Err("completed Coinbase bar arrived before history seeding".to_string());
        }
        if profile.interval != axiusflow_market_data::ChartInterval::Minute1 {
            publish_aggregated_coinbase_interval(
                worker,
                generation,
                completed,
                retained,
                model,
                worker_label,
                profile,
                instrument,
                bar_definition,
                message_tx,
            )?;
            continue;
        }
        let item = live_provenance(completed, generation)?;
        let previous = retained
            .back()
            .ok_or_else(|| "Coinbase live stream has no snapshot predecessor".to_string())?
            .value()
            .source_sequence;
        let delta = StreamDelta::try_new(previous, item.value().source_sequence, item)
            .map_err(|error| error.to_string())?;
        retained.push_back(delta.item().clone());
        if retained.len() > MODEL_ITEM_CAPACITY {
            retained.pop_front();
        }
        publish_update(
            worker,
            generation,
            model,
            ReplayStreamUpdate::Delta(delta),
            worker_label,
            message_tx,
        )?;
    }
    Ok(())
}

fn enforce_coinbase_disk_cache_quota(
    history_root: &std::path::Path,
    quota: u64,
) -> Result<(), String> {
    let segments = history_root.join("segments");
    if !segments.exists() {
        return Ok(());
    }
    let root = history_root
        .canonicalize()
        .map_err(|error| format!("Coinbase cache root is unavailable: {error}"))?;
    let segments = segments
        .canonicalize()
        .map_err(|error| format!("Coinbase cache segment root is unavailable: {error}"))?;
    if !segments.starts_with(&root) {
        return Err("Coinbase cache segment root escaped its configured root".to_string());
    }
    let mut files = std::fs::read_dir(&segments)
        .map_err(|error| format!("Coinbase cache inventory failed: {error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                let modified = metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                (entry.path(), metadata.len(), modified)
            })
        })
        .collect::<Vec<_>>();
    let mut retained_bytes = files.iter().map(|(_, bytes, _)| *bytes).sum::<u64>();
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, bytes, _) in files {
        if retained_bytes <= quota {
            break;
        }
        let resolved = path
            .canonicalize()
            .map_err(|error| format!("Coinbase cache entry is unavailable: {error}"))?;
        if !resolved.starts_with(&segments) {
            return Err("Coinbase cache entry escaped its segment root".to_string());
        }
        std::fs::remove_file(&resolved)
            .map_err(|error| format!("Coinbase cache eviction failed: {error}"))?;
        retained_bytes = retained_bytes.saturating_sub(bytes);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_aggregated_coinbase_interval<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    completed: CoinbaseAggregatedBar,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
    profile: &ProductProfile,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let interval = CoinbaseInterval::try_from(profile.interval).map_err(str::to_string)?;
    let (bucket, _) = aggregate_coinbase_bars(&[completed.bar], interval)?;
    let mut bucket = *bucket
        .first()
        .ok_or_else(|| "Coinbase live interval aggregation produced no bar".to_string())?;
    let previous_sequence = retained
        .back()
        .ok_or_else(|| "Coinbase live stream has no snapshot predecessor".to_string())?
        .value()
        .source_sequence;
    let replace_last = retained.back().is_some_and(|previous| {
        previous.value().exchange_timestamp_seconds == bucket.exchange_timestamp_seconds
    });
    if replace_last {
        let previous = *retained
            .back()
            .ok_or_else(|| "Coinbase aggregate predecessor disappeared".to_string())?
            .value();
        let (combined, _) = aggregate_coinbase_bars(&[previous, completed.bar], interval)?;
        bucket = *combined
            .first()
            .ok_or_else(|| "Coinbase live interval merge produced no bar".to_string())?;
        bucket.source_sequence = previous_sequence;
    } else {
        bucket.source_sequence = previous_sequence
            .checked_add(1)
            .ok_or_else(|| "Coinbase live interval sequence overflow".to_string())?;
    }
    let item = live_provenance(
        CoinbaseAggregatedBar {
            bar: bucket,
            provider_timestamp_unix_nanos: completed.provider_timestamp_unix_nanos,
            provider_sequence_num: completed.provider_sequence_num,
        },
        generation,
    )?;
    if replace_last {
        retained.pop_back();
    }
    retained.push_back(item);
    if retained.len() > MODEL_ITEM_CAPACITY {
        retained.pop_front();
    }
    let snapshot = axiusflow_application::ReplaySnapshot::try_from_provenanced_values(
        instrument.clone(),
        axiusflow_application::ReplayProvenance::LiveProvider,
        bar_definition.clone(),
        model
            .current_generation()
            .map_or(1, |current| current.generation().saturating_add(1)),
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    publish_update(
        worker,
        generation,
        model,
        ReplayStreamUpdate::Snapshot(snapshot),
        worker_label,
        message_tx,
    )
}

fn publish_coinbase_depth(
    profile: &ProductProfile,
    generation: SessionGeneration,
    payload: &[u8],
    level2: &mut Option<CoinbaseLevel2Book>,
    dom: &mut axiusflow_terminal_ui::ReadOnlyDom,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let generation_value = generation.get();
    if level2.is_none()
        || dom.selection().is_none_or(|selection| {
            selection.session_generation != generation_value
                || selection.instrument_id != profile.instrument_id
        })
    {
        *level2 = Some(
            CoinbaseLevel2Book::try_new(
                profile.product_id.clone(),
                profile.price_scale,
                profile.quantity_scale,
                generation_value,
            )
            .map_err(|error| error.to_string())?,
        );
        dom.select(axiusflow_terminal_ui::DomSelection {
            provider_id: "coinbase".to_string(),
            instrument_id: profile.instrument_id.clone(),
            entitlement_id: axiusflow_coinbase_market_adapter::ENTITLEMENT_CLASS.to_string(),
            session_generation: generation_value,
            selection_generation: generation_value,
            precision: InstrumentPrecision::try_new(profile.price_scale, profile.quantity_scale)
                .map_err(|error| error.to_string())?,
        });
    }
    let outcome = level2
        .as_mut()
        .ok_or_else(|| "Coinbase Level 2 state is unavailable".to_string())?
        .apply_message(payload, unix_nanos()?)
        .map_err(|error| error.to_string())?;
    let frame = match outcome {
        CoinbaseLevel2Outcome::Snapshot(snapshot) => match dom
            .apply_event(&MarketEvent::DepthSnapshot(snapshot))
            .map_err(|error| error.to_string())?
        {
            axiusflow_terminal_ui::DomUpdateOutcome::Published(frame)
            | axiusflow_terminal_ui::DomUpdateOutcome::RecoveryRequired(frame, _) => Some(frame),
            axiusflow_terminal_ui::DomUpdateOutcome::Ignored => None,
        },
        CoinbaseLevel2Outcome::Deltas { deltas, .. } => {
            let mut frame = None;
            for delta in deltas {
                match dom
                    .apply_event(&MarketEvent::DepthDelta(delta))
                    .map_err(|error| error.to_string())?
                {
                    axiusflow_terminal_ui::DomUpdateOutcome::Published(next)
                    | axiusflow_terminal_ui::DomUpdateOutcome::RecoveryRequired(next, _) => {
                        frame = Some(next);
                    }
                    axiusflow_terminal_ui::DomUpdateOutcome::Ignored => {}
                }
            }
            frame
        }
        CoinbaseLevel2Outcome::RecoveryRequired => dom.mark_stale(),
        CoinbaseLevel2Outcome::Ignored => None,
    };
    if let Some(frame) = frame {
        let _ = message_tx.send(MarketWorkerMessage::CoinbaseDom(frame));
    }
    Ok(())
}

fn establish_coinbase_stream_if_ready<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    events: &CoinbaseProviderEvents,
    streaming_generation: Option<SessionGeneration>,
) -> Result<(), String> {
    if streaming_generation.is_none() && events.has_ready() {
        match worker.try_recv_coinbase_aggregated_bar(events) {
            Ok(Some(_)) => {
                return Err("completed Coinbase bar arrived before history seeding".to_string());
            }
            Ok(None) => {}
            Err(error) if is_stale_coinbase_callback(&error) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn is_stale_coinbase_callback(error: &CoinbaseDesktopEventError) -> bool {
    matches!(
        error,
        CoinbaseDesktopEventError::Runtime(DesktopMarketWorkerError::Provider(
            DesktopProviderError::StaleGeneration
        ))
    )
}

fn request_recovery_if_required<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    events: &CoinbaseProviderEvents,
    state: &mut LiveLoopState,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let provider_state = worker.provider_state().map_err(|error| error.to_string())?;
    if let DesktopProviderState::RecoveryRequired { reason, .. } = provider_state {
        worker.reset_aggregation();
        state.streaming_generation = None;
        state.prepared = None;
        state.retained.clear();
        if !state.recovery_announced {
            message_tx
                .send(MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message: format!(
                        "Coinbase provider recovery required ({reason:?}, source={:?}); awaiting a fresh covering snapshot",
                        events.invalid_reason()
                    ),
                })
                .map_err(|()| "desktop market UI channel disconnected".to_string())?;
            state.recovery_announced = true;
        }
        if state.reconnect_backoff.retry_ready(Instant::now()) {
            worker
                .request_connection()
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
