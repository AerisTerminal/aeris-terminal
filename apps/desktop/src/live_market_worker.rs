//! Explicit direct-device Coinbase desktop composition.

mod history;
mod lifecycle;

use crate::market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup,
};
use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, MarketStreamPublication,
    Provenanced, ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
    StreamDelta, validate_provenanced_market_bar,
};
use axiusflow_chart_integration::ReplayRecoveryCommand;
use axiusflow_coinbase_market_adapter::{
    CoinbaseAggregatedBar, CoinbaseBarAggregatorConfig, CoinbaseConfig, ENTITLEMENT_CLASS,
};
use axiusflow_desktop_history::HistoryWorkerConfig;
use axiusflow_desktop_provider_runtime::{
    CoinbaseProviderDriver, CoinbaseProviderEvents, DesktopMarketWorker, DesktopMarketWorkerConfig,
    DesktopProviderConfig, DesktopProviderState, SessionGeneration,
};
use axiusflow_desktop_storage::{CatalogKey, SegmentEncryptionKey};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_platform_runtime::{
    CredentialVault, NativeCredentialVault, NetworkEvent, PowerEvent,
};
use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, ThreadId},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, Zeroizing};

use history::{
    PreparedHistory, StreamingSeriesContext, install_ready_history, prepare_initial_history,
};
use lifecycle::{
    EnvironmentalEvent, InboxDrainContext, ReconnectBackoff, WorkerInboxEvent,
    apply_initial_network, drain_worker_inbox, environment_events, forward_commands,
    wait_for_inbox,
};

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
const SUBSCRIPTION_ID: &str = "desktop_coinbase_one_minute_bars";
const VAULT_SERVICE: &str = "axiusflow-desktop-market-history";
const CATALOG_KEY_ID: &str = "history-catalog-key-v1";
const SEGMENT_KEY_ID: &str = "coinbase-public-bars-key-v1";

struct ProductProfile {
    product_id: String,
    instrument_id: String,
    symbol: String,
}

type CoinbaseDesktopWorker =
    DesktopMarketWorker<MarketBar, NativeCredentialVault, CoinbaseProviderDriver>;

struct OpenedWorker {
    worker: CoinbaseDesktopWorker,
    events: CoinbaseProviderEvents,
    segment_key: SegmentEncryptionKey,
}

struct LiveLoopState {
    prepared: Option<PreparedHistory>,
    streaming_generation: Option<SessionGeneration>,
    retained: VecDeque<ProvenancedMarketBar>,
    reconnect_backoff: ReconnectBackoff,
    recovery_announced: bool,
    pending_recovery: VecDeque<ReplayRecoveryCommand>,
}

pub(crate) fn start(
    product_id: String,
    history_root: PathBuf,
    ui_thread: ThreadId,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let profile = product_profile(product_id)?;
    let startup = loading_startup(&profile)?;
    let (message_tx, message_rx) = mpsc::sync_channel(MESSAGE_CAPACITY);
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (inbox_tx, inbox_rx) = mpsc::sync_channel(INBOX_CAPACITY);
    let provider_wake_pending = Arc::new(AtomicBool::new(false));
    let command_inbox_tx = inbox_tx.clone();
    thread::Builder::new()
        .name("axiusflow-coinbase-command-inbox".to_string())
        .spawn(move || forward_commands(&command_rx, &command_inbox_tx))
        .map_err(|error| error.to_string())?;
    thread::Builder::new()
        .name("axiusflow-coinbase-market-worker".to_string())
        .spawn(move || {
            if let Err(error) = run_worker(
                &profile,
                history_root,
                ui_thread,
                &message_tx,
                &inbox_tx,
                &inbox_rx,
                &provider_wake_pending,
            ) {
                let _ = message_tx.send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: error,
                });
            }
        })
        .map_err(|error| error.to_string())?;
    Ok((
        startup,
        MarketDataWorker::from_channels(command_tx, message_rx),
    ))
}

fn run_worker(
    profile: &ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    message_tx: &SyncSender<MarketWorkerMessage>,
    inbox_tx: &SyncSender<WorkerInboxEvent>,
    inbox_rx: &Receiver<WorkerInboxEvent>,
    provider_wake_pending: &Arc<AtomicBool>,
) -> Result<(), String> {
    let OpenedWorker {
        mut worker,
        events,
        segment_key,
    } = open_worker(
        profile,
        history_root,
        ui_thread,
        inbox_tx,
        provider_wake_pending,
    )?;
    let (initial_network, monitors_active) = environment_events(inbox_tx.clone());
    apply_initial_network(&mut worker, initial_network)?;
    let prepared = prepare_initial_history(&mut worker)?;

    let instrument = instrument(profile)?;
    let bar_definition = bar_definition();
    let worker_label = worker_label(monitors_active);
    let mut model = client_model();
    let mut state = LiveLoopState {
        prepared,
        streaming_generation: None,
        retained: VecDeque::new(),
        reconnect_backoff: ReconnectBackoff::new(),
        recovery_announced: false,
        pending_recovery: VecDeque::with_capacity(COMMAND_CAPACITY),
    };
    let mut ready_event = None;

    loop {
        if drain_worker_inbox(
            inbox_rx,
            &mut ready_event,
            &mut InboxDrainContext {
                worker: &mut worker,
                events: &events,
                state: &mut state,
                message_tx,
                provider_wake_pending,
            },
        )? {
            return Ok(());
        }

        reconcile_provider_recovery(&mut worker, &mut state, message_tx)?;

        establish_coinbase_stream_if_ready(&mut worker, &events, state.streaming_generation)?;

        if install_ready_history(
            &mut worker,
            &StreamingSeriesContext {
                profile,
                segment_key: &segment_key,
                instrument: &instrument,
                bar_definition: &bar_definition,
                worker_label: &worker_label,
            },
            &mut state,
            &mut model,
            message_tx,
        )? {
            continue;
        }

        if state.streaming_generation.is_some() {
            drain_coinbase_callbacks(
                &mut worker,
                &events,
                state.streaming_generation,
                &mut state.retained,
                &mut model,
                &worker_label,
                message_tx,
            )?;
        }

        reconcile_provider_recovery(&mut worker, &mut state, message_tx)?;

        if !publish_ready_recovery(
            state.streaming_generation,
            &mut state.pending_recovery,
            message_tx,
            (&instrument, &bar_definition),
            &state.retained,
            &mut model,
            &worker_label,
        ) {
            return Ok(());
        }
        discard_provider_events(&mut worker)?;
        if events.has_ready() {
            continue;
        }
        let recovery_required = matches!(
            worker.provider_state().map_err(|error| error.to_string())?,
            DesktopProviderState::RecoveryRequired { .. }
        );
        ready_event = wait_for_inbox(
            inbox_rx,
            state
                .reconnect_backoff
                .wait_duration(recovery_required, Instant::now()),
        )?;
    }
}

fn reconcile_provider_recovery(
    worker: &mut CoinbaseDesktopWorker,
    state: &mut LiveLoopState,
    message_tx: &SyncSender<MarketWorkerMessage>,
) -> Result<(), String> {
    request_recovery_if_required(
        worker,
        &mut state.prepared,
        &mut state.streaming_generation,
        &mut state.reconnect_backoff,
        &mut state.recovery_announced,
        &mut state.retained,
        message_tx,
    )
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
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: ChartState::Recovering,
        message: format!("Coinbase history recovery required: {error}"),
    });
    Ok(())
}

fn open_worker(
    profile: &ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    inbox_tx: &SyncSender<WorkerInboxEvent>,
    provider_wake_pending: &Arc<AtomicBool>,
) -> Result<OpenedWorker, String> {
    let vault = NativeCredentialVault::new(VAULT_SERVICE).map_err(|error| error.to_string())?;
    let catalog_key = load_catalog_key(&vault)?;
    let segment_key = load_segment_key(&vault)?;
    let runtime_vault =
        NativeCredentialVault::new(VAULT_SERVICE).map_err(|error| error.to_string())?;
    let provider_config = CoinbaseConfig::try_new(vec![profile.product_id.clone()])
        .map_err(|error| error.to_string())?;
    let provider_inbox_tx = inbox_tx.clone();
    let wake_pending = Arc::clone(provider_wake_pending);
    let wake = Arc::new(move || {
        if wake_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && provider_inbox_tx
                .try_send(WorkerInboxEvent::ProviderReady)
                .is_err()
        {
            wake_pending.store(false, Ordering::Release);
        }
    });
    let (driver, events) = CoinbaseProviderDriver::new_with_wake(
        provider_config,
        nonzero(PROVIDER_EVENT_CAPACITY),
        wake,
    );
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

fn apply_environment_event(
    worker: &mut CoinbaseDesktopWorker,
    events: &CoinbaseProviderEvents,
    event: EnvironmentalEvent,
    prepared: &mut Option<PreparedHistory>,
    streaming_generation: &mut Option<SessionGeneration>,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    message_tx: &SyncSender<MarketWorkerMessage>,
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

fn loading_startup(profile: &ProductProfile) -> Result<MarketWorkerStartup, String> {
    Ok(MarketWorkerStartup::Loading {
        instrument: instrument(profile)?,
        subscription_id: SUBSCRIPTION_ID.to_string(),
        worker_label: "Coinbase direct · loading local provider history".to_string(),
    })
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
                .send(MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message:
                        "Coinbase provider recovery required; awaiting a fresh covering snapshot"
                            .to_string(),
                })
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
    pending_recovery: &mut VecDeque<ReplayRecoveryCommand>,
    message_tx: &SyncSender<MarketWorkerMessage>,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> bool {
    while let Some(command) = pending_recovery.pop_front() {
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
    pending_recovery: &mut VecDeque<ReplayRecoveryCommand>,
    message_tx: &SyncSender<MarketWorkerMessage>,
    series: (&InstrumentRevision, &BarDefinition),
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> bool {
    streaming_generation.is_none()
        || publish_recovery_commands(
            pending_recovery,
            message_tx,
            series.0,
            series.1,
            retained,
            model,
            worker_label,
        )
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
    use super::history::history_installation_time;
    use super::{SUBSCRIPTION_ID, loading_startup, product_profile};
    use crate::market_worker::MarketWorkerStartup;

    #[test]
    fn live_mode_accepts_only_reviewed_coinbase_precision_profiles() {
        assert!(product_profile("BTC-USD".to_string()).is_ok());
        assert!(product_profile("ETH-USD".to_string()).is_ok());
        assert!(product_profile("SOL-USD".to_string()).is_err());
    }

    #[test]
    fn live_startup_is_loading_metadata_without_synthetic_market_data() {
        let profile = product_profile("BTC-USD".to_string()).expect("profile validates");
        let MarketWorkerStartup::Loading {
            instrument,
            subscription_id,
            worker_label,
        } = loading_startup(&profile).expect("loading startup validates")
        else {
            panic!("live startup must wait for a provider snapshot");
        };
        assert_eq!(instrument.instrument_id.as_str(), profile.instrument_id);
        assert_eq!(subscription_id, SUBSCRIPTION_ID);
        assert!(worker_label.contains("loading"));
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
