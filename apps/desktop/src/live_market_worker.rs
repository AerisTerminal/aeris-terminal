//! Explicit direct-device Coinbase desktop composition.

use crate::market_worker::{
    DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap, MarketWorkerMessage,
    MarketWorkerPublication,
};
use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, MarketStreamPublication,
    Provenanced, ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
    StreamDelta, validate_provenanced_market_bar,
};
use axiusflow_chart_integration::ReplayRecoveryCommand;
use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseAggregatedBar, CoinbaseBarAggregatorConfig, CoinbaseConfig,
    CoinbaseHistoryCapabilityAdapter, ENTITLEMENT_CLASS, decode_history_bar,
};
use axiusflow_desktop_history::{HistoryWorkerConfig, StartupCacheState};
use axiusflow_desktop_provider_runtime::{
    CoinbaseProviderDriver, CoinbaseProviderEvents, DesktopMarketWorker, DesktopMarketWorkerConfig,
    DesktopProviderConfig, DesktopProviderState, HistoryCompletionInstall, SessionGeneration,
};
use axiusflow_desktop_storage::{
    CatalogKey, DataKind, HistoryScope, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_platform_runtime::{
    CredentialVault, NativeCredentialVault, NativeNetworkMonitor, NativePowerMonitor, NetworkEvent,
    PowerEvent,
};
use axiusflow_provider_history::{
    Completion, DataClass, HistoryPageRequest, HistoryRange, HistoryScheduler,
    ProviderHistoryAdapter, RequestInterest, RequestPriority, SchedulerConfig,
};
use std::{
    collections::VecDeque,
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread::{self, ThreadId},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, Zeroizing};

const HISTORY_BARS: usize = 300;
const MODEL_ITEM_CAPACITY: usize = 350;
const PROVIDER_EVENT_CAPACITY: usize = 16_384;
const PROVIDER_EVENT_BATCH: usize = 1_024;
const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 1;
const ENVIRONMENT_CAPACITY: usize = 8;
const PARTITION_ID: u32 = 7;
const SCHEMA_VERSION: u32 = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MINIMUM_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAXIMUM_RECONNECT_DELAY: Duration = Duration::from_secs(8);
const SUBSCRIPTION_ID: &str = "desktop_coinbase_one_minute_bars";
const VAULT_SERVICE: &str = "axiusflow-desktop-market-history";
const CATALOG_KEY_ID: &str = "history-catalog-key-v1";
const SEGMENT_KEY_ID: &str = "coinbase-public-bars-key-v1";

enum EnvironmentalEvent {
    Network(NetworkEvent),
    Power(PowerEvent),
}

struct PreparedHistory {
    identity: SegmentIdentity,
    completion: Completion,
    received_unix_nanos: i64,
}

struct ProductProfile {
    product_id: String,
    instrument_id: String,
    symbol: String,
}

struct StreamingHistoryRequest<'a> {
    generation: SessionGeneration,
    profile: &'a ProductProfile,
    segment_key: &'a SegmentEncryptionKey,
    instrument: &'a InstrumentRevision,
    bar_definition: &'a BarDefinition,
    worker_label: &'a str,
}

type CoinbaseDesktopWorker =
    DesktopMarketWorker<MarketBar, NativeCredentialVault, CoinbaseProviderDriver>;

struct OpenedWorker {
    worker: CoinbaseDesktopWorker,
    events: CoinbaseProviderEvents,
    segment_key: SegmentEncryptionKey,
}

struct ReconnectBackoff {
    delay: Duration,
    retry_at: Option<Instant>,
}

impl ReconnectBackoff {
    const fn new() -> Self {
        Self {
            delay: MINIMUM_RECONNECT_DELAY,
            retry_at: None,
        }
    }

    fn retry_ready(&mut self, now: Instant) -> bool {
        let Some(retry_at) = self.retry_at else {
            self.retry_at = Some(now + self.delay);
            return false;
        };
        if now < retry_at {
            return false;
        }
        self.retry_at = None;
        self.delay = self
            .delay
            .checked_mul(2)
            .unwrap_or(MAXIMUM_RECONNECT_DELAY)
            .min(MAXIMUM_RECONNECT_DELAY);
        true
    }

    fn reset(&mut self) {
        self.delay = MINIMUM_RECONNECT_DELAY;
        self.retry_at = None;
    }
}

pub(crate) fn start(
    product_id: String,
    history_root: PathBuf,
    ui_thread: ThreadId,
) -> Result<(MarketWorkerBootstrap, MarketDataWorker), String> {
    let profile = product_profile(product_id)?;
    let placeholder = placeholder_snapshot(&profile)?;
    let mut model = client_model();
    let generation = published_generation(
        &mut model,
        ReplayStreamUpdate::Snapshot(placeholder.clone()),
    )?;
    let bootstrap = MarketWorkerBootstrap {
        snapshot: placeholder,
        subscription_id: SUBSCRIPTION_ID.to_string(),
        generation,
        worker_label: "Coinbase direct · loading local provider history".to_string(),
    };
    let (message_tx, message_rx) = mpsc::sync_channel(MESSAGE_CAPACITY);
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let thread_placeholder = bootstrap.snapshot.clone();
    thread::Builder::new()
        .name("axiusflow-coinbase-market-worker".to_string())
        .spawn(move || {
            if let Err(error) = run_worker(
                &profile,
                history_root,
                ui_thread,
                thread_placeholder,
                &message_tx,
                &command_rx,
            ) {
                let _ = message_tx.send(MarketWorkerMessage::Failed(error));
            }
        })
        .map_err(|error| error.to_string())?;
    Ok((
        bootstrap,
        MarketDataWorker::from_channels(command_tx, message_rx),
    ))
}

fn run_worker(
    profile: &ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    placeholder: ReplaySnapshot,
    message_tx: &SyncSender<MarketWorkerMessage>,
    command_rx: &Receiver<ReplayRecoveryCommand>,
) -> Result<(), String> {
    let OpenedWorker {
        mut worker,
        events,
        segment_key,
    } = open_worker(profile, history_root, ui_thread)?;
    let (environment_rx, monitors_active) = environment_events();
    drain_initial_environment(&mut worker, &environment_rx)?;
    let mut prepared = prepare_initial_history(&mut worker)?;

    let instrument = instrument(profile)?;
    let bar_definition = bar_definition();
    let worker_label = worker_label(monitors_active);
    let mut model = client_model();
    published_generation(&mut model, ReplayStreamUpdate::Snapshot(placeholder))?;
    let mut retained = VecDeque::<ProvenancedMarketBar>::new();
    let mut streaming_generation = None;
    let mut reconnect_backoff = ReconnectBackoff::new();
    let mut recovery_announced = false;

    loop {
        apply_environment_events(
            &mut worker,
            &events,
            &environment_rx,
            &mut prepared,
            &mut streaming_generation,
            &mut retained,
            message_tx,
        )?;

        request_recovery_if_required(
            &mut worker,
            &mut prepared,
            &mut streaming_generation,
            &mut reconnect_backoff,
            &mut recovery_announced,
            &mut retained,
            message_tx,
        )?;

        establish_coinbase_stream_if_ready(&mut worker, &events, streaming_generation)?;

        if streaming_generation.is_none()
            && let DesktopProviderState::Streaming { generation } =
                worker.provider_state().map_err(|error| error.to_string())?
        {
            let history = install_streaming_history(
                &mut worker,
                &StreamingHistoryRequest {
                    generation,
                    profile,
                    segment_key: &segment_key,
                    instrument: &instrument,
                    bar_definition: &bar_definition,
                    worker_label: &worker_label,
                },
                &mut prepared,
                &mut model,
                message_tx,
            );
            match history {
                Ok(history) => {
                    retained = history;
                    streaming_generation = Some(generation);
                    reconnect_backoff.reset();
                    recovery_announced = false;
                }
                Err(error) => {
                    fence_failed_history(
                        &mut worker,
                        generation,
                        &mut retained,
                        &mut recovery_announced,
                        message_tx,
                        &error,
                    )?;
                    continue;
                }
            }
        }

        if streaming_generation.is_some() {
            drain_coinbase_callbacks(
                &mut worker,
                &events,
                streaming_generation,
                &mut retained,
                &mut model,
                &worker_label,
                message_tx,
            )?;
        }

        if !publish_ready_recovery(
            streaming_generation,
            command_rx,
            message_tx,
            (&instrument, &bar_definition),
            &retained,
            &mut model,
            &worker_label,
        ) {
            return Ok(());
        }
        discard_provider_events(&mut worker)?;
        thread::sleep(POLL_INTERVAL);
    }
}

fn worker_label(monitors_active: bool) -> String {
    if monitors_active {
        "Coinbase direct · native lifecycle monitored"
    } else {
        "Coinbase direct · native lifecycle monitor unavailable"
    }
    .to_string()
}

fn discard_provider_events(worker: &mut CoinbaseDesktopWorker) -> Result<(), String> {
    while worker
        .try_recv_provider_event()
        .map_err(|error| error.to_string())?
        .is_some()
    {}
    Ok(())
}

fn fence_failed_history(
    worker: &mut CoinbaseDesktopWorker,
    generation: SessionGeneration,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    recovery_announced: &mut bool,
    message_tx: &SyncSender<MarketWorkerMessage>,
    error: &str,
) -> Result<(), String> {
    worker
        .session_invalid(generation)
        .map_err(|failure| failure.to_string())?;
    retained.clear();
    *recovery_announced = true;
    let _ = message_tx.send(MarketWorkerMessage::Failed(format!(
        "Coinbase history recovery required: {error}"
    )));
    Ok(())
}

fn open_worker(
    profile: &ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
) -> Result<OpenedWorker, String> {
    let vault = NativeCredentialVault::new(VAULT_SERVICE).map_err(|error| error.to_string())?;
    let catalog_key = load_catalog_key(&vault)?;
    let segment_key = load_segment_key(&vault)?;
    let runtime_vault =
        NativeCredentialVault::new(VAULT_SERVICE).map_err(|error| error.to_string())?;
    let provider_config = CoinbaseConfig::try_new(vec![profile.product_id.clone()])
        .map_err(|error| error.to_string())?;
    let (driver, events) =
        CoinbaseProviderDriver::new(provider_config, nonzero(PROVIDER_EVENT_CAPACITY));
    let mut worker = DesktopMarketWorker::try_open(
        runtime_vault,
        driver,
        "coinbase-public-session",
        history_root,
        catalog_key,
        ui_thread,
        worker_config(),
    )
    .map_err(|error| error.to_string())?;
    worker
        .register_coinbase_bar_product(
            CoinbaseBarAggregatorConfig::try_new(
                profile.product_id.clone(),
                2,
                8,
                nonzero(MODEL_ITEM_CAPACITY),
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    Ok(OpenedWorker {
        worker,
        events,
        segment_key,
    })
}

fn install_streaming_history(
    worker: &mut CoinbaseDesktopWorker,
    request: &StreamingHistoryRequest<'_>,
    prepared: &mut Option<PreparedHistory>,
    model: &mut MarketBarClientModel,
    message_tx: &SyncSender<MarketWorkerMessage>,
) -> Result<VecDeque<ProvenancedMarketBar>, String> {
    let history = match prepared.take() {
        Some(history) => history,
        None => fetch_history(request.profile)?,
    };
    install_history(
        worker,
        request.generation,
        request.profile,
        &history,
        request.segment_key,
    )?;
    let bars = worker
        .coinbase_bar_history(&request.profile.product_id)
        .map_err(|error| error.to_string())?;
    let retained = bars
        .into_iter()
        .map(|bar| history_provenance(bar, request.generation, history.received_unix_nanos))
        .collect::<Result<VecDeque<_>, _>>()?;
    let snapshot_generation = model
        .current_generation()
        .map_or(1, |current| current.generation().saturating_add(1));
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        request.instrument.clone(),
        ReplayProvenance::LiveProvider,
        request.bar_definition.clone(),
        snapshot_generation,
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    publish_update(
        worker,
        request.generation,
        model,
        ReplayStreamUpdate::Snapshot(snapshot),
        request.worker_label,
        message_tx,
    )?;
    Ok(retained)
}

fn apply_environment_events(
    worker: &mut CoinbaseDesktopWorker,
    events: &CoinbaseProviderEvents,
    environment_rx: &Receiver<EnvironmentalEvent>,
    prepared: &mut Option<PreparedHistory>,
    streaming_generation: &mut Option<SessionGeneration>,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    message_tx: &SyncSender<MarketWorkerMessage>,
) -> Result<(), String> {
    while let Ok(event) = environment_rx.try_recv() {
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
        if next.is_some() {
            *prepared = None;
        }
        *streaming_generation = None;
        retained.clear();
        let _ = message_tx.send(MarketWorkerMessage::Failed(
            "direct provider lifecycle changed; a fresh snapshot is required".to_string(),
        ));
    }
    Ok(())
}

fn discard_coinbase_callbacks(events: &CoinbaseProviderEvents) {
    while events.try_recv().is_some() {}
}

fn drain_coinbase_callbacks(
    worker: &mut CoinbaseDesktopWorker,
    events: &CoinbaseProviderEvents,
    streaming_generation: Option<SessionGeneration>,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
    message_tx: &SyncSender<MarketWorkerMessage>,
) -> Result<(), String> {
    for _ in 0..PROVIDER_EVENT_BATCH {
        if !events.has_ready() {
            break;
        }
        let received = match worker.try_recv_coinbase_aggregated_bar(events) {
            Ok(received) => received,
            Err(_error)
                if matches!(
                    worker.provider_state().map_err(|error| error.to_string())?,
                    DesktopProviderState::RecoveryRequired { .. }
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        let Some((generation, completed)) = received else {
            continue;
        };
        if streaming_generation != Some(generation) {
            return Err("completed Coinbase bar arrived before history seeding".to_string());
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

fn establish_coinbase_stream_if_ready(
    worker: &mut CoinbaseDesktopWorker,
    events: &CoinbaseProviderEvents,
    streaming_generation: Option<SessionGeneration>,
) -> Result<(), String> {
    if streaming_generation.is_none()
        && events.has_ready()
        && worker
            .try_recv_coinbase_aggregated_bar(events)
            .map_err(|error| error.to_string())?
            .is_some()
    {
        return Err("completed Coinbase bar arrived before history seeding".to_string());
    }
    Ok(())
}

fn prepare_initial_history(
    worker: &mut CoinbaseDesktopWorker,
) -> Result<Option<PreparedHistory>, String> {
    worker
        .request_connection()
        .map_err(|error| error.to_string())?;
    Ok(None)
}

fn request_recovery_if_required(
    worker: &mut CoinbaseDesktopWorker,
    prepared: &mut Option<PreparedHistory>,
    streaming_generation: &mut Option<SessionGeneration>,
    reconnect_backoff: &mut ReconnectBackoff,
    recovery_announced: &mut bool,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    message_tx: &SyncSender<MarketWorkerMessage>,
) -> Result<(), String> {
    if matches!(
        worker.provider_state().map_err(|error| error.to_string())?,
        DesktopProviderState::RecoveryRequired { .. }
    ) {
        *streaming_generation = None;
        *prepared = None;
        retained.clear();
        if !*recovery_announced {
            message_tx
                .send(MarketWorkerMessage::Failed(
                    "Coinbase provider recovery required; awaiting a fresh covering snapshot"
                        .to_string(),
                ))
                .map_err(|_| "desktop market UI channel disconnected".to_string())?;
            *recovery_announced = true;
        }
        if reconnect_backoff.retry_ready(Instant::now()) {
            worker
                .request_connection()
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn publish_recovery_commands(
    command_rx: &Receiver<ReplayRecoveryCommand>,
    message_tx: &SyncSender<MarketWorkerMessage>,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> bool {
    while let Ok(command) = command_rx.try_recv() {
        let result = recovery_snapshot(instrument, bar_definition, retained, model, worker_label);
        if message_tx
            .send(MarketWorkerMessage::Recovery {
                request_id: command.request_id,
                result,
            })
            .is_err()
        {
            return false;
        }
    }
    true
}

fn publish_ready_recovery(
    streaming_generation: Option<SessionGeneration>,
    command_rx: &Receiver<ReplayRecoveryCommand>,
    message_tx: &SyncSender<MarketWorkerMessage>,
    series: (&InstrumentRevision, &BarDefinition),
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> bool {
    streaming_generation.is_none()
        || publish_recovery_commands(
            command_rx,
            message_tx,
            series.0,
            series.1,
            retained,
            model,
            worker_label,
        )
}

fn install_history(
    worker: &mut CoinbaseDesktopWorker,
    generation: SessionGeneration,
    profile: &ProductProfile,
    history: &PreparedHistory,
    segment_key: &SegmentEncryptionKey,
) -> Result<(), String> {
    let interest = RequestInterest::new(NonZeroU64::MIN);
    let now_seconds =
        history_installation_time(history.identity.range_end_unix_nanos, unix_nanos()?)?;
    let binding = worker
        .begin_scheduled_history_handoff(
            generation,
            history.identity.clone(),
            interest,
            segment_key,
            now_seconds,
        )
        .map_err(|error| error.to_string())?;
    worker
        .install_history_completion(
            generation,
            &history.identity,
            &history.completion,
            HistoryCompletionInstall {
                binding,
                snapshot_generation: NonZeroU64::new(generation.get()).unwrap_or(NonZeroU64::MIN),
                empty_cutover_watermark: None,
                startup_cache_state: StartupCacheState::Cold,
            },
            |_| Ok(size_of::<MarketBar>()),
            |item, _| decode_history_bar(item).map(|bar| (bar, size_of::<MarketBar>())),
        )
        .map_err(|error| error.to_string())?;
    worker
        .seed_coinbase_bar_history(
            generation,
            &profile.product_id,
            &history.identity,
            segment_key,
            now_seconds,
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn history_installation_time(
    range_end_unix_nanos: i64,
    now_unix_nanos: i64,
) -> Result<i64, String> {
    let current_minute = now_unix_nanos.div_euclid(60_000_000_000) * 60_000_000_000;
    if range_end_unix_nanos != current_minute {
        return Err("Coinbase history became stale before installation".to_string());
    }
    Ok(now_unix_nanos / 1_000_000_000)
}

fn fetch_history(profile: &ProductProfile) -> Result<PreparedHistory, String> {
    let now = unix_nanos()?;
    let minute_nanos = 60_000_000_000_i64;
    let end = now.div_euclid(minute_nanos) * minute_nanos;
    let start = end
        .checked_sub(minute_nanos * i64::try_from(HISTORY_BARS).map_err(|error| error.to_string())?)
        .ok_or_else(|| "Coinbase history range underflow".to_string())?;
    let identity = history_identity(profile, start, end);
    let request = HistoryPageRequest {
        provider_id: "coinbase".to_string(),
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: ENTITLEMENT_CLASS.to_string(),
        instrument_id: profile.instrument_id.clone(),
        data_class: DataClass::Bars,
        resolution: "1m".to_string(),
        range: HistoryRange {
            start_unix_nanos: start,
            end_unix_nanos: end,
        },
        maximum_items: nonzero(HISTORY_BARS),
        continuation: None,
    };
    let mut adapter =
        CoinbaseHistoryCapabilityAdapter::try_new().map_err(|error| error.to_string())?;
    let mut scheduler = HistoryScheduler::try_new(
        adapter.capabilities().clone(),
        SchedulerConfig {
            maximum_queued_requests: NonZeroUsize::MIN,
            maximum_total_inflight: NonZeroUsize::MIN,
            maximum_interests_per_request: NonZeroUsize::MIN,
            maximum_continuations_per_request: NonZeroUsize::MIN,
            maximum_fetch_attempts: NonZeroUsize::MIN,
            adjacent_prefetch_windows: 0,
        },
    )
    .map_err(|error| error.to_string())?;
    let interest = RequestInterest::new(NonZeroU64::MIN);
    scheduler
        .submit(request, interest, RequestPriority::Visible, now)
        .map_err(|error| error.to_string())?;
    let dispatch = scheduler
        .dispatch_next(now, 0)
        .map_err(|error| error.to_string())?
        .dispatch
        .ok_or_else(|| "Coinbase history request was not dispatchable".to_string())?;
    let page = adapter.fetch_page(&dispatch.request)?;
    if page.items.is_empty() {
        return Err("Coinbase returned no completed history bars".to_string());
    }
    let received_unix_nanos = unix_nanos()?;
    let completion = scheduler
        .complete(dispatch.dispatch_id, page, received_unix_nanos)
        .map_err(|error| error.to_string())?;
    Ok(PreparedHistory {
        identity,
        completion,
        received_unix_nanos,
    })
}

fn publish_update(
    worker: &mut CoinbaseDesktopWorker,
    provider_generation: SessionGeneration,
    model: &mut MarketBarClientModel,
    update: ReplayStreamUpdate,
    worker_label: &str,
    message_tx: &SyncSender<MarketWorkerMessage>,
) -> Result<(), String> {
    let generation = published_generation(model, update.clone())?;
    let publication = MarketStreamPublication::try_new(
        SUBSCRIPTION_ID.to_string(),
        update.clone(),
        generation.clone(),
    )
    .map_err(|error| error.to_string())?;
    worker
        .publish(provider_generation, publication)
        .map_err(|error| error.to_string())?;
    message_tx
        .send(MarketWorkerMessage::Update(MarketWorkerPublication {
            update,
            generation,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: worker_label.to_string(),
        }))
        .map_err(|_| "desktop market UI channel disconnected".to_string())
}

fn recovery_snapshot(
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> Result<MarketWorkerBootstrap, String> {
    let generation = model
        .current_generation()
        .map_or(1, |current| current.generation().saturating_add(1));
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        instrument.clone(),
        ReplayProvenance::LiveProvider,
        bar_definition.clone(),
        generation,
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    let model_generation =
        published_generation(model, ReplayStreamUpdate::Snapshot(snapshot.clone()))?;
    Ok(MarketWorkerBootstrap {
        snapshot,
        subscription_id: SUBSCRIPTION_ID.to_string(),
        generation: model_generation,
        worker_label: worker_label.to_string(),
    })
}

fn published_generation(
    model: &mut MarketBarClientModel,
    update: ReplayStreamUpdate,
) -> Result<DesktopMarketGeneration, String> {
    match model
        .apply_update(update)
        .map_err(|error| error.to_string())?
    {
        MarketBarModelOutcome::Published(generation) => Ok(generation),
        outcome => Err(format!(
            "desktop market update was not published: {outcome:?}"
        )),
    }
}

fn history_provenance(
    bar: MarketBar,
    generation: SessionGeneration,
    received_unix_nanos: i64,
) -> Result<ProvenancedMarketBar, String> {
    let exchange = bar
        .exchange_timestamp_seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase history timestamp overflow".to_string())?;
    provenanced(
        bar,
        generation,
        format!("coinbase_history_bar_{}_{exchange}", bar.source_sequence),
        exchange,
        received_unix_nanos,
        received_unix_nanos,
        "coinbase_https_history".to_string(),
    )
}

fn live_provenance(
    completed: CoinbaseAggregatedBar,
    generation: SessionGeneration,
) -> Result<ProvenancedMarketBar, String> {
    let provider_sequence_num = completed
        .provider_sequence_num
        .ok_or_else(|| "Coinbase completed bar has no live provider sequence".to_string())?;
    let provider_timestamp_unix_nanos = completed
        .provider_timestamp_unix_nanos
        .ok_or_else(|| "Coinbase completed bar has no live provider timestamp".to_string())?;
    let received = unix_nanos()?;
    let exchange = completed
        .bar
        .exchange_timestamp_seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase live bar timestamp overflow".to_string())?;
    provenanced(
        completed.bar,
        generation,
        format!(
            "coinbase_live_bar_{}_message_{}",
            completed.bar.source_sequence, provider_sequence_num
        ),
        exchange,
        provider_timestamp_unix_nanos,
        received,
        format!("coinbase_message_sequence_{provider_sequence_num}"),
    )
}

fn provenanced(
    bar: MarketBar,
    generation: SessionGeneration,
    event_id: String,
    event_time_unix_nanos: i64,
    provider_receive_timestamp_unix_nanos: i64,
    received_unix_nanos: i64,
    causation_id: String,
) -> Result<ProvenancedMarketBar, String> {
    let exchange_timestamp_unix_nanos =
        bar.exchange_timestamp_seconds
            .checked_mul(1_000_000_000)
            .ok_or_else(|| "Coinbase bar timestamp overflow".to_string())?;
    let item = Provenanced::new(
        bar,
        MarketEventProvenance {
            event_id,
            event_time_unix_nanos,
            publication_time_unix_nanos: received_unix_nanos,
            producer: "axiusflow_desktop_coinbase_worker".to_string(),
            schema_version: SCHEMA_VERSION,
            correlation_id: format!("coinbase_generation_{}", generation.get()),
            causation_id,
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            partition_id: PARTITION_ID,
            ownership_epoch: generation.get().saturating_add(1),
            source_id: "coinbase".to_string(),
            source_sequence: bar.source_sequence,
            exchange_timestamp_unix_nanos,
            provider_receive_timestamp_unix_nanos,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: received_unix_nanos,
            normalized_timestamp_unix_nanos: received_unix_nanos,
            fanout_enqueue_timestamp_unix_nanos: None,
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        },
    );
    validate_provenanced_market_bar(&item).map_err(|error| error.to_string())?;
    Ok(item)
}

fn placeholder_snapshot(profile: &ProductProfile) -> Result<ReplaySnapshot, String> {
    let bar = MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: 60,
        open: 1,
        high: 1,
        low: 1,
        close: 1,
        volume: 0,
    };
    let item = Provenanced::new(
        bar,
        MarketEventProvenance {
            event_id: "desktop_coinbase_loading_placeholder".to_string(),
            event_time_unix_nanos: 60_000_000_000,
            publication_time_unix_nanos: 60_000_000_000,
            producer: "desktop_loading_fixture".to_string(),
            schema_version: SCHEMA_VERSION,
            correlation_id: "desktop_coinbase_startup".to_string(),
            causation_id: String::new(),
            entitlement_revision: "loading_fixture_v1".to_string(),
            partition_id: PARTITION_ID,
            ownership_epoch: 1,
            source_id: "desktop_loading_fixture".to_string(),
            source_sequence: 1,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            provider_receive_timestamp_unix_nanos: 60_000_000_000,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: 60_000_000_000,
            normalized_timestamp_unix_nanos: 60_000_000_000,
            fanout_enqueue_timestamp_unix_nanos: None,
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 1,
        },
    );
    ReplaySnapshot::try_from_provenanced_values(
        instrument(profile)?,
        ReplayProvenance::EmbeddedFixture,
        bar_definition(),
        1,
        vec![item],
    )
    .map_err(|error| error.to_string())
}

fn product_profile(product_id: String) -> Result<ProductProfile, String> {
    let (base, instrument_id) = match product_id.as_str() {
        "BTC-USD" => ("BTC", "instrument:coinbase:btc:usd"),
        "ETH-USD" => ("ETH", "instrument:coinbase:eth:usd"),
        _ => return Err("Coinbase desktop mode supports only BTC-USD and ETH-USD".to_string()),
    };
    Ok(ProductProfile {
        product_id,
        instrument_id: instrument_id.to_string(),
        symbol: format!("{base}/USD"),
    })
}

fn instrument(profile: &ProductProfile) -> Result<InstrumentRevision, String> {
    Ok(InstrumentRevision {
        instrument_id: InstrumentId::try_new(profile.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: 1,
        asset_class: AssetClass::CryptoAsset,
        symbol: profile.symbol.clone(),
        venue_id: "COINBASE".to_string(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(2, 8).map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    })
}

fn bar_definition() -> BarDefinition {
    BarDefinition {
        definition_id: "coinbase:spot:one_minute:unadjusted:v1".to_string(),
        version: 1,
        interval_seconds: 60,
    }
}

fn history_identity(profile: &ProductProfile, start: i64, end: i64) -> SegmentIdentity {
    SegmentIdentity {
        scope: HistoryScope {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
        },
        instrument_id: profile.instrument_id.clone(),
        data_kind: DataKind::Bars,
        resolution: "1m".to_string(),
        range_start_unix_nanos: start,
        range_end_unix_nanos: end,
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn worker_config() -> DesktopMarketWorkerConfig {
    DesktopMarketWorkerConfig {
        provider: DesktopProviderConfig::new(nonzero(32), nonzero(1)),
        history: HistoryWorkerConfig {
            maximum_cache_entries: nonzero(2),
            maximum_decoded_bytes: nonzero(2 * 1024 * 1024),
            maximum_charts: nonzero(1),
            maximum_segment_read_bytes: nonzero(1024 * 1024),
            maximum_buffered_live: nonzero(512),
            maximum_handoffs: nonzero(1),
        },
        maximum_catalog_entries: 64,
    }
}

fn client_model() -> MarketBarClientModel {
    MarketBarClientModel::new(nonzero(MODEL_ITEM_CAPACITY))
}

fn load_catalog_key(vault: &NativeCredentialVault) -> Result<CatalogKey, String> {
    let bytes = load_or_create_key(vault, CATALOG_KEY_ID)?;
    CatalogKey::try_new(CATALOG_KEY_ID.to_string(), bytes).map_err(|error| error.to_string())
}

fn load_segment_key(vault: &NativeCredentialVault) -> Result<SegmentEncryptionKey, String> {
    let bytes = load_or_create_key(vault, SEGMENT_KEY_ID)?;
    SegmentEncryptionKey::try_new(SEGMENT_KEY_ID.to_string(), bytes)
        .map_err(|error| error.to_string())
}

fn load_or_create_key(vault: &NativeCredentialVault, key_id: &str) -> Result<[u8; 32], String> {
    if let Some(mut stored) = vault.load(key_id).map_err(|error| error.to_string())? {
        let result = <[u8; 32]>::try_from(stored.as_slice())
            .map_err(|_| "operating-system vault key has an invalid length".to_string());
        stored.zeroize();
        return result;
    }
    let mut generated = Zeroizing::new([0_u8; 32]);
    getrandom::fill(generated.as_mut()).map_err(|error| error.to_string())?;
    vault
        .store(key_id, generated.as_ref())
        .map_err(|error| error.to_string())?;
    let mut stored = vault
        .load(key_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "operating-system vault did not retain the generated key".to_string())?;
    let result = <[u8; 32]>::try_from(stored.as_slice())
        .map_err(|_| "operating-system vault key has an invalid length".to_string())?;
    stored.zeroize();
    Ok(result)
}

fn environment_events() -> (Receiver<EnvironmentalEvent>, bool) {
    let (sender, receiver) = mpsc::sync_channel(ENVIRONMENT_CAPACITY);
    let network = NativeNetworkMonitor::connect().ok();
    let power = NativePowerMonitor::connect().ok();
    let network_active = if let Some(mut monitor) = network {
        let initial = monitor.current();
        let network_sender = sender.clone();
        if network_sender
            .send(EnvironmentalEvent::Network(initial))
            .is_ok()
        {
            thread::Builder::new()
                .name("axiusflow-network-monitor".to_string())
                .spawn(move || {
                    while let Ok(event) = monitor.next_event() {
                        if network_sender
                            .send(EnvironmentalEvent::Network(event))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .is_ok()
        } else {
            false
        }
    } else {
        false
    };
    let power_active = if let Some(mut monitor) = power {
        thread::Builder::new()
            .name("axiusflow-power-monitor".to_string())
            .spawn(move || {
                while let Ok(event) = monitor.next_event() {
                    if sender.send(EnvironmentalEvent::Power(event)).is_err() {
                        break;
                    }
                }
            })
            .is_ok()
    } else {
        false
    };
    (receiver, network_active && power_active)
}

fn drain_initial_environment(
    worker: &mut CoinbaseDesktopWorker,
    receiver: &Receiver<EnvironmentalEvent>,
) -> Result<(), String> {
    loop {
        match receiver.try_recv() {
            Ok(EnvironmentalEvent::Network(NetworkEvent::Unavailable)) => {
                worker
                    .handle_network_event(NetworkEvent::Unavailable)
                    .map_err(|error| error.to_string())?;
            }
            Ok(EnvironmentalEvent::Network(NetworkEvent::Available)) => {}
            Ok(EnvironmentalEvent::Power(event)) => {
                worker
                    .handle_power_event(event)
                    .map_err(|error| error.to_string())?;
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

fn unix_nanos() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos()
        .try_into()
        .map_err(|_| "system time exceeds the supported range".to_string())
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MINIMUM_RECONNECT_DELAY, ReconnectBackoff, bar_definition, history_installation_time,
        placeholder_snapshot, product_profile,
    };
    use axiusflow_application::ReplayProvenance;
    use std::time::Instant;

    #[test]
    fn live_mode_accepts_only_reviewed_coinbase_precision_profiles() {
        assert!(product_profile("BTC-USD".to_string()).is_ok());
        assert!(product_profile("ETH-USD".to_string()).is_ok());
        assert!(product_profile("SOL-USD".to_string()).is_err());
    }

    #[test]
    fn startup_placeholder_is_explicitly_non_live_and_series_compatible() {
        let profile = product_profile("BTC-USD".to_string()).expect("profile validates");
        let snapshot = placeholder_snapshot(&profile).expect("placeholder validates");
        assert_eq!(snapshot.provenance(), ReplayProvenance::EmbeddedFixture);
        assert_eq!(
            snapshot.instrument().instrument_id.as_str(),
            profile.instrument_id
        );
        assert_eq!(snapshot.bar_definition(), &bar_definition());
    }

    #[test]
    fn reconnect_backoff_delays_and_resets_attempts() {
        let started = Instant::now();
        let mut backoff = ReconnectBackoff::new();
        assert!(!backoff.retry_ready(started));
        assert!(!backoff.retry_ready(started + MINIMUM_RECONNECT_DELAY / 2));
        assert!(backoff.retry_ready(started + MINIMUM_RECONNECT_DELAY));
        assert_eq!(backoff.delay, MINIMUM_RECONNECT_DELAY * 2);

        backoff.reset();
        assert_eq!(backoff.delay, MINIMUM_RECONNECT_DELAY);
        assert_eq!(backoff.retry_at, None);
    }

    #[test]
    fn history_installation_rejects_a_crossed_minute_boundary() {
        let requested_end = 120_000_000_000;
        assert_eq!(
            history_installation_time(requested_end, requested_end + 30_000_000_000),
            Ok(150)
        );
        assert!(history_installation_time(requested_end, requested_end + 60_000_000_000).is_err());
    }
}
