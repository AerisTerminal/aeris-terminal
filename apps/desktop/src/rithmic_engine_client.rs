use crate::{
    resident_market_worker::{
        EngineSeriesRequest, MarketDataWorker, MarketWorkerCommand, MarketWorkerMessage,
        MarketWorkerSender, MarketWorkerStartup, ProviderCatalogCommand, ProviderCatalogEvent,
        market_worker_channel,
    },
    rithmic_engine_history::{RithmicHistoryTask, history_message, validate_engine_instrument},
    rithmic_history::{RithmicSeries, RithmicSeriesRequest},
};
use axiusflow_engine_protocol::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderInstrumentSearchResult, ProviderInstrumentSelection, SearchProviderInstruments,
    SelectProviderInstrument, envelope,
};
use axiusflow_observability::FeedConnectionState;
use std::{
    collections::BTreeSet,
    num::{NonZeroU64, NonZeroUsize},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::Duration,
};

use crate::engine_supervisor::EngineSupervisor;

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const ENGINE_WORKSPACE_ID: u64 = 1;

struct WorkerState {
    catalog: EngineCatalogSession,
    history: RithmicHistoryTask,
    installed: Option<InstallProviderInstrument>,
    pending_history: Option<RithmicSeriesRequest>,
    pending_searches: BTreeSet<u64>,
    pending_selections: BTreeSet<u64>,
}

/// Starts the bounded Rithmic UI bridge backed exclusively by the resident engine.
///
/// # Errors
/// Returns a redacted engine, shell, or worker-start failure.
pub fn start() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    spawn_worker(|messages, commands| run(&messages, &commands))
}

fn spawn_worker(
    task: impl FnOnce(MarketWorkerSender, Receiver<MarketWorkerCommand>) + Send + 'static,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("axiusflow-rithmic-engine-client".to_string())
        .spawn(move || {
            task(message_tx, command_rx);
            let _ = shutdown_tx.send(());
        })
        .map_err(|_| "Rithmic engine client thread is unavailable".to_string())?;

    Ok((
        MarketWorkerStartup::Rithmic,
        MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None),
    ))
}

fn run(messages: &MarketWorkerSender, commands: &Receiver<MarketWorkerCommand>) {
    send_connection(
        messages,
        FeedConnectionState::Discovering,
        "connecting to the resident Rithmic engine",
    );
    let state = EngineCatalogSession::connect().and_then(|catalog| {
        RithmicHistoryTask::start().map(|history| WorkerState {
            catalog,
            history,
            installed: None,
            pending_history: None,
            pending_searches: BTreeSet::new(),
            pending_selections: BTreeSet::new(),
        })
    });
    let Ok(mut state) = state else {
        send_connection(
            messages,
            FeedConnectionState::Recovering,
            "resident Rithmic engine is unavailable",
        );
        wait_for_shutdown(commands);
        return;
    };
    send_connection(
        messages,
        FeedConnectionState::Authenticating,
        "resident engine owns the Rithmic catalog session",
    );

    loop {
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(MarketWorkerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(command) => process_command(messages, &mut state, command),
            Err(RecvTimeoutError::Timeout) => {}
        }
        match state.catalog.poll() {
            Ok(poll) if poll.reconnected => {
                send_connection(
                    messages,
                    FeedConnectionState::Recovering,
                    "resident Rithmic engine restarted; restoring catalog demand",
                );
            }
            Ok(poll) => {
                if let Some(event) = poll.event {
                    handle_catalog_event(messages, &mut state, event);
                }
            }
            Err(_) => {
                send_connection(
                    messages,
                    FeedConnectionState::Recovering,
                    "resident Rithmic catalog connection is recovering",
                );
                break;
            }
        }
        if let Some(frame) = state.history.try_recv_dom() {
            let _ = messages.send(MarketWorkerMessage::RithmicDom(frame));
        }
        if let Some(result) = state.history.try_recv() {
            let current = state.pending_history.is_some_and(|request| {
                request.selection_generation == result.selection_generation
                    && request.series_generation == result.series_generation
            });
            if current {
                if result.result.is_err() {
                    state.pending_history = None;
                }
                let _ = messages.send(history_message(result));
            }
        }
    }
    send_connection(
        messages,
        FeedConnectionState::Stopped,
        "Rithmic engine client stopped",
    );
}

fn process_command(
    messages: &MarketWorkerSender,
    state: &mut WorkerState,
    command: MarketWorkerCommand,
) {
    match command {
        MarketWorkerCommand::ProviderSearch(search) => {
            let generation = search.search_generation;
            if state.catalog.search(&search).is_ok() {
                state.pending_searches.insert(generation);
            } else {
                publish_dispatch_rejection(messages, generation, ProviderCatalogCommand::Search);
            }
        }
        MarketWorkerCommand::ProviderSelect(selection) => {
            let generation = selection.selection_generation;
            state.history.cancel();
            state.pending_history = None;
            if state.catalog.select(&selection).is_ok() {
                state.pending_selections.insert(generation);
            } else {
                publish_dispatch_rejection(messages, generation, ProviderCatalogCommand::Selection);
            }
        }
        MarketWorkerCommand::EngineSeries(EngineSeriesRequest {
            selection_generation,
            series_generation,
            interval,
        }) => {
            let request = RithmicSeriesRequest {
                selection_generation,
                series_generation,
                series: RithmicSeries::from(interval),
            };
            let result = state
                .installed
                .clone()
                .ok_or_else(|| "Rithmic instrument selection is unavailable".to_string())
                .and_then(|instrument| state.history.request(request, instrument));
            if result.is_ok() {
                state.pending_history = Some(request);
            } else {
                let _ = messages.send(MarketWorkerMessage::RithmicHistory {
                    selection_generation: request.selection_generation,
                    series_generation: request.series_generation,
                    result: Err(result
                        .err()
                        .unwrap_or_else(|| "Rithmic history is unavailable".to_string())),
                });
            }
        }
        MarketWorkerCommand::Visibility(visible) => {
            let _ = state.history.set_visibility(visible);
        }
        MarketWorkerCommand::Shutdown
        | MarketWorkerCommand::Recovery(_)
        | MarketWorkerCommand::CoinbaseSelect(_)
        | MarketWorkerCommand::ChartViewport(_) => {}
    }
}

fn handle_catalog_event(
    messages: &MarketWorkerSender,
    state: &mut WorkerState,
    event: envelope::Payload,
) {
    match event {
        envelope::Payload::ProviderInstrumentSearchResult(result) => {
            publish_search_result(messages, state, result);
        }
        envelope::Payload::ProviderInstrumentSelection(selection) => {
            publish_selection(messages, state, selection);
        }
        envelope::Payload::ProviderCatalogRejected(rejection) => {
            if rejection.provider != "rithmic" {
                return;
            }
            let command = if state
                .pending_selections
                .remove(&rejection.command_generation)
            {
                ProviderCatalogCommand::Selection
            } else {
                state.pending_searches.remove(&rejection.command_generation);
                ProviderCatalogCommand::Search
            };
            let _ = messages.send(MarketWorkerMessage::ProviderCatalog(
                ProviderCatalogEvent::CommandRejected { rejection, command },
            ));
            send_connection(
                messages,
                FeedConnectionState::Recovering,
                "Rithmic catalog command was rejected",
            );
        }
        envelope::Payload::ProviderState(provider) if provider.provider == "rithmic" => {}
        envelope::Payload::Fault(fault) => {
            send_connection(
                messages,
                FeedConnectionState::Recovering,
                &fault.redacted_detail,
            );
        }
        _ => {}
    }
}

fn publish_search_result(
    messages: &MarketWorkerSender,
    state: &mut WorkerState,
    result: ProviderInstrumentSearchResult,
) {
    if result.provider != "rithmic" {
        return;
    }
    if NonZeroU64::new(result.provider_generation).is_none()
        || usize_generation(result.search_generation).is_none()
    {
        return;
    }
    if !state.pending_searches.remove(&result.search_generation) {
        return;
    }
    let _ = messages.send(MarketWorkerMessage::ProviderCatalog(
        ProviderCatalogEvent::SearchCompleted(result),
    ));
    send_connection(
        messages,
        FeedConnectionState::Authenticating,
        "Rithmic catalog search completed in the resident engine",
    );
}

fn publish_selection(
    messages: &MarketWorkerSender,
    state: &mut WorkerState,
    selection: ProviderInstrumentSelection,
) {
    let Some(instrument) = selection.instrument else {
        return;
    };
    if instrument.provider != "rithmic"
        || !state
            .pending_selections
            .remove(&instrument.selection_generation)
    {
        return;
    }
    if NonZeroU64::new(instrument.session_generation).is_none()
        || usize_generation(instrument.selection_generation).is_none()
    {
        return;
    }
    if validate_engine_instrument(&instrument).is_err() {
        return;
    }
    state.history.cancel();
    state.pending_history = None;
    state.installed = Some(instrument.clone());
    let _ = messages.send(MarketWorkerMessage::ProviderCatalog(
        ProviderCatalogEvent::SelectionInstalled(instrument),
    ));
    send_connection(
        messages,
        FeedConnectionState::Streaming,
        "resident engine installed the Rithmic instrument",
    );
}

fn publish_dispatch_rejection(
    messages: &MarketWorkerSender,
    command_generation: u64,
    command: ProviderCatalogCommand,
) {
    let _ = messages.send(MarketWorkerMessage::ProviderCatalog(
        ProviderCatalogEvent::CommandRejected {
            rejection: ProviderCatalogRejected {
                consumer_id: 0,
                provider: "rithmic".to_string(),
                provider_generation: None,
                command_generation,
                reason: ProviderCatalogRejectionReason::DispatchUnavailable as i32,
            },
            command,
        },
    ));
}

struct EngineCatalogSession {
    client: EngineSupervisor,
    consumer_id: u64,
}

impl EngineCatalogSession {
    fn connect() -> Result<Self, String> {
        let client_id = random_identity()?;
        let consumer_id = random_identity()?;
        let mut client = EngineSupervisor::connect(client_id)?;
        if let Err(error) = client.register_consumer(ENGINE_WORKSPACE_ID, consumer_id) {
            let _ = client.detach_client();
            return Err(error);
        }
        Ok(Self {
            client,
            consumer_id,
        })
    }

    fn search(&mut self, search: &SearchProviderInstruments) -> Result<(), String> {
        if search.provider != "rithmic"
            || search.search_generation == 0
            || search.query.trim().is_empty()
            || search.maximum_results == 0
        {
            return Err("Rithmic search shape is unsupported".to_string());
        }
        let mut search = search.clone();
        search.consumer_id = self.consumer_id;
        self.client.search_provider_instruments(search)
    }

    fn select(&mut self, selection: &SelectProviderInstrument) -> Result<(), String> {
        if selection.provider != "rithmic"
            || selection.selection_generation == 0
            || selection.search_generation == 0
            || selection.symbol.trim().is_empty()
            || selection.exchange.trim().is_empty()
            || selection.entitlement_id.trim().is_empty()
        {
            return Err("Rithmic selection shape is unsupported".to_string());
        }
        let mut selection = selection.clone();
        selection.consumer_id = self.consumer_id;
        self.client.select_provider_instrument(selection)
    }

    fn poll(&mut self) -> Result<crate::engine_supervisor::SupervisedPoll, String> {
        self.client.poll_market_event(self.consumer_id)
    }
}

impl Drop for EngineCatalogSession {
    fn drop(&mut self) {
        let _ = self.client.remove_market_consumer(self.consumer_id);
        let _ = self.client.detach_client();
    }
}

fn random_identity() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    Ok(NonZeroU64::new(u64::from_le_bytes(bytes))
        .unwrap_or(NonZeroU64::MIN)
        .get())
}

fn send_connection(messages: &MarketWorkerSender, state: FeedConnectionState, message: &str) {
    let _ = messages.send(MarketWorkerMessage::Connection {
        state,
        message: message.to_string(),
    });
}

fn wait_for_shutdown(commands: &Receiver<MarketWorkerCommand>) {
    while let Ok(command) = commands.recv() {
        if matches!(command, MarketWorkerCommand::Shutdown) {
            break;
        }
    }
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

fn usize_generation(generation: u64) -> Option<NonZeroUsize> {
    usize::try_from(generation).ok().and_then(NonZeroUsize::new)
}
