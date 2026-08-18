//! Engine-owned Rithmic trade-session lifecycle.

use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use axiusflow_local_engine_protocol::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderInstrumentSearchResult, ProviderInstrumentSummary, SearchProviderInstruments,
    SelectProviderInstrument,
};
use axiusflow_market_data::{DepthSnapshot, MarketEvent, MarketTrade};
use axiusflow_platform_runtime::{
    NativeCredentialVault, NativeNetworkMonitor, NativeNetworkMonitorCancellation,
    NativePowerMonitor, NativePowerMonitorCancellation, NetworkEvent, PowerEvent,
};
use axiusflow_rithmic_protocol_adapter::{
    AppliedRithmicEvent, InstrumentDescriptor, MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES,
    ProviderSessionEvent, RITHMIC_TEST_VAULT_KEY, RITHMIC_TEST_VAULT_SERVICE,
    RithmicCallbackLimits, RithmicCatalogEvent as AdapterCatalogEvent, RithmicCatalogRejection,
    RithmicEnvironmentEvent, RithmicInstrumentSelection, RithmicProviderConfig,
    RithmicProviderDriver, RithmicProviderEvents, RithmicProviderInstrument,
    RithmicProviderRuntime, RithmicProviderRuntimeConfig, RithmicProviderRuntimeState,
    RithmicReadOnlySubscription, RithmicRetryScheduler, RithmicSessionLimits, RithmicSymbolSearch,
    SearchPattern, SessionGeneration, apply_rithmic_environment_event, try_recv_rithmic_event,
};

const CALLBACK_CAPACITY: usize = 256;
const CALLBACK_BYTES: usize = 8 * 1024 * 1024;
const EVENT_WAIT: Duration = Duration::from_millis(16);
const MESSAGE_SILENCE: Duration = Duration::from_mins(2);
const ENVIRONMENT_CAPACITY: usize = 8;

pub(crate) enum RithmicRealtimeControl {
    Select(InstallProviderInstrument),
    Stop,
}

pub(crate) enum RithmicCatalogControl {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
}

pub(crate) enum RithmicCatalogEvent {
    SearchCompleted(ProviderInstrumentSearchResult),
    SelectionResolved {
        consumer_id: u64,
        instrument: InstallProviderInstrument,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
    },
}

pub(crate) enum RithmicRealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Trade(u64, MarketTrade),
    Depth(u64, DepthSnapshot),
    Heartbeat(u64),
    Recovering(u64),
    Disconnected(u64),
}

type Runtime = RithmicProviderRuntime<NativeCredentialVault, RithmicProviderDriver>;

enum EnvironmentMessage {
    Event(RithmicEnvironmentEvent),
    Failed,
}

#[derive(Clone, Copy)]
struct EnvironmentState {
    network: NetworkEvent,
    suspended: bool,
}

struct EnvironmentMonitors {
    events: Option<Receiver<EnvironmentMessage>>,
    state: EnvironmentState,
    network_cancellation: Option<NativeNetworkMonitorCancellation>,
    power_cancellation: Option<NativePowerMonitorCancellation>,
    network_worker: Option<thread::JoinHandle<()>>,
    power_worker: Option<thread::JoinHandle<()>>,
}

impl EnvironmentMonitors {
    fn parts(&mut self) -> (&Receiver<EnvironmentMessage>, &mut EnvironmentState) {
        let Some(events) = self.events.as_ref() else {
            unreachable!("environment receiver exists until monitor shutdown");
        };
        (events, &mut self.state)
    }

    fn shutdown(&mut self) -> Vec<&'static str> {
        drop(self.events.take());
        if let Some(cancellation) = self.network_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(cancellation) = self.power_cancellation.take() {
            cancellation.cancel();
        }
        let mut panicked = Vec::new();
        if self
            .network_worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            panicked.push("axiusflow-engine-rithmic-network-monitor");
        }
        if self
            .power_worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            panicked.push("axiusflow-engine-rithmic-power-monitor");
        }
        panicked
    }
}

impl Drop for EnvironmentMonitors {
    fn drop(&mut self) {
        let panicked = self.shutdown();
        assert!(
            panicked.is_empty() || thread::panicking(),
            "native environment monitor workers panicked: {}",
            panicked.join(", ")
        );
    }
}

impl EnvironmentState {
    fn observe(&mut self, event: RithmicEnvironmentEvent) {
        match event {
            RithmicEnvironmentEvent::Network(network) => self.network = network,
            RithmicEnvironmentEvent::Power(PowerEvent::Suspending) => self.suspended = true,
            RithmicEnvironmentEvent::Power(PowerEvent::Resumed) => self.suspended = false,
        }
    }
}

pub(crate) fn run_catalog(
    controls: &Receiver<RithmicCatalogControl>,
    publications: &SyncSender<RithmicCatalogEvent>,
    reconnect_delay: Duration,
) {
    let Ok(mut environment) = start_environment_monitors() else {
        while let Ok(control) = controls.recv() {
            reject_catalog_control(publications, control, None);
        }
        return;
    };
    let mut generation = 0_u64;
    loop {
        let Ok((runtime, events)) = open_catalog_runtime() else {
            match controls.recv_timeout(reconnect_delay) {
                Ok(control) => reject_catalog_control(publications, control, None),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
            continue;
        };
        generation = next_generation(generation, generation);
        let (environment_events, environment_state) = environment.parts();
        match run_catalog_session(
            runtime,
            &events,
            controls,
            publications,
            environment_events,
            environment_state,
            generation,
        ) {
            CatalogSessionExit::Retry(updated) => {
                generation = updated;
                thread::park_timeout(reconnect_delay);
            }
            CatalogSessionExit::Closed => return,
        }
    }
}

enum CatalogSessionExit {
    Retry(u64),
    Closed,
}

fn run_catalog_session(
    mut runtime: Runtime,
    events: &RithmicProviderEvents,
    controls: &Receiver<RithmicCatalogControl>,
    publications: &SyncSender<RithmicCatalogEvent>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
    mut generation: u64,
) -> CatalogSessionExit {
    let mut retries = RithmicRetryScheduler::default();
    let mut searches = BTreeMap::new();
    let mut selections = BTreeMap::new();
    if apply_current_environment(&mut runtime, events, &mut retries, *environment_state).is_err() {
        return CatalogSessionExit::Retry(generation);
    }
    loop {
        match poll_catalog_environment(
            environment,
            environment_state,
            &mut runtime,
            events,
            &mut retries,
        ) {
            Ok(true) => {
                generation = next_generation(generation, generation);
                searches.clear();
                selections.clear();
            }
            Ok(false) => {}
            Err(()) => {
                let _ = runtime.stop();
                return CatalogSessionExit::Closed;
            }
        }
        loop {
            match controls.try_recv() {
                Ok(control) => dispatch_catalog_control(
                    &runtime,
                    events,
                    publications,
                    generation,
                    &mut searches,
                    &mut selections,
                    control,
                ),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = runtime.stop();
                    return CatalogSessionExit::Closed;
                }
            }
        }
        while let Some(callback) = events.try_recv_catalog() {
            if active_generation(&runtime) != Some(callback.generation) {
                continue;
            }
            publish_catalog_callback(
                publications,
                generation,
                callback.event,
                &mut searches,
                &mut selections,
            );
        }
        while events.has_ready() {
            match try_recv_rithmic_event(&mut runtime, events, &mut retries, Instant::now()) {
                Ok(Some(AppliedRithmicEvent::RetryScheduled(_)) | None) => break,
                Ok(Some(AppliedRithmicEvent::TerminalFailure { .. })) | Err(_) => {
                    let _ = runtime.stop();
                    return CatalogSessionExit::Retry(generation);
                }
                Ok(Some(AppliedRithmicEvent::Semantic(_))) => {}
            }
        }
        if retries
            .retry_due(&mut runtime, Instant::now())
            .is_ok_and(|started| started.is_some())
        {
            generation = next_generation(generation, generation);
            searches.clear();
            selections.clear();
        }
        thread::sleep(EVENT_WAIT);
    }
}

fn poll_catalog_environment(
    environment: &Receiver<EnvironmentMessage>,
    state: &mut EnvironmentState,
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
) -> Result<bool, ()> {
    let event = match environment.try_recv() {
        Ok(EnvironmentMessage::Event(event)) => event,
        Ok(EnvironmentMessage::Failed) | Err(TryRecvError::Disconnected) => return Err(()),
        Err(TryRecvError::Empty) => return Ok(false),
    };
    state.observe(event);
    apply_rithmic_environment_event(runtime, events, retries, event)
        .map(|started| started.is_some())
        .map_err(|_| ())
}

fn dispatch_catalog_control(
    runtime: &Runtime,
    events: &RithmicProviderEvents,
    publications: &SyncSender<RithmicCatalogEvent>,
    provider_generation: u64,
    searches: &mut BTreeMap<usize, u64>,
    selections: &mut BTreeMap<usize, u64>,
    control: RithmicCatalogControl,
) {
    let Some(session_generation) = active_generation(runtime) else {
        reject_catalog_control(publications, control, Some(provider_generation));
        return;
    };
    match control {
        RithmicCatalogControl::Search(search) => {
            let Some(generation) = usize_generation(search.search_generation) else {
                reject_catalog_control(
                    publications,
                    RithmicCatalogControl::Search(search),
                    Some(provider_generation),
                );
                return;
            };
            let maximum_results = usize::try_from(search.maximum_results)
                .ok()
                .and_then(NonZeroUsize::new);
            let request = maximum_results.and_then(|maximum_results| {
                RithmicSymbolSearch::try_new(
                    generation,
                    search.query,
                    None,
                    None,
                    None,
                    SearchPattern::Equals,
                    maximum_results,
                )
                .ok()
            });
            if request
                .is_some_and(|request| events.search_symbols(session_generation, request).is_ok())
            {
                searches.insert(generation.get(), search.consumer_id);
            } else {
                reject_catalog_generation(
                    publications,
                    search.consumer_id,
                    Some(provider_generation),
                    search.search_generation,
                    false,
                );
            }
        }
        RithmicCatalogControl::Select(selection) => {
            let generation = usize_generation(selection.selection_generation);
            let search_generation = usize_generation(selection.search_generation);
            let subscription = RithmicReadOnlySubscription::try_new(false, true, false).ok();
            let request = generation
                .zip(search_generation)
                .zip(subscription)
                .and_then(|((generation, search_generation), subscription)| {
                    RithmicInstrumentSelection::try_new(
                        generation,
                        search_generation,
                        selection.symbol,
                        selection.exchange,
                        selection.entitlement_id,
                        subscription,
                    )
                    .ok()
                });
            if request.is_some_and(|request| {
                events
                    .select_instrument(session_generation, request)
                    .is_ok()
            }) {
                selections.insert(
                    usize::try_from(selection.selection_generation).unwrap_or(usize::MAX),
                    selection.consumer_id,
                );
            } else {
                reject_catalog_generation(
                    publications,
                    selection.consumer_id,
                    Some(provider_generation),
                    selection.selection_generation,
                    true,
                );
            }
        }
    }
}

fn publish_catalog_callback(
    publications: &SyncSender<RithmicCatalogEvent>,
    provider_generation: u64,
    event: AdapterCatalogEvent,
    searches: &mut BTreeMap<usize, u64>,
    selections: &mut BTreeMap<usize, u64>,
) {
    match event {
        AdapterCatalogEvent::SearchCompleted {
            search_generation,
            symbols,
            ..
        } => {
            let Some(consumer_id) = searches.remove(&search_generation.get()) else {
                return;
            };
            let instruments = symbols
                .results
                .into_iter()
                .map(|result| ProviderInstrumentSummary {
                    symbol: result.symbol,
                    exchange: result.exchange,
                    name: result.name,
                    product_code: result.product_code,
                    instrument_type: result.instrument_type,
                    expiration_date: result.expiration_date,
                })
                .collect();
            let _ = publications.send(RithmicCatalogEvent::SearchCompleted(
                ProviderInstrumentSearchResult {
                    consumer_id,
                    provider: "rithmic".to_string(),
                    provider_generation,
                    search_generation: u64::try_from(search_generation.get()).unwrap_or(u64::MAX),
                    instruments,
                },
            ));
        }
        AdapterCatalogEvent::SelectionInstalled {
            selection_generation,
            instrument,
            entitlement_id,
            ..
        } => {
            let Some(consumer_id) = selections.remove(&selection_generation.get()) else {
                return;
            };
            let _ = publications.send(RithmicCatalogEvent::SelectionResolved {
                consumer_id,
                instrument: protocol_instrument(
                    provider_generation,
                    selection_generation,
                    instrument,
                    entitlement_id,
                ),
            });
        }
        AdapterCatalogEvent::CommandRejected {
            command_generation,
            reason,
            ..
        } => {
            let selection = matches!(
                reason,
                RithmicCatalogRejection::InstrumentUnavailable
                    | RithmicCatalogRejection::SubscriptionRejected
                    | RithmicCatalogRejection::SelectionDispatchUnavailable
            );
            let consumer_id = if selection {
                selections.remove(&command_generation.get())
            } else {
                searches.remove(&command_generation.get())
            };
            let Some(consumer_id) = consumer_id else {
                return;
            };
            let _ = publications.send(RithmicCatalogEvent::Rejected {
                rejection: ProviderCatalogRejected {
                    consumer_id,
                    provider: "rithmic".to_string(),
                    provider_generation: Some(provider_generation),
                    command_generation: u64::try_from(command_generation.get()).unwrap_or(u64::MAX),
                    reason: protocol_rejection(reason) as i32,
                },
                selection,
            });
        }
    }
}

fn protocol_instrument(
    provider_generation: u64,
    selection_generation: NonZeroUsize,
    instrument: InstrumentDescriptor,
    entitlement_id: String,
) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation: provider_generation,
        selection_generation: u64::try_from(selection_generation.get()).unwrap_or(u64::MAX),
        instrument_id: instrument.instrument_id,
        provider_symbol: instrument.provider_symbol,
        display_symbol: instrument.display_symbol,
        venue_id: instrument.venue_id,
        price_scale: u32::from(instrument.price_scale),
        quantity_scale: u32::from(instrument.quantity_scale),
        entitlement_id,
    }
}

fn reject_catalog_control(
    publications: &SyncSender<RithmicCatalogEvent>,
    control: RithmicCatalogControl,
    provider_generation: Option<u64>,
) {
    let (consumer_id, command_generation, selection) = match control {
        RithmicCatalogControl::Search(search) => {
            (search.consumer_id, search.search_generation, false)
        }
        RithmicCatalogControl::Select(selection) => {
            (selection.consumer_id, selection.selection_generation, true)
        }
    };
    reject_catalog_generation(
        publications,
        consumer_id,
        provider_generation,
        command_generation,
        selection,
    );
}

fn reject_catalog_generation(
    publications: &SyncSender<RithmicCatalogEvent>,
    consumer_id: u64,
    provider_generation: Option<u64>,
    command_generation: u64,
    selection: bool,
) {
    let _ = publications.send(RithmicCatalogEvent::Rejected {
        rejection: ProviderCatalogRejected {
            consumer_id,
            provider: "rithmic".to_string(),
            provider_generation,
            command_generation,
            reason: ProviderCatalogRejectionReason::DispatchUnavailable as i32,
        },
        selection,
    });
}

const fn protocol_rejection(reason: RithmicCatalogRejection) -> ProviderCatalogRejectionReason {
    match reason {
        RithmicCatalogRejection::SearchRejected => ProviderCatalogRejectionReason::SearchRejected,
        RithmicCatalogRejection::SupersededSearch => {
            ProviderCatalogRejectionReason::SupersededSearch
        }
        RithmicCatalogRejection::InstrumentUnavailable => {
            ProviderCatalogRejectionReason::InstrumentUnavailable
        }
        RithmicCatalogRejection::SubscriptionRejected => {
            ProviderCatalogRejectionReason::SubscriptionRejected
        }
        RithmicCatalogRejection::SearchDispatchUnavailable
        | RithmicCatalogRejection::SelectionDispatchUnavailable => {
            ProviderCatalogRejectionReason::DispatchUnavailable
        }
    }
}

fn active_generation(runtime: &Runtime) -> Option<SessionGeneration> {
    match runtime.state().ok()? {
        RithmicProviderRuntimeState::Connecting { generation, .. }
        | RithmicProviderRuntimeState::Streaming { generation } => Some(generation),
        RithmicProviderRuntimeState::RecoveryRequired { generation, .. } => generation,
        RithmicProviderRuntimeState::Disconnected
        | RithmicProviderRuntimeState::StopUnconfirmed { .. }
        | RithmicProviderRuntimeState::Suspended
        | RithmicProviderRuntimeState::NetworkUnavailable
        | RithmicProviderRuntimeState::Stopped => None,
    }
}

fn usize_generation(generation: u64) -> Option<NonZeroUsize> {
    usize::try_from(generation).ok().and_then(NonZeroUsize::new)
}

pub(crate) fn run(
    controls: &Receiver<RithmicRealtimeControl>,
    publications: &SyncSender<RithmicRealtimeEvent>,
) {
    let Ok(mut environment) = start_environment_monitors() else {
        while let Ok(control) = controls.recv() {
            if let RithmicRealtimeControl::Select(selected) = control {
                let _ = publications.send(RithmicRealtimeEvent::Disconnected(
                    selected.session_generation,
                ));
            }
        }
        return;
    };
    let mut last_generation = 0_u64;
    loop {
        let mut selected = loop {
            match controls.recv() {
                Ok(RithmicRealtimeControl::Select(selected)) => break selected,
                Ok(RithmicRealtimeControl::Stop) => {}
                Err(_) => return,
            }
        };
        loop {
            let mut stop_requested = false;
            while let Ok(control) = controls.try_recv() {
                match control {
                    RithmicRealtimeControl::Select(newer) => {
                        selected = newer;
                        stop_requested = false;
                    }
                    RithmicRealtimeControl::Stop => stop_requested = true,
                }
            }
            if stop_requested {
                break;
            }
            let generation = next_generation(last_generation, selected.session_generation);
            let (environment_events, environment_state) = environment.parts();
            match run_selection(
                &selected,
                generation,
                controls,
                publications,
                environment_events,
                environment_state,
            ) {
                SelectionExit::Replace {
                    selected: replacement,
                    generation,
                } => {
                    selected = replacement;
                    last_generation = generation;
                }
                SelectionExit::Idle { generation } => {
                    last_generation = generation;
                    break;
                }
                SelectionExit::Closed { generation } => {
                    let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
                    return;
                }
            }
        }
    }
}

enum SelectionExit {
    Replace {
        selected: InstallProviderInstrument,
        generation: u64,
    },
    Closed {
        generation: u64,
    },
    Idle {
        generation: u64,
    },
}

fn run_selection(
    selected: &InstallProviderInstrument,
    mut generation: u64,
    controls: &Receiver<RithmicRealtimeControl>,
    publications: &SyncSender<RithmicRealtimeEvent>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
) -> SelectionExit {
    let opened = open_runtime(selected);
    let Ok((mut runtime, events)) = opened else {
        let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
        return wait_for_replacement(controls, environment, environment_state, generation);
    };
    let _ = publications.send(RithmicRealtimeEvent::Connecting(generation));
    let mut retries = RithmicRetryScheduler::default();
    if apply_current_environment(&mut runtime, &events, &mut retries, *environment_state).is_err() {
        let _ = publications.send(RithmicRealtimeEvent::Recovering(generation));
    }
    loop {
        match poll_environment(
            environment,
            environment_state,
            &mut runtime,
            &events,
            &mut retries,
            publications,
            generation,
        ) {
            Ok(Some(updated)) => generation = updated,
            Ok(None) => {}
            Err(()) => {
                let _ = runtime.stop();
                return SelectionExit::Closed { generation };
            }
        }
        match controls.try_recv() {
            Ok(RithmicRealtimeControl::Select(replacement)) => {
                let _ = runtime.stop();
                return SelectionExit::Replace {
                    selected: replacement,
                    generation,
                };
            }
            Ok(RithmicRealtimeControl::Stop) => {
                return stop_selection(&mut runtime, generation);
            }
            Err(TryRecvError::Disconnected) => {
                let _ = runtime.stop();
                return SelectionExit::Closed { generation };
            }
            Err(TryRecvError::Empty) => {}
        }
        while events.has_ready() {
            match try_recv_rithmic_event(&mut runtime, &events, &mut retries, Instant::now()) {
                Ok(Some(AppliedRithmicEvent::Semantic(event))) => match event {
                    ProviderSessionEvent::InstrumentsDiscovered { .. } => {
                        let _ = publications.send(RithmicRealtimeEvent::Connected(generation));
                    }
                    ProviderSessionEvent::Market {
                        event: MarketEvent::Trade(mut trade),
                        ..
                    } => {
                        trade.metadata.session_generation = generation;
                        let _ = publications.send(RithmicRealtimeEvent::Trade(generation, trade));
                    }
                    ProviderSessionEvent::Market {
                        event: MarketEvent::DepthSnapshot(mut snapshot),
                        ..
                    } => {
                        snapshot.metadata.session_generation = generation;
                        let _ =
                            publications.send(RithmicRealtimeEvent::Depth(generation, snapshot));
                    }
                    ProviderSessionEvent::Heartbeat { .. } => {
                        let _ = publications.send(RithmicRealtimeEvent::Heartbeat(generation));
                    }
                    ProviderSessionEvent::DiscoveryStarted
                    | ProviderSessionEvent::SystemsDiscovered { .. }
                    | ProviderSessionEvent::AuthenticationChanged { .. }
                    | ProviderSessionEvent::Market { .. }
                    | ProviderSessionEvent::Invalidated { .. }
                    | ProviderSessionEvent::Stopped => {}
                },
                Ok(Some(AppliedRithmicEvent::RetryScheduled(_))) => {
                    let _ = publications.send(RithmicRealtimeEvent::Recovering(generation));
                }
                Ok(Some(AppliedRithmicEvent::TerminalFailure { .. })) | Err(_) => {
                    let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
                    let _ = runtime.stop();
                    return wait_for_replacement(
                        controls,
                        environment,
                        environment_state,
                        generation,
                    );
                }
                Ok(None) => break,
            }
        }
        if retries
            .retry_due(&mut runtime, Instant::now())
            .is_ok_and(|started| started.is_some())
        {
            generation = next_generation(generation, generation);
            let _ = publications.send(RithmicRealtimeEvent::Connecting(generation));
        }
        std::thread::sleep(EVENT_WAIT);
    }
}

fn stop_selection(runtime: &mut Runtime, generation: u64) -> SelectionExit {
    let _ = runtime.stop();
    SelectionExit::Idle { generation }
}

fn poll_environment(
    environment: &Receiver<EnvironmentMessage>,
    state: &mut EnvironmentState,
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    publications: &SyncSender<RithmicRealtimeEvent>,
    generation: u64,
) -> Result<Option<u64>, ()> {
    let event = match environment.try_recv() {
        Ok(EnvironmentMessage::Event(event)) => event,
        Ok(EnvironmentMessage::Failed) | Err(TryRecvError::Disconnected) => return Err(()),
        Err(TryRecvError::Empty) => return Ok(None),
    };
    state.observe(event);
    match apply_rithmic_environment_event(runtime, events, retries, event) {
        Ok(Some(_)) => {
            let generation = next_generation(generation, generation);
            let _ = publications.send(RithmicRealtimeEvent::Connecting(generation));
            Ok(Some(generation))
        }
        Ok(None) => {
            let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
            Ok(None)
        }
        Err(_) => {
            let _ = publications.send(RithmicRealtimeEvent::Recovering(generation));
            Ok(None)
        }
    }
}

fn wait_for_replacement(
    controls: &Receiver<RithmicRealtimeControl>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
    generation: u64,
) -> SelectionExit {
    loop {
        match environment.try_recv() {
            Ok(EnvironmentMessage::Event(event)) => environment_state.observe(event),
            Ok(EnvironmentMessage::Failed) | Err(TryRecvError::Disconnected) => {
                return SelectionExit::Closed { generation };
            }
            Err(TryRecvError::Empty) => {}
        }
        match controls.recv_timeout(EVENT_WAIT) {
            Ok(RithmicRealtimeControl::Select(selected)) => {
                return SelectionExit::Replace {
                    selected,
                    generation,
                };
            }
            Ok(RithmicRealtimeControl::Stop) => {
                return SelectionExit::Idle { generation };
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return SelectionExit::Closed { generation };
            }
        }
    }
}

fn apply_current_environment(
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    state: EnvironmentState,
) -> Result<(), String> {
    if state.network == NetworkEvent::Unavailable {
        apply_rithmic_environment_event(
            runtime,
            events,
            retries,
            RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable),
        )
        .map_err(|_| "Rithmic native network state could not be applied".to_string())?;
    }
    if state.suspended {
        apply_rithmic_environment_event(
            runtime,
            events,
            retries,
            RithmicEnvironmentEvent::Power(PowerEvent::Suspending),
        )
        .map_err(|_| "Rithmic native power state could not be applied".to_string())?;
    }
    if state.network != NetworkEvent::Unavailable && !state.suspended {
        runtime
            .request_connection()
            .map_err(|_| "Rithmic connection could not start".to_string())?;
    }
    Ok(())
}

fn start_environment_monitors() -> Result<EnvironmentMonitors, String> {
    let network = NativeNetworkMonitor::connect()
        .map_err(|_| "Rithmic native network monitor is unavailable".to_string())?;
    let initial_network = network.current();
    let network_cancellation = network.cancellation();
    let power = NativePowerMonitor::connect()
        .map_err(|_| "Rithmic native power monitor is unavailable".to_string())?;
    let power_cancellation = power.cancellation();
    let (sender, receiver) = mpsc::sync_channel(ENVIRONMENT_CAPACITY);
    let mut network = network;
    let network_worker = spawn_environment_monitor(
        "axiusflow-engine-rithmic-network-monitor",
        sender.clone(),
        move || {
            network
                .next_event()
                .map(RithmicEnvironmentEvent::Network)
                .map_err(|_| ())
        },
    )?;
    let mut power = power;
    let power_worker = match spawn_environment_monitor(
        "axiusflow-engine-rithmic-power-monitor",
        sender,
        move || {
            power
                .next_event()
                .map(RithmicEnvironmentEvent::Power)
                .map_err(|_| ())
        },
    ) {
        Ok(worker) => worker,
        Err(error) => {
            drop(receiver);
            network_cancellation.cancel();
            power_cancellation.cancel();
            let _ = network_worker.join();
            return Err(error);
        }
    };
    Ok(EnvironmentMonitors {
        events: Some(receiver),
        state: EnvironmentState {
            network: initial_network,
            suspended: false,
        },
        network_cancellation: Some(network_cancellation),
        power_cancellation: Some(power_cancellation),
        network_worker: Some(network_worker),
        power_worker: Some(power_worker),
    })
}

fn spawn_environment_monitor(
    name: &'static str,
    sender: SyncSender<EnvironmentMessage>,
    mut next: impl FnMut() -> Result<RithmicEnvironmentEvent, ()> + Send + 'static,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            loop {
                match next() {
                    Ok(event) if sender.send(EnvironmentMessage::Event(event)).is_ok() => {}
                    Ok(_) => return,
                    Err(()) => {
                        let _ = sender.send(EnvironmentMessage::Failed);
                        return;
                    }
                }
            }
        })
        .map_err(|_| "Rithmic native environment monitor could not start".to_string())
}

fn open_runtime(
    selected: &InstallProviderInstrument,
) -> Result<(Runtime, RithmicProviderEvents), String> {
    let descriptor = InstrumentDescriptor {
        instrument_id: selected.instrument_id.clone(),
        provider_symbol: selected.provider_symbol.clone(),
        display_symbol: selected.display_symbol.clone(),
        venue_id: selected.venue_id.clone(),
        price_scale: u8::try_from(selected.price_scale)
            .map_err(|_| "Rithmic live price scale is invalid".to_string())?,
        quantity_scale: u8::try_from(selected.quantity_scale)
            .map_err(|_| "Rithmic live quantity scale is invalid".to_string())?,
    };
    open_runtime_with_instruments(vec![RithmicProviderInstrument {
        descriptor,
        entitlement_id: selected.entitlement_id.clone(),
        trades: true,
        quotes: false,
        order_book: true,
    }])
}

fn open_catalog_runtime() -> Result<(Runtime, RithmicProviderEvents), String> {
    open_runtime_with_instruments(Vec::new())
}

fn open_runtime_with_instruments(
    instruments: Vec<RithmicProviderInstrument>,
) -> Result<(Runtime, RithmicProviderEvents), String> {
    let provider = RithmicProviderConfig::try_new(
        "Axiusflow",
        env!("CARGO_PKG_VERSION"),
        RithmicSessionLimits::default(),
        MESSAGE_SILENCE,
        instruments,
    )
    .map_err(|_| "Rithmic live provider configuration is invalid".to_string())?;
    let limits = RithmicCallbackLimits::try_new(
        nonzero(CALLBACK_CAPACITY),
        nonzero(CALLBACK_BYTES),
        NonZeroUsize::MIN,
    )
    .map_err(|_| "Rithmic live callback limits are invalid".to_string())?;
    let (driver, events) = RithmicProviderDriver::new(provider, limits);
    let vault = NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE)
        .map_err(|_| "Rithmic live credential vault is unavailable".to_string())?;
    let runtime = RithmicProviderRuntime::try_new(
        vault,
        driver,
        RITHMIC_TEST_VAULT_KEY,
        RithmicProviderRuntimeConfig::new(nonzero(MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES)),
    )
    .map_err(|_| "Rithmic live runtime is unavailable".to_string())?;
    Ok((runtime, events))
}

const fn next_generation(previous: u64, requested: u64) -> u64 {
    let incremented = previous.saturating_add(1);
    if requested > incremented {
        requested
    } else {
        incremented
    }
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::{EnvironmentState, next_generation};
    use axiusflow_platform_runtime::{NetworkEvent, PowerEvent};
    use axiusflow_rithmic_protocol_adapter::RithmicEnvironmentEvent;

    #[test]
    fn engine_generation_never_regresses_across_catalog_replacements_and_retries() {
        assert_eq!(next_generation(0, 7), 7);
        assert_eq!(next_generation(7, 7), 8);
        assert_eq!(next_generation(8, 12), 12);
        assert_eq!(next_generation(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn native_environment_state_survives_provider_selection_replacement() {
        let mut state = EnvironmentState {
            network: NetworkEvent::Available,
            suspended: false,
        };
        state.observe(RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable));
        state.observe(RithmicEnvironmentEvent::Power(PowerEvent::Suspending));
        assert_eq!(state.network, NetworkEvent::Unavailable);
        assert!(state.suspended);
        state.observe(RithmicEnvironmentEvent::Network(NetworkEvent::Available));
        state.observe(RithmicEnvironmentEvent::Power(PowerEvent::Resumed));
        assert_eq!(state.network, NetworkEvent::Available);
        assert!(!state.suspended);
    }
}
