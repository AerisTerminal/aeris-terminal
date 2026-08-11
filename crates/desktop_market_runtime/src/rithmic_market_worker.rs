use crate::{
    market_worker::{
        MarketDataWorker, MarketWorkerCommand, MarketWorkerMessage, MarketWorkerStartup,
        market_worker_channel,
    },
    rithmic_history::{
        InstalledRithmicInstrument, RithmicHistoryTask, RithmicSeriesRequest, history_message,
    },
    rithmic_shell::RithmicShellState,
};
use axiusflow_desktop_provider_runtime::{InstrumentDescriptor, SessionGeneration};
use axiusflow_local_engine_client::{
    EngineClient, connect_or_start_engine, sibling_engine_executable,
};
use axiusflow_local_engine_protocol::{
    ProviderCatalogRejectionReason, ProviderInstrumentSearchResult, ProviderInstrumentSelection,
    SearchProviderInstruments, SelectProviderInstrument, envelope,
};
use axiusflow_observability::FeedConnectionState;
use axiusflow_rithmic_protocol_adapter::{
    CollectedSymbols, RithmicCatalogEvent, RithmicCatalogRejection, RithmicInstrumentSelection,
    RithmicSymbolSearch, SearchPattern, SymbolSearchResult,
};
use std::{
    collections::BTreeSet,
    num::{NonZeroU64, NonZeroUsize},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::Duration,
};

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const ENGINE_WORKSPACE_ID: u64 = 1;

struct WorkerState {
    catalog: EngineCatalogSession,
    history: RithmicHistoryTask,
    installed: Option<InstalledRithmicInstrument>,
    pending_history: Option<RithmicSeriesRequest>,
    pending_searches: BTreeSet<u64>,
    pending_selections: BTreeSet<u64>,
}

/// Starts the bounded Rithmic UI bridge backed exclusively by the resident engine.
///
/// # Errors
/// Returns a redacted engine, shell, or worker-start failure.
pub fn start() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let shell = RithmicShellState::local()?;
    spawn_worker(shell, |messages, commands| run(&messages, &commands))
}

fn spawn_worker(
    shell: RithmicShellState,
    task: impl FnOnce(crate::market_worker::MarketWorkerSender, Receiver<MarketWorkerCommand>)
    + Send
    + 'static,
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
        MarketWorkerStartup::Shell(shell),
        MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None),
    ))
}

fn run(
    messages: &crate::market_worker::MarketWorkerSender,
    commands: &Receiver<MarketWorkerCommand>,
) {
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
            Ok(Some(event)) => handle_catalog_event(messages, &mut state, event),
            Ok(None) => {}
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
    messages: &crate::market_worker::MarketWorkerSender,
    state: &mut WorkerState,
    command: MarketWorkerCommand,
) {
    match command {
        MarketWorkerCommand::RithmicSearch(search) => {
            let generation = u64_generation(search.generation());
            if state.catalog.search(&search).is_ok() {
                state.pending_searches.insert(generation);
            } else {
                publish_dispatch_rejection(messages, search.generation(), false);
            }
        }
        MarketWorkerCommand::RithmicSelect(selection) => {
            let generation = u64_generation(selection.generation());
            state.history.cancel();
            state.pending_history = None;
            if state.catalog.select(&selection).is_ok() {
                state.pending_selections.insert(generation);
            } else {
                publish_dispatch_rejection(messages, selection.generation(), true);
            }
        }
        MarketWorkerCommand::RithmicHistory(request) => {
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
        MarketWorkerCommand::Shutdown
        | MarketWorkerCommand::Recovery(_)
        | MarketWorkerCommand::CoinbaseSelect(_)
        | MarketWorkerCommand::ChartViewport(_) => {}
    }
}

fn handle_catalog_event(
    messages: &crate::market_worker::MarketWorkerSender,
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
            let selection = state
                .pending_selections
                .remove(&rejection.command_generation);
            state.pending_searches.remove(&rejection.command_generation);
            let Some(command_generation) = usize_generation(rejection.command_generation) else {
                return;
            };
            let session_generation = rejection
                .provider_generation
                .and_then(NonZeroU64::new)
                .map(SessionGeneration::new);
            let reason = catalog_rejection(rejection.reason, selection);
            let _ = messages.send(MarketWorkerMessage::RithmicCatalog(
                RithmicCatalogEvent::CommandRejected {
                    session_generation,
                    command_generation,
                    reason,
                },
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
    messages: &crate::market_worker::MarketWorkerSender,
    state: &mut WorkerState,
    result: ProviderInstrumentSearchResult,
) {
    if result.provider != "rithmic" {
        return;
    }
    let Some(session_generation) = NonZeroU64::new(result.provider_generation) else {
        return;
    };
    let Some(search_generation) = usize_generation(result.search_generation) else {
        return;
    };
    if !state.pending_searches.remove(&result.search_generation) {
        return;
    }
    let symbols = CollectedSymbols {
        results: result
            .instruments
            .into_iter()
            .map(|instrument| SymbolSearchResult {
                symbol: instrument.symbol,
                exchange: instrument.exchange,
                name: instrument.name,
                product_code: instrument.product_code,
                instrument_type: instrument.instrument_type,
                expiration_date: instrument.expiration_date,
            })
            .collect(),
        duplicate_count: 0,
    };
    let _ = messages.send(MarketWorkerMessage::RithmicCatalog(
        RithmicCatalogEvent::SearchCompleted {
            session_generation: SessionGeneration::new(session_generation),
            search_generation,
            symbols,
        },
    ));
    send_connection(
        messages,
        FeedConnectionState::Authenticating,
        "Rithmic catalog search completed in the resident engine",
    );
}

fn publish_selection(
    messages: &crate::market_worker::MarketWorkerSender,
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
    let Some(session_generation) = NonZeroU64::new(instrument.session_generation) else {
        return;
    };
    let Some(selection_generation) = usize_generation(instrument.selection_generation) else {
        return;
    };
    let Ok(price_scale) = u8::try_from(instrument.price_scale) else {
        return;
    };
    let Ok(quantity_scale) = u8::try_from(instrument.quantity_scale) else {
        return;
    };
    let descriptor = InstrumentDescriptor {
        instrument_id: instrument.instrument_id,
        provider_symbol: instrument.provider_symbol,
        display_symbol: instrument.display_symbol,
        venue_id: instrument.venue_id,
        price_scale,
        quantity_scale,
    };
    if descriptor.validate().is_err() {
        return;
    }
    let session_generation = SessionGeneration::new(session_generation);
    state.history.cancel();
    state.pending_history = None;
    state.installed = Some(InstalledRithmicInstrument {
        session_generation: session_generation.get(),
        selection_generation,
        descriptor: descriptor.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
    });
    let _ = messages.send(MarketWorkerMessage::RithmicCatalog(
        RithmicCatalogEvent::SelectionInstalled {
            session_generation,
            selection_generation,
            instrument: descriptor,
            entitlement_id: instrument.entitlement_id,
        },
    ));
    send_connection(
        messages,
        FeedConnectionState::Streaming,
        "resident engine installed the Rithmic instrument",
    );
}

fn publish_dispatch_rejection(
    messages: &crate::market_worker::MarketWorkerSender,
    command_generation: NonZeroUsize,
    selection: bool,
) {
    let reason = if selection {
        RithmicCatalogRejection::SelectionDispatchUnavailable
    } else {
        RithmicCatalogRejection::SearchDispatchUnavailable
    };
    let _ = messages.send(MarketWorkerMessage::RithmicCatalog(
        RithmicCatalogEvent::CommandRejected {
            session_generation: None,
            command_generation,
            reason,
        },
    ));
}

fn catalog_rejection(reason: i32, selection: bool) -> RithmicCatalogRejection {
    match ProviderCatalogRejectionReason::try_from(reason).ok() {
        Some(ProviderCatalogRejectionReason::SearchRejected) => {
            RithmicCatalogRejection::SearchRejected
        }
        Some(ProviderCatalogRejectionReason::SupersededSearch) => {
            RithmicCatalogRejection::SupersededSearch
        }
        Some(ProviderCatalogRejectionReason::InstrumentUnavailable) => {
            RithmicCatalogRejection::InstrumentUnavailable
        }
        Some(ProviderCatalogRejectionReason::SubscriptionRejected) => {
            RithmicCatalogRejection::SubscriptionRejected
        }
        Some(
            ProviderCatalogRejectionReason::DispatchUnavailable
            | ProviderCatalogRejectionReason::Unspecified,
        )
        | None
            if selection =>
        {
            RithmicCatalogRejection::SelectionDispatchUnavailable
        }
        Some(
            ProviderCatalogRejectionReason::DispatchUnavailable
            | ProviderCatalogRejectionReason::Unspecified,
        )
        | None => RithmicCatalogRejection::SearchDispatchUnavailable,
    }
}

struct EngineCatalogSession {
    client: EngineClient,
    client_id: u64,
    consumer_id: u64,
}

impl EngineCatalogSession {
    fn connect() -> Result<Self, String> {
        let executable = sibling_engine_executable()?;
        let mut client = connect_or_start_engine(&executable)?;
        let client_id = random_identity()?;
        let consumer_id = random_identity()?;
        client.attach_client(client_id)?;
        if let Err(error) = client.register_consumer(client_id, ENGINE_WORKSPACE_ID, consumer_id) {
            let _ = client.detach_client(client_id);
            return Err(error);
        }
        Ok(Self {
            client,
            client_id,
            consumer_id,
        })
    }

    fn search(&mut self, search: &RithmicSymbolSearch) -> Result<(), String> {
        if search.exchange().is_some()
            || search.product_code().is_some()
            || search.instrument_type().is_some()
            || search.pattern() != SearchPattern::Equals
        {
            return Err("Rithmic search shape is unsupported".to_string());
        }
        self.client
            .search_provider_instruments(SearchProviderInstruments {
                consumer_id: self.consumer_id,
                search_generation: u64_generation(search.generation()),
                provider: "rithmic".to_string(),
                query: search.query().to_string(),
                maximum_results: u32::try_from(search.maximum_results().get())
                    .map_err(|_| "Rithmic search result bound is invalid".to_string())?,
            })
    }

    fn select(&mut self, selection: &RithmicInstrumentSelection) -> Result<(), String> {
        self.client
            .select_provider_instrument(SelectProviderInstrument {
                consumer_id: self.consumer_id,
                selection_generation: u64_generation(selection.generation()),
                search_generation: u64_generation(selection.search_generation()),
                provider: "rithmic".to_string(),
                symbol: selection.symbol().to_string(),
                exchange: selection.exchange().to_string(),
                entitlement_id: selection.entitlement_id().to_string(),
            })
    }

    fn poll(&mut self) -> Result<Option<envelope::Payload>, String> {
        self.client.poll_market_event(self.consumer_id)
    }
}

impl Drop for EngineCatalogSession {
    fn drop(&mut self) {
        let _ = self.client.remove_market_consumer(self.consumer_id);
        let _ = self.client.detach_client(self.client_id);
    }
}

fn random_identity() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    Ok(NonZeroU64::new(u64::from_le_bytes(bytes))
        .unwrap_or(NonZeroU64::MIN)
        .get())
}

fn send_connection(
    messages: &crate::market_worker::MarketWorkerSender,
    state: FeedConnectionState,
    message: &str,
) {
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

fn u64_generation(generation: NonZeroUsize) -> u64 {
    u64::try_from(generation.get()).unwrap_or(u64::MAX)
}

fn usize_generation(generation: u64) -> Option<NonZeroUsize> {
    usize::try_from(generation).ok().and_then(NonZeroUsize::new)
}
