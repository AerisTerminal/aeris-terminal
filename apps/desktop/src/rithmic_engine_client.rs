use crate::{
    rithmic_engine_history::{
        DomIdentity, demand_error_message, dom_from_snapshot, engine_series, live_tail,
        snapshot_bootstrap, validate_engine_instrument,
    },
    rithmic_history::{RithmicSeries, RithmicSeriesRequest},
};
use axiusflow_application::ReplayStreamUpdate;
use axiusflow_desktop::market_worker::{
    EngineSeriesRequest, MarketDataWorker, MarketWorkerCommand, MarketWorkerMessage,
    MarketWorkerSender, MarketWorkerStartup, ProviderCatalogCommand, ProviderCatalogEvent,
    classify_provider_catalog_event, market_worker_channel,
};
use axiusflow_engine_protocol::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderInstrumentSearchResult, SearchProviderInstruments, SelectProviderInstrument, SeriesKey,
    SeriesLoadState, envelope,
};
use axiusflow_observability::FeedConnectionState;
use std::{
    collections::VecDeque,
    num::{NonZeroU8, NonZeroU64, NonZeroUsize},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use crate::engine_supervisor::EngineSupervisor;

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const INITIAL_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(250);
const CATALOG_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const INITIAL_SERIES_TIMEOUT: Duration = Duration::from_secs(15);
const MAXIMUM_INITIAL_CONNECT_ATTEMPTS: NonZeroU8 = match NonZeroU8::new(3) {
    Some(attempts) => attempts,
    None => NonZeroU8::MIN,
};
const ENGINE_WORKSPACE_ID: u64 = 1;

/// Connection message contract that arms the desktop autoload and
/// reconnect instrument searches (`rithmic_ready_action`).
pub(crate) const RITHMIC_CATALOG_READY_MESSAGE: &str =
    "Rithmic Test session is ready for instrument search";

struct WorkerState {
    catalog: EngineCatalogSession,
    installed: Option<InstallProviderInstrument>,
    active_series: Option<ActiveSeries>,
    pending_search: Option<PendingOperation>,
    pending_selection: Option<PendingOperation>,
}

struct ActiveSeries {
    request: RithmicSeriesRequest,
    instrument: InstallProviderInstrument,
    key: SeriesKey,
    initial_snapshot_deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingOperation {
    generation: u64,
    deadline: Instant,
}

impl PendingOperation {
    fn new(generation: u64, now: Instant, timeout: Duration) -> Self {
        Self {
            generation,
            deadline: now.checked_add(timeout).unwrap_or(now),
        }
    }

    fn expired(self, now: Instant) -> bool {
        now >= self.deadline
    }
}

enum InitialConnection<T> {
    Connected {
        session: T,
        pending: VecDeque<MarketWorkerCommand>,
    },
    Shutdown,
    Exhausted,
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
    let (catalog, pending) = match connect_initial(
        messages,
        commands,
        MAXIMUM_INITIAL_CONNECT_ATTEMPTS,
        INITIAL_CONNECT_RETRY_DELAY,
        EngineCatalogSession::connect,
    ) {
        InitialConnection::Connected { session, pending } => (session, pending),
        InitialConnection::Shutdown => {
            send_connection(
                messages,
                FeedConnectionState::Stopped,
                "Rithmic engine client stopped",
            );
            return;
        }
        InitialConnection::Exhausted => return,
    };
    let mut state = WorkerState {
        catalog,
        installed: None,
        active_series: None,
        pending_search: None,
        pending_selection: None,
    };
    send_connection(
        messages,
        FeedConnectionState::Authenticating,
        RITHMIC_CATALOG_READY_MESSAGE,
    );
    for command in pending {
        process_command(messages, &mut state, command);
    }

    loop {
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(MarketWorkerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(command) => process_command(messages, &mut state, command),
            Err(RecvTimeoutError::Timeout) => {}
        }
        match state.catalog.receive(POLL_INTERVAL) {
            Ok(poll) if poll.reconnected => {
                send_connection(
                    messages,
                    FeedConnectionState::Recovering,
                    "resident Rithmic engine restarted; restoring catalog demand",
                );
                send_connection(
                    messages,
                    FeedConnectionState::Authenticating,
                    RITHMIC_CATALOG_READY_MESSAGE,
                );
            }
            Ok(poll) => {
                if let Some(event) = poll.event {
                    handle_engine_event(messages, &mut state, event);
                }
            }
            Err(_) => {
                send_connection(
                    messages,
                    FeedConnectionState::Recovering,
                    "resident Rithmic catalog connection is recovering",
                );
            }
        }
        expire_silent_operations(messages, &mut state, Instant::now());
    }
    send_connection(
        messages,
        FeedConnectionState::Stopped,
        "Rithmic engine client stopped",
    );
}

fn connect_initial<T>(
    messages: &MarketWorkerSender,
    commands: &Receiver<MarketWorkerCommand>,
    maximum_attempts: NonZeroU8,
    retry_delay: Duration,
    mut connect: impl FnMut() -> Result<T, String>,
) -> InitialConnection<T> {
    let mut pending = VecDeque::with_capacity(COMMAND_CAPACITY);
    for attempt in 1..=maximum_attempts.get() {
        match connect() {
            Ok(session) => return InitialConnection::Connected { session, pending },
            Err(_) if attempt < maximum_attempts.get() => {
                send_connection(
                    messages,
                    FeedConnectionState::Recovering,
                    "resident Rithmic engine is unavailable; retrying startup",
                );
                if !wait_for_initial_retry(commands, retry_delay, &mut pending) {
                    return InitialConnection::Shutdown;
                }
            }
            Err(_) => {
                send_connection(
                    messages,
                    FeedConnectionState::Stopped,
                    "resident Rithmic engine is unavailable after bounded startup retries",
                );
                return InitialConnection::Exhausted;
            }
        }
    }
    InitialConnection::Exhausted
}

fn wait_for_initial_retry(
    commands: &Receiver<MarketWorkerCommand>,
    delay: Duration,
    pending: &mut VecDeque<MarketWorkerCommand>,
) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        if pending.len() == COMMAND_CAPACITY {
            thread::sleep(remaining);
            return true;
        }
        match commands.recv_timeout(remaining) {
            Ok(MarketWorkerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                return false;
            }
            Ok(command) => {
                if pending.len() < COMMAND_CAPACITY {
                    pending.push_back(command);
                }
            }
            Err(RecvTimeoutError::Timeout) => return true,
        }
    }
}

fn process_command(
    messages: &MarketWorkerSender,
    state: &mut WorkerState,
    command: MarketWorkerCommand,
) {
    match command {
        MarketWorkerCommand::ProviderSearch(search) => {
            let generation = search.search_generation;
            if state.catalog.search(search).is_err() {
                publish_dispatch_rejection(messages, generation, ProviderCatalogCommand::Search);
            } else {
                state.pending_search = Some(PendingOperation::new(
                    generation,
                    Instant::now(),
                    CATALOG_RESPONSE_TIMEOUT,
                ));
            }
        }
        MarketWorkerCommand::ProviderSelect(selection) => {
            let generation = selection.selection_generation;
            state.active_series = None;
            if state.catalog.select(selection).is_err() {
                publish_dispatch_rejection(messages, generation, ProviderCatalogCommand::Selection);
            } else {
                state.pending_selection = Some(PendingOperation::new(
                    generation,
                    Instant::now(),
                    CATALOG_RESPONSE_TIMEOUT,
                ));
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
                .and_then(|instrument| {
                    if u64::try_from(request.selection_generation.get()).unwrap_or(u64::MAX)
                        != instrument.selection_generation
                        || !request.series.supports_native_history()
                    {
                        return Err("Rithmic history series is unavailable".to_string());
                    }
                    let key = engine_series(request, &instrument)?;
                    state.catalog.install_series(
                        request.series_generation,
                        instrument.clone(),
                        key.clone(),
                    )?;
                    state.active_series = Some(ActiveSeries {
                        request,
                        instrument,
                        key,
                        initial_snapshot_deadline: Some(
                            Instant::now()
                                .checked_add(INITIAL_SERIES_TIMEOUT)
                                .unwrap_or_else(Instant::now),
                        ),
                    });
                    Ok(())
                });
            if let Err(error) = result {
                let _ = messages.send(MarketWorkerMessage::RithmicHistory {
                    selection_generation: request.selection_generation,
                    series_generation: request.series_generation,
                    result: Err(error),
                });
            }
        }
        MarketWorkerCommand::ResourceClass(resource_class) => {
            let _ = state.catalog.set_resource_class(resource_class);
        }
        MarketWorkerCommand::Shutdown
        | MarketWorkerCommand::Recovery(_)
        | MarketWorkerCommand::CoinbaseSelect(_)
        | MarketWorkerCommand::ChartViewport(_) => {}
    }
}

fn handle_engine_event(
    messages: &MarketWorkerSender,
    state: &mut WorkerState,
    event: envelope::Payload,
) {
    let Some(event) = handle_catalog_event(messages, state, event) else {
        return;
    };
    let Some(active) = state.active_series.as_ref() else {
        if let envelope::Payload::Fault(fault) = event {
            send_connection(
                messages,
                FeedConnectionState::Recovering,
                &fault.redacted_detail,
            );
        }
        return;
    };
    match event {
        envelope::Payload::SeriesSnapshot(snapshot)
            if series_identity_matches(state.catalog.consumer_id, active, &snapshot) =>
        {
            if active.initial_snapshot_deadline.is_none() {
                return;
            }
            let request = active.request;
            let result = snapshot_bootstrap(request, &active.instrument, &snapshot).map(Box::new);
            if let Some(active) = state.active_series.as_mut() {
                active.initial_snapshot_deadline = None;
            }
            let _ = messages.send(MarketWorkerMessage::RithmicHistory {
                selection_generation: request.selection_generation,
                series_generation: request.series_generation,
                result,
            });
        }
        envelope::Payload::SeriesUpdate(update)
            if update.consumer_id == state.catalog.consumer_id
                && update.generation == series_generation(active.request)
                && update.series.as_ref() == Some(&active.key)
                && update.provider_generation >= active.instrument.session_generation =>
        {
            match live_tail(active.request, &active.instrument, &update) {
                Ok(tail) => {
                    let _ = messages.send(MarketWorkerMessage::RithmicLive {
                        selection_generation: active.request.selection_generation,
                        series_generation: active.request.series_generation,
                        update: ReplayStreamUpdate::Tail(tail),
                    });
                }
                Err(error) => publish_series_error(messages, active.request, error),
            }
        }
        envelope::Payload::OrderBookSnapshot(snapshot)
            if snapshot.consumer_id == state.catalog.consumer_id =>
        {
            if let Ok(frame) = dom_from_snapshot(
                &DomIdentity {
                    instrument: &active.instrument,
                    series_generation: series_generation(active.request),
                },
                &snapshot,
            ) {
                let _ = messages.send(MarketWorkerMessage::RithmicDom(frame));
            }
        }
        envelope::Payload::SeriesState(series_state)
            if series_state.consumer_id == state.catalog.consumer_id
                && series_state.generation == series_generation(active.request) =>
        {
            if matches!(
                SeriesLoadState::try_from(series_state.state),
                Ok(SeriesLoadState::Failed | SeriesLoadState::Superseded)
            ) {
                let request = active.request;
                if let Some(active) = state.active_series.as_mut() {
                    active.initial_snapshot_deadline = None;
                }
                publish_series_error(
                    messages,
                    request,
                    series_state
                        .detail
                        .unwrap_or_else(|| "Rithmic series is unavailable".to_string()),
                );
            }
        }
        envelope::Payload::DemandError(error) => {
            let request = active.request;
            if let Some(active) = state.active_series.as_mut() {
                active.initial_snapshot_deadline = None;
            }
            publish_series_error(messages, request, demand_error_message(&error));
        }
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

fn handle_catalog_event(
    messages: &MarketWorkerSender,
    state: &mut WorkerState,
    event: envelope::Payload,
) -> Option<envelope::Payload> {
    let (catalog, passthrough) =
        classify_provider_catalog_event(event, "rithmic", state.catalog.consumer_id);
    match catalog {
        Some(ProviderCatalogEvent::SearchCompleted(result)) => {
            if !take_current_operation(&mut state.pending_search, result.search_generation) {
                return None;
            }
            publish_search_result(messages, result);
        }
        Some(ProviderCatalogEvent::SelectionInstalled(instrument)) => {
            if !take_current_operation(
                &mut state.pending_selection,
                instrument.selection_generation,
            ) {
                return None;
            }
            publish_selection(messages, state, instrument);
        }
        Some(ProviderCatalogEvent::CommandRejected { rejection, command }) => {
            let current = match command {
                ProviderCatalogCommand::Search => {
                    take_current_operation(&mut state.pending_search, rejection.command_generation)
                }
                ProviderCatalogCommand::Selection => take_current_operation(
                    &mut state.pending_selection,
                    rejection.command_generation,
                ),
            };
            if !current {
                return None;
            }
            let _ = messages.send(MarketWorkerMessage::ProviderCatalog(
                ProviderCatalogEvent::CommandRejected { rejection, command },
            ));
            send_connection(
                messages,
                FeedConnectionState::Recovering,
                "Rithmic catalog command was rejected",
            );
        }
        None => {}
    }
    passthrough
}

fn take_current_operation(pending: &mut Option<PendingOperation>, generation: u64) -> bool {
    if pending.is_some_and(|operation| operation.generation == generation) {
        *pending = None;
        true
    } else {
        false
    }
}

fn take_expired_operation(pending: &mut Option<PendingOperation>, now: Instant) -> Option<u64> {
    if pending.is_some_and(|operation| operation.expired(now)) {
        pending.take().map(|operation| operation.generation)
    } else {
        None
    }
}

fn take_expired_deadline(deadline: &mut Option<Instant>, now: Instant) -> bool {
    if deadline.is_some_and(|deadline| now >= deadline) {
        *deadline = None;
        true
    } else {
        false
    }
}

fn expire_silent_operations(messages: &MarketWorkerSender, state: &mut WorkerState, now: Instant) {
    if let Some(generation) = take_expired_operation(&mut state.pending_search, now) {
        state.catalog.abandon_search("rithmic", generation);
        publish_catalog_timeout(messages, generation, ProviderCatalogCommand::Search);
    }
    if let Some(generation) = take_expired_operation(&mut state.pending_selection, now) {
        state.catalog.abandon_selection("rithmic", generation);
        publish_catalog_timeout(messages, generation, ProviderCatalogCommand::Selection);
    }
    if let Some(active) = state.active_series.as_mut()
        && take_expired_deadline(&mut active.initial_snapshot_deadline, now)
    {
        publish_series_error(
            messages,
            active.request,
            "Rithmic history timed out; choose the series to retry".to_string(),
        );
    }
}

fn publish_catalog_timeout(
    messages: &MarketWorkerSender,
    generation: u64,
    command: ProviderCatalogCommand,
) {
    let reason = match command {
        ProviderCatalogCommand::Search => ProviderCatalogRejectionReason::SearchTimedOut,
        ProviderCatalogCommand::Selection => ProviderCatalogRejectionReason::SelectionTimedOut,
    };
    let _ = messages.send(MarketWorkerMessage::ProviderCatalog(
        ProviderCatalogEvent::CommandRejected {
            rejection: ProviderCatalogRejected {
                consumer_id: 0,
                provider: "rithmic".to_string(),
                provider_generation: None,
                command_generation: generation,
                reason: reason as i32,
            },
            command,
        },
    ));
}

fn series_identity_matches(
    consumer_id: u64,
    active: &ActiveSeries,
    snapshot: &axiusflow_engine_protocol::SeriesSnapshot,
) -> bool {
    snapshot.consumer_id == consumer_id
        && snapshot.generation == series_generation(active.request)
        && snapshot.series.as_ref() == Some(&active.key)
        && snapshot.provider_generation >= active.instrument.session_generation
}

fn publish_series_error(
    messages: &MarketWorkerSender,
    request: RithmicSeriesRequest,
    error: String,
) {
    let _ = messages.send(MarketWorkerMessage::RithmicHistory {
        selection_generation: request.selection_generation,
        series_generation: request.series_generation,
        result: Err(error),
    });
}

fn series_generation(request: RithmicSeriesRequest) -> u64 {
    u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX)
}

fn publish_search_result(messages: &MarketWorkerSender, result: ProviderInstrumentSearchResult) {
    if NonZeroU64::new(result.provider_generation).is_none()
        || usize_generation(result.search_generation).is_none()
    {
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
    instrument: InstallProviderInstrument,
) {
    if NonZeroU64::new(instrument.session_generation).is_none()
        || usize_generation(instrument.selection_generation).is_none()
    {
        return;
    }
    if validate_engine_instrument(&instrument).is_err() {
        return;
    }
    state.active_series = None;
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

    fn search(&mut self, mut search: SearchProviderInstruments) -> Result<(), String> {
        search.consumer_id = self.consumer_id;
        self.client.search_provider_instruments(search)
    }

    fn select(&mut self, mut selection: SelectProviderInstrument) -> Result<(), String> {
        selection.consumer_id = self.consumer_id;
        self.client.select_provider_instrument(selection)
    }

    fn abandon_search(&mut self, provider: &str, generation: u64) {
        self.client
            .abandon_provider_search(self.consumer_id, provider, generation);
    }

    fn abandon_selection(&mut self, provider: &str, generation: u64) {
        self.client
            .abandon_provider_selection(self.consumer_id, provider, generation);
    }

    fn install_series(
        &mut self,
        generation: NonZeroUsize,
        instrument: InstallProviderInstrument,
        series: SeriesKey,
    ) -> Result<(), String> {
        self.client.install_provider_instrument(instrument)?;
        self.client.set_series_demand(
            self.consumer_id,
            u64::try_from(generation.get()).unwrap_or(u64::MAX),
            series,
        )
    }

    fn set_resource_class(
        &mut self,
        resource_class: axiusflow_engine_protocol::ConsumerResourceClass,
    ) -> Result<(), String> {
        self.client
            .set_market_resource_class(self.consumer_id, resource_class)
    }

    fn receive(
        &mut self,
        timeout: Duration,
    ) -> Result<crate::engine_supervisor::SupervisedEvent, String> {
        self.client
            .receive_market_event_for(self.consumer_id, timeout)
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

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

fn usize_generation(generation: u64) -> Option<NonZeroUsize> {
    usize::try_from(generation).ok().and_then(NonZeroUsize::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_engine_protocol::ConsumerResourceClass;

    #[test]
    fn initial_connection_recovers_after_engine_and_ipc_failures() {
        let (messages, receiver) = market_worker_channel(nonzero(8));
        let (commands, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        commands
            .send(MarketWorkerCommand::ResourceClass(
                ConsumerResourceClass::Background,
            ))
            .expect("startup command queues");
        let mut attempts = 0;

        let result = connect_initial(
            &messages,
            &command_rx,
            MAXIMUM_INITIAL_CONNECT_ATTEMPTS,
            Duration::from_millis(1),
            || {
                attempts += 1;
                match attempts {
                    1 => Err("fixture engine absent".to_string()),
                    2 => Err("fixture IPC registration failure".to_string()),
                    _ => Ok(7_u8),
                }
            },
        );

        let InitialConnection::Connected {
            session,
            mut pending,
        } = result
        else {
            panic!("delayed startup connects");
        };
        assert_eq!(session, 7);
        assert_eq!(attempts, 3);
        assert!(matches!(
            pending.pop_front(),
            Some(MarketWorkerCommand::ResourceClass(
                ConsumerResourceClass::Background
            ))
        ));
        assert!(pending.is_empty());
        let (events, _) = receiver.drain();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    MarketWorkerMessage::Connection {
                        state: FeedConnectionState::Recovering,
                        ..
                    }
                ))
                .count(),
            2
        );
    }

    #[test]
    fn initial_connection_exhaustion_is_terminal_and_bounded() {
        let (messages, receiver) = market_worker_channel(nonzero(8));
        let (_commands, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let mut attempts = 0;

        let result = connect_initial(
            &messages,
            &command_rx,
            MAXIMUM_INITIAL_CONNECT_ATTEMPTS,
            Duration::ZERO,
            || {
                attempts += 1;
                Err::<(), _>("fixture engine absent".to_string())
            },
        );

        assert!(matches!(result, InitialConnection::Exhausted));
        assert_eq!(attempts, MAXIMUM_INITIAL_CONNECT_ATTEMPTS.get());
        let (events, _) = receiver.drain();
        assert!(matches!(
            events.last(),
            Some(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Stopped,
                message,
            }) if message == "resident Rithmic engine is unavailable after bounded startup retries"
        ));
    }

    #[test]
    fn initial_connection_cancels_while_waiting_to_retry() {
        let (messages, _receiver) = market_worker_channel(nonzero(8));
        let (commands, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        commands
            .send(MarketWorkerCommand::Shutdown)
            .expect("shutdown queues");
        let mut attempts = 0;

        let result = connect_initial(
            &messages,
            &command_rx,
            MAXIMUM_INITIAL_CONNECT_ATTEMPTS,
            Duration::from_secs(1),
            || {
                attempts += 1;
                Err::<(), _>("fixture engine absent".to_string())
            },
        );

        assert!(matches!(result, InitialConnection::Shutdown));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn silent_operation_deadline_is_generation_fenced_and_one_shot() {
        let now = Instant::now();
        let mut pending = Some(PendingOperation::new(7, now, Duration::from_secs(1)));

        assert_eq!(take_expired_operation(&mut pending, now), None);
        assert!(!take_current_operation(&mut pending, 6));
        assert_eq!(
            take_expired_operation(&mut pending, now + Duration::from_secs(1)),
            Some(7)
        );
        assert_eq!(
            take_expired_operation(&mut pending, now + Duration::from_secs(2)),
            None
        );
    }

    #[test]
    fn initial_series_deadline_resolves_once_without_a_market_event() {
        let now = Instant::now();
        let mut deadline = now.checked_add(Duration::from_secs(1));

        assert!(!take_expired_deadline(&mut deadline, now));
        assert!(take_expired_deadline(
            &mut deadline,
            now + Duration::from_secs(1)
        ));
        assert!(!take_expired_deadline(
            &mut deadline,
            now + Duration::from_secs(2)
        ));
    }

    #[test]
    fn catalog_timeouts_are_actionable_for_search_and_selection() {
        let (messages, receiver) = market_worker_channel(nonzero(4));
        publish_catalog_timeout(&messages, 11, ProviderCatalogCommand::Search);
        publish_catalog_timeout(&messages, 12, ProviderCatalogCommand::Selection);

        let (events, _) = receiver.drain();
        assert!(matches!(
            events.as_slice(),
            [
                MarketWorkerMessage::ProviderCatalog(ProviderCatalogEvent::CommandRejected {
                    rejection: ProviderCatalogRejected {
                        command_generation: 11,
                        reason,
                        ..
                    },
                    command: ProviderCatalogCommand::Search,
                }),
                MarketWorkerMessage::ProviderCatalog(ProviderCatalogEvent::CommandRejected {
                    rejection: ProviderCatalogRejected {
                        command_generation: 12,
                        reason: selection_reason,
                        ..
                    },
                    command: ProviderCatalogCommand::Selection,
                })
            ] if *reason == ProviderCatalogRejectionReason::SearchTimedOut as i32
                && *selection_reason == ProviderCatalogRejectionReason::SelectionTimedOut as i32
        ));
    }
}
