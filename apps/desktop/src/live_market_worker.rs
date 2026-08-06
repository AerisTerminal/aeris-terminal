//! Explicit direct-device Coinbase desktop composition.

mod composition;
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
use axiusflow_coinbase_market_adapter::CoinbaseProviderEvents;
use axiusflow_desktop_provider_runtime::{DesktopProviderState, SessionGeneration};
use axiusflow_desktop_storage::SegmentEncryptionKey;
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::BarDefinition;
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
    CoinbaseDesktopWorker, OpenedWorker, ProductProfile, bar_definition, client_model, instrument,
    loading_startup, nonzero, open_worker, product_profile, unix_nanos, worker_label,
};
use diagnostics::{diagnostics_wait_duration, flush_diagnostics};
use history::{
    PreparedHistory, StreamingSeriesContext, install_ready_history, prepare_initial_history,
};
use lifecycle::{
    EnvironmentalEvent, InboxDrainContext, ReconnectBackoff, WorkerInboxEvent,
    apply_initial_network, drain_worker_inbox, environment_events, forward_commands,
    wait_for_inbox,
};
use provenance::{history_provenance, live_provenance};
use publication::{publish_ready_recovery, publish_update};

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

struct RunningWorker {
    profile: ProductProfile,
    worker: CoinbaseDesktopWorker,
    events: CoinbaseProviderEvents,
    segment_key: SegmentEncryptionKey,
    instrument: InstrumentRevision,
    bar_definition: BarDefinition,
    worker_label: String,
    model: MarketBarClientModel,
    state: LiveLoopState,
}

pub(crate) fn start(
    product_id: String,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let profile = product_profile(product_id)?;
    let startup = loading_startup(&profile)?;
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
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
        mut worker,
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
    apply_initial_network(&mut worker, initial_network)?;
    let prepared = prepare_initial_history(&mut worker)?;
    let running = RunningWorker {
        instrument: instrument(&profile)?,
        bar_definition: bar_definition(),
        worker_label: worker_label(monitors_active),
        model: client_model(),
        state: LiveLoopState {
            prepared,
            streaming_generation: None,
            retained: VecDeque::new(),
            reconnect_backoff: ReconnectBackoff::new(),
            recovery_announced: false,
            pending_recovery: VecDeque::with_capacity(COMMAND_CAPACITY),
        },
        profile,
        worker,
        events,
        segment_key,
    };
    run_worker_loop(
        running,
        &message_tx,
        &inbox_rx,
        &provider_wake_pending,
        &ui_diagnostics_rx,
    )
}

fn run_worker_loop(
    mut running: RunningWorker,
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
            return Ok(());
        }

        flush_diagnostics(&mut running.worker, ui_diagnostics_rx, message_tx)?;

        reconcile_provider_recovery(&mut running.worker, &mut running.state, message_tx)?;

        establish_coinbase_stream_if_ready(
            &mut running.worker,
            &running.events,
            running.state.streaming_generation,
        )?;

        if install_ready_history(
            &mut running.worker,
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
                running.state.streaming_generation,
                &mut running.state.retained,
                &mut running.model,
                &running.worker_label,
                message_tx,
            )?;
        }

        reconcile_provider_recovery(&mut running.worker, &mut running.state, message_tx)?;

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

fn reconcile_provider_recovery(
    worker: &mut CoinbaseDesktopWorker,
    state: &mut LiveLoopState,
    message_tx: &MarketWorkerSender,
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
    message_tx: &MarketWorkerSender,
    error: &str,
) -> Result<(), String> {
    worker.reset_aggregation();
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

fn apply_environment_event(
    worker: &mut CoinbaseDesktopWorker,
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

fn drain_coinbase_callbacks(
    worker: &mut CoinbaseDesktopWorker,
    events: &CoinbaseProviderEvents,
    streaming_generation: Option<SessionGeneration>,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
    message_tx: &MarketWorkerSender,
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
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    if matches!(
        worker.provider_state().map_err(|error| error.to_string())?,
        DesktopProviderState::RecoveryRequired { .. }
    ) {
        worker.reset_aggregation();
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
                .map_err(|()| "desktop market UI channel disconnected".to_string())?;
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
