//! Engine-owned Rithmic trade-session lifecycle.

use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    thread,
    time::{Duration, Instant},
};

use aeris_contracts::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderContractMetadata, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
    SearchProviderInstruments, SelectProviderInstrument,
};
use aeris_market_data::{DepthSnapshot, MarketEvent, MarketTrade, TopOfBookQuote};
use aeris_platform_runtime::{
    NativeCredentialVault, NativeNetworkMonitor, NativeNetworkMonitorCancellation,
    NativePowerMonitor, NativePowerMonitorCancellation, NetworkEvent, PowerEvent,
};
use aeris_rithmic_protocol_adapter::{
    AppliedRithmicEvent, InstrumentContractMetadata, InstrumentDescriptor,
    MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, MAXIMUM_RITHMIC_SEARCH_RESULTS,
    ProviderInvalidationReason, ProviderSessionEvent, RITHMIC_APPLICATION_NAME,
    RITHMIC_TEST_VAULT_KEY, RITHMIC_TEST_VAULT_SERVICE, RithmicCallbackLimits,
    RithmicCatalogEvent as AdapterCatalogEvent, RithmicCatalogRejection, RithmicEnvironmentEvent,
    RithmicInstrumentSelection, RithmicProviderCommandError, RithmicProviderConfig,
    RithmicProviderDriver, RithmicProviderEvents, RithmicProviderInstrument,
    RithmicProviderRuntime, RithmicProviderRuntimeConfig, RithmicProviderRuntimeError,
    RithmicProviderRuntimeState, RithmicReadOnlySubscription, RithmicRetryScheduler,
    RithmicSessionLimits, RithmicSymbolSearch, SearchPattern, SessionGeneration,
    apply_rithmic_environment_event, try_recv_rithmic_event,
};

use crate::market_service::{CatalogPublisher, ProviderCoordinatorWake};

const CALLBACK_CAPACITY: usize = 256;
const CALLBACK_BYTES: usize = 8 * 1024 * 1024;
const CALLBACK_DEPTH_LEVELS: usize = 4_096;
/// Tight bound for adapter/control queues that cannot wake the blocking
/// provider loop directly. Warm symbol replacement should not inherit a frame-
/// sized scheduling delay before it reaches the authenticated session.
const EVENT_WAIT: Duration = Duration::from_millis(2);
const MESSAGE_SILENCE: Duration = Duration::from_mins(2);
const ENVIRONMENT_CAPACITY: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicInstrumentDemand {
    pub(crate) instrument: InstallProviderInstrument,
    pub(crate) trades: bool,
    pub(crate) quotes: bool,
    pub(crate) order_book: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RithmicRealtimeDemand {
    pub(crate) instruments: Vec<RithmicInstrumentDemand>,
}

impl RithmicRealtimeDemand {
    fn requested_generation(&self) -> u64 {
        self.instruments
            .iter()
            .map(|demand| demand.instrument.session_generation)
            .max()
            .unwrap_or(0)
    }

    fn is_empty(&self) -> bool {
        self.instruments.is_empty()
    }
}

pub(crate) enum RithmicRealtimeControl {
    Subscribe(RithmicRealtimeDemand),
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
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
    },
}

pub(crate) enum RithmicRealtimeEvent {
    Connecting(u64),
    Failed(u64, RithmicProviderRuntimeError),
    Connected(u64),
    Trade(u64, MarketTrade),
    Quote(u64, TopOfBookQuote),
    Depth(u64, DepthSnapshot),
    Heartbeat(u64, Option<u64>),
    Recovering(u64, Option<ProviderInvalidationReason>),
    Disconnected(u64, Option<ProviderInvalidationReason>),
}

impl RithmicRealtimeEvent {
    pub(crate) fn generation(&self) -> u64 {
        match self {
            RithmicRealtimeEvent::Failed(g, _)
            | RithmicRealtimeEvent::Connecting(g)
            | RithmicRealtimeEvent::Connected(g)
            | RithmicRealtimeEvent::Recovering(g, _)
            | RithmicRealtimeEvent::Disconnected(g, _)
            | RithmicRealtimeEvent::Heartbeat(g, _)
            | RithmicRealtimeEvent::Trade(g, _)
            | RithmicRealtimeEvent::Quote(g, _)
            | RithmicRealtimeEvent::Depth(g, _) => *g,
        }
    }
}

type Runtime = RithmicProviderRuntime<NativeCredentialVault, RithmicProviderDriver>;

#[derive(Clone, Copy)]
struct ProviderChannels<'a> {
    catalog_controls: &'a Receiver<RithmicCatalogControl>,
    catalog_publications: &'a CatalogPublisher<RithmicCatalogEvent>,
    realtime_controls: &'a Receiver<RithmicRealtimeControl>,
    realtime_publications: &'a SyncSender<RithmicRealtimeEvent>,
    coordinator_wake: &'a ProviderCoordinatorWake,
    reconnect_delay: Duration,
}

impl ProviderChannels<'_> {
    fn publish_realtime(&self, event: RithmicRealtimeEvent) {
        let generation = event.generation();
        if self.coordinator_wake.overflowed("rithmic", generation) {
            return;
        }
        match self.realtime_publications.try_send(event) {
            Ok(()) => self.coordinator_wake.notify(),
            Err(TrySendError::Full(_)) => {
                self.coordinator_wake.report_overflow("rithmic", generation);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn publish_catalog(&self, event: RithmicCatalogEvent) {
        if self.catalog_publications.send(event).is_ok() {
            self.coordinator_wake.notify();
        }
    }
}

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
            panicked.push("aeris-engine-rithmic-network-monitor");
        }
        if self
            .power_worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            panicked.push("aeris-engine-rithmic-power-monitor");
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

/// One authenticated ticker-plant login together with the bookkeeping that
/// must follow it when ownership moves from catalog service to live demand.
///
/// Rithmic Test permits one ticker-plant login per account. Stopping the
/// catalog session and opening a second login for realtime left a window in
/// which every catalog command was rejected and two logins overlapped, so the
/// session is handed over intact instead.
struct CatalogHandoff {
    runtime: Runtime,
    events: RithmicProviderEvents,
    retries: RithmicRetryScheduler,
    /// Searches still awaiting a callback, keyed by search generation.
    searches: BTreeMap<usize, u64>,
    /// Selections still awaiting a callback, keyed by selection generation.
    selections: BTreeMap<usize, u64>,
}

enum CatalogSessionExit {
    /// A non-empty realtime demand arrived while the catalog session was
    /// authenticated; the same login now serves that demand. The handoff is
    /// boxed so the far more common retry and closed exits stay small.
    Demand {
        handoff: Box<CatalogHandoff>,
        generation: u64,
        demand: RithmicRealtimeDemand,
    },
    Retry(u64),
    Closed,
}

#[allow(clippy::too_many_lines)]
fn run_catalog_session(
    mut runtime: Runtime,
    events: RithmicProviderEvents,
    channels: ProviderChannels<'_>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
    mut generation: u64,
    initial_control: RithmicCatalogControl,
) -> CatalogSessionExit {
    let mut retries = RithmicRetryScheduler::default();
    let mut searches = BTreeMap::new();
    let mut selections = BTreeMap::new();
    let mut initial_control = Some(initial_control);
    if apply_current_environment(&mut runtime, &events, &mut retries, *environment_state).is_err() {
        return CatalogSessionExit::Retry(generation);
    }
    loop {
        match poll_catalog_environment(
            environment,
            environment_state,
            &mut runtime,
            &events,
            &mut retries,
        ) {
            Ok(true) => {
                if searches.is_empty() && selections.is_empty() {
                    generation = next_generation(generation, generation);
                    continue;
                }
                // Retire pending demand before restarting an offline-born
                // session so consumers can re-demand on the fresh generation.
                generation = retire_pending_catalog_generation(
                    channels.catalog_publications,
                    generation,
                    &mut searches,
                    &mut selections,
                );
                return retry_catalog_session(&mut runtime, generation);
            }
            Ok(false) => {}
            Err(()) => {
                let _ = runtime.stop();
                return CatalogSessionExit::Closed;
            }
        }
        match channels.realtime_controls.try_recv() {
            Ok(RithmicRealtimeControl::Subscribe(demand)) if !demand.is_empty() => {
                return CatalogSessionExit::Demand {
                    handoff: Box::new(CatalogHandoff {
                        runtime,
                        events,
                        retries,
                        searches,
                        selections,
                    }),
                    generation,
                    demand,
                };
            }
            Ok(RithmicRealtimeControl::Subscribe(_) | RithmicRealtimeControl::Stop)
            | Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let _ = runtime.stop();
                return CatalogSessionExit::Closed;
            }
        }
        loop {
            let control = initial_control
                .take()
                .map_or_else(|| channels.catalog_controls.try_recv(), Ok);
            match control {
                Ok(control) => dispatch_catalog_control(
                    &runtime,
                    &events,
                    channels.catalog_publications,
                    generation,
                    &mut searches,
                    &mut selections,
                    control,
                    false,
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
            if let Some(selection) = publish_catalog_callback(
                channels.catalog_publications,
                generation,
                callback.event,
                &mut searches,
                &mut selections,
            ) {
                // The login stays open: the coordinator answers a resolved
                // selection with a realtime demand for this same generation,
                // and further catalog commands keep working meanwhile.
                channels.publish_catalog(selection);
            }
        }
        if catalog_session_failed(&mut runtime, &events, &mut retries) {
            generation = retire_pending_catalog_generation(
                channels.catalog_publications,
                generation,
                &mut searches,
                &mut selections,
            );
            return retry_catalog_session(&mut runtime, generation);
        }
        let restarted = match retries.retry_due(&mut runtime, Instant::now()) {
            Ok(started) => started.is_some(),
            Err(error) => {
                eprintln!("Aeris Rithmic reconnect start failed: {error}");
                if retries.ticket().is_none() {
                    reject_pending_catalog(
                        channels.catalog_publications,
                        generation,
                        &mut searches,
                        &mut selections,
                    );
                    return retry_catalog_session(&mut runtime, generation);
                }
                false
            }
        };
        if restarted {
            generation = retire_pending_catalog_generation(
                channels.catalog_publications,
                generation,
                &mut searches,
                &mut selections,
            );
        }
        thread::sleep(EVENT_WAIT);
    }
}

fn catalog_session_failed(
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
) -> bool {
    while events.has_ready() {
        match try_recv_rithmic_event(runtime, events, retries, Instant::now()) {
            Ok(Some(AppliedRithmicEvent::RetryScheduled(_)) | None) => break,
            Ok(Some(AppliedRithmicEvent::TerminalFailure { .. })) | Err(_) => return true,
            Ok(Some(AppliedRithmicEvent::Semantic(_))) => {}
        }
    }
    false
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

fn catalog_selection_subscription(realtime: bool) -> Option<RithmicReadOnlySubscription> {
    RithmicReadOnlySubscription::try_new(realtime, true, realtime).ok()
}

#[allow(clippy::too_many_arguments)]
fn dispatch_catalog_control(
    runtime: &Runtime,
    events: &RithmicProviderEvents,
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    searches: &mut BTreeMap<usize, u64>,
    selections: &mut BTreeMap<usize, u64>,
    control: RithmicCatalogControl,
    realtime_selection: bool,
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
            let request =
                bounded_search_results(search.maximum_results).and_then(|maximum_results| {
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
            let subscription = catalog_selection_subscription(realtime_selection);
            let request = generation
                .zip(search_generation)
                .zip(subscription)
                .and_then(|((generation, search_generation), subscription)| {
                    let constructor = if realtime_selection {
                        RithmicInstrumentSelection::try_new_reference_only
                    } else {
                        RithmicInstrumentSelection::try_new
                    };
                    constructor(
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
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    event: AdapterCatalogEvent,
    searches: &mut BTreeMap<usize, u64>,
    selections: &mut BTreeMap<usize, u64>,
) -> Option<RithmicCatalogEvent> {
    match event {
        AdapterCatalogEvent::SearchCompleted {
            search_generation,
            symbols,
            ..
        } => {
            let consumer_id = searches.remove(&search_generation.get())?;
            let instruments = symbols
                .results
                .into_iter()
                .map(|result| ProviderInstrumentSummary {
                    display_symbol: result.symbol.clone(),
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
            None
        }
        AdapterCatalogEvent::SelectionInstalled {
            selection_generation,
            instrument,
            entitlement_id,
            ..
        } => {
            let consumer_id = selections.remove(&selection_generation.get())?;
            Some(RithmicCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation: u64::try_from(selection_generation.get()).unwrap_or(u64::MAX),
                instrument: protocol_instrument(
                    provider_generation,
                    selection_generation,
                    instrument,
                    entitlement_id,
                ),
            })
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
            let consumer_id = consumer_id?;
            let _ = publications.send(RithmicCatalogEvent::Rejected {
                rejection: ProviderCatalogRejected {
                    consumer_id,
                    provider: "rithmic".to_string(),
                    provider_generation: Some(provider_generation),
                    command_generation: u64::try_from(command_generation.get()).unwrap_or(u64::MAX),
                    reason: protocol_rejection(reason),
                },
                selection,
            });
            None
        }
    }
}

fn protocol_instrument(
    provider_generation: u64,
    selection_generation: NonZeroUsize,
    instrument: InstrumentDescriptor,
    entitlement_id: String,
) -> InstallProviderInstrument {
    let InstrumentDescriptor {
        instrument_id,
        provider_symbol,
        display_symbol,
        venue_id,
        price_scale,
        quantity_scale,
        price_increment,
        contract,
    } = instrument;
    let contract = contract.map(|contract| *contract);
    InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation: provider_generation,
        selection_generation: u64::try_from(selection_generation.get()).unwrap_or(u64::MAX),
        instrument_id,
        provider_symbol,
        display_symbol,
        venue_id,
        price_scale: u32::from(price_scale),
        quantity_scale: u32::from(quantity_scale),
        entitlement_id,
        price_increment,
        contract_metadata: contract.map(|value| {
            Box::new(ProviderContractMetadata {
                point_value: value.point_value.map(|value| value.0),
                point_value_scale: value.point_value.map(|value| u32::from(value.1)),
                currency: value.currency,
                contract_expiry: value.expiration_date,
                first_notice_date: value.first_notice_date,
                last_trade_date: value.last_trade_date,
                session_hours: Vec::new(),
                order_quantity_increment: None,
            })
        }),
    }
}

fn reject_catalog_control(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
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
    publications: &CatalogPublisher<RithmicCatalogEvent>,
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
            reason: ProviderCatalogRejectionReason::DispatchUnavailable,
        },
        selection,
    });
}

/// Fails every pending catalog search and selection with an actionable
/// rejection when their generation is retired. Callers restart the session
/// afterwards so re-demand lands on a generation that can actually connect.
fn reject_pending_catalog(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    searches: &mut BTreeMap<usize, u64>,
    selections: &mut BTreeMap<usize, u64>,
) {
    for (command_generation, consumer_id) in searches.iter() {
        reject_catalog_generation(
            publications,
            *consumer_id,
            Some(provider_generation),
            u64::try_from(*command_generation).unwrap_or(u64::MAX),
            false,
        );
    }
    for (command_generation, consumer_id) in selections.iter() {
        reject_catalog_generation(
            publications,
            *consumer_id,
            Some(provider_generation),
            u64::try_from(*command_generation).unwrap_or(u64::MAX),
            true,
        );
    }
    searches.clear();
    selections.clear();
}

/// Retires one provider generation and resolves every command that can no
/// longer receive a callback from it. Consumers can then re-demand on the
/// fresh generation instead of waiting for their outer deadline.
fn retire_pending_catalog_generation(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    generation: u64,
    searches: &mut BTreeMap<usize, u64>,
    selections: &mut BTreeMap<usize, u64>,
) -> u64 {
    reject_pending_catalog(publications, generation, searches, selections);
    next_generation(generation, generation)
}

fn retry_catalog_session(runtime: &mut Runtime, generation: u64) -> CatalogSessionExit {
    let _ = runtime.stop();
    CatalogSessionExit::Retry(generation)
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

/// A consumer's result count is an upper bound, so it is capped at the
/// direct-session limit instead of rejecting the whole search.
fn bounded_search_results(requested: u32) -> Option<NonZeroUsize> {
    usize::try_from(requested)
        .ok()
        .map(|requested| requested.min(MAXIMUM_RITHMIC_SEARCH_RESULTS))
        .and_then(NonZeroUsize::new)
}

fn usize_generation(generation: u64) -> Option<NonZeroUsize> {
    usize::try_from(generation).ok().and_then(NonZeroUsize::new)
}

pub(crate) fn run(
    catalog_controls: &Receiver<RithmicCatalogControl>,
    catalog_publications: &CatalogPublisher<RithmicCatalogEvent>,
    realtime_controls: &Receiver<RithmicRealtimeControl>,
    realtime_publications: &SyncSender<RithmicRealtimeEvent>,
    coordinator_wake: &ProviderCoordinatorWake,
    reconnect_delay: Duration,
) {
    let channels = provider_channels(
        catalog_controls,
        catalog_publications,
        realtime_controls,
        realtime_publications,
        coordinator_wake,
        reconnect_delay,
    );
    let Ok(mut environment) = start_environment_monitors() else {
        reject_unavailable_provider(channels);
        return;
    };
    let mut last_generation = 0_u64;
    loop {
        match realtime_controls.try_recv() {
            Ok(RithmicRealtimeControl::Subscribe(demand)) if !demand.is_empty() => {
                let generation = next_generation(last_generation, demand.requested_generation());
                match serve_demand(demand, generation, None, channels, &mut environment) {
                    DemandExit::Idle(generation) => last_generation = generation,
                    DemandExit::Closed => return,
                }
            }
            Ok(RithmicRealtimeControl::Subscribe(_) | RithmicRealtimeControl::Stop)
            | Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return,
        }

        match handle_idle_catalog(&mut environment, channels, last_generation) {
            IdleCatalogOutcome::Unchanged => {}
            IdleCatalogOutcome::Retired(generation) => last_generation = generation,
            IdleCatalogOutcome::Demand {
                handoff,
                generation,
                demand,
            } => {
                let generation = handoff_generation(generation, demand.requested_generation());
                match serve_demand(
                    demand,
                    generation,
                    Some(handoff),
                    channels,
                    &mut environment,
                ) {
                    DemandExit::Idle(generation) => last_generation = generation,
                    DemandExit::Closed => return,
                }
            }
            IdleCatalogOutcome::Closed => return,
        }
    }
}

enum DemandExit {
    /// Demand ended; the last generation label used.
    Idle(u64),
    Closed,
}

/// Serves realtime demand until it is stopped or emptied, replacing the
/// session only after a transport failure. A handed-over catalog login is used
/// for the first session instead of opening a second ticker-plant login.
fn serve_demand(
    mut demand: RithmicRealtimeDemand,
    mut generation: u64,
    mut handoff: Option<Box<CatalogHandoff>>,
    channels: ProviderChannels<'_>,
    environment: &mut EnvironmentMonitors,
) -> DemandExit {
    let mut transport_recovery = false;
    loop {
        let mut stop_requested = false;
        while let Ok(control) = channels.realtime_controls.try_recv() {
            match control {
                RithmicRealtimeControl::Subscribe(newer) => {
                    demand = newer;
                    stop_requested = false;
                }
                RithmicRealtimeControl::Stop => stop_requested = true,
            }
        }
        if stop_requested || demand.is_empty() {
            if let Some(mut handoff) = handoff.take() {
                reject_pending_catalog(
                    channels.catalog_publications,
                    generation,
                    &mut handoff.searches,
                    &mut handoff.selections,
                );
                let _ = handoff.runtime.stop();
            }
            return DemandExit::Idle(generation);
        }
        let reconnect_delay = reconnect_backoff_delay(transport_recovery, channels.reconnect_delay);
        if !reconnect_delay.is_zero() {
            thread::park_timeout(reconnect_delay);
        }
        let (environment_events, environment_state) = environment.parts();
        match run_demand(
            demand,
            generation,
            handoff.take(),
            channels,
            environment_events,
            environment_state,
        ) {
            SelectionExit::Replace {
                demand: replacement,
                generation: replaced,
                transport_recovery: replacement_transport_recovery,
            } => {
                demand = replacement;
                generation = next_generation(replaced, demand.requested_generation());
                transport_recovery = replacement_transport_recovery;
            }
            SelectionExit::Idle { generation } => {
                channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
                return DemandExit::Idle(generation);
            }
            SelectionExit::Closed { generation } => {
                channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
                return DemandExit::Closed;
            }
        }
    }
}

/// Generation label for demand served by a handed-over catalog login.
///
/// The live session keeps its catalog label unless the demand was issued
/// under a newer one; labels never regress, and the session is not replaced
/// merely because the label moves forward.
const fn handoff_generation(session: u64, requested: u64) -> u64 {
    if requested > session {
        requested
    } else {
        session
    }
}

enum IdleCatalogOutcome {
    Unchanged,
    /// The catalog session ended without demand; the last generation label used.
    Retired(u64),
    Demand {
        handoff: Box<CatalogHandoff>,
        generation: u64,
        demand: RithmicRealtimeDemand,
    },
    Closed,
}

fn handle_idle_catalog(
    environment: &mut EnvironmentMonitors,
    channels: ProviderChannels<'_>,
    last_generation: u64,
) -> IdleCatalogOutcome {
    let control = match channels.catalog_controls.recv_timeout(EVENT_WAIT) {
        Ok(control) => control,
        Err(RecvTimeoutError::Timeout) => return IdleCatalogOutcome::Unchanged,
        Err(RecvTimeoutError::Disconnected) => return IdleCatalogOutcome::Closed,
    };
    let Ok((runtime, events)) = open_catalog_runtime() else {
        reject_catalog_control(channels.catalog_publications, control, None);
        thread::park_timeout(channels.reconnect_delay);
        return IdleCatalogOutcome::Unchanged;
    };
    let generation = next_generation(last_generation, last_generation);
    let (environment_events, environment_state) = environment.parts();
    match run_catalog_session(
        runtime,
        events,
        channels,
        environment_events,
        environment_state,
        generation,
        control,
    ) {
        CatalogSessionExit::Demand {
            handoff,
            generation,
            demand,
        } => IdleCatalogOutcome::Demand {
            handoff,
            generation,
            demand,
        },
        CatalogSessionExit::Retry(generation) => IdleCatalogOutcome::Retired(generation),
        CatalogSessionExit::Closed => IdleCatalogOutcome::Closed,
    }
}

const fn provider_channels<'a>(
    catalog_controls: &'a Receiver<RithmicCatalogControl>,
    catalog_publications: &'a CatalogPublisher<RithmicCatalogEvent>,
    realtime_controls: &'a Receiver<RithmicRealtimeControl>,
    realtime_publications: &'a SyncSender<RithmicRealtimeEvent>,
    coordinator_wake: &'a ProviderCoordinatorWake,
    reconnect_delay: Duration,
) -> ProviderChannels<'a> {
    ProviderChannels {
        catalog_controls,
        catalog_publications,
        realtime_controls,
        realtime_publications,
        coordinator_wake,
        reconnect_delay,
    }
}

fn reject_unavailable_provider(channels: ProviderChannels<'_>) {
    loop {
        while let Ok(control) = channels.catalog_controls.try_recv() {
            reject_catalog_control(channels.catalog_publications, control, None);
        }
        match channels.realtime_controls.recv_timeout(EVENT_WAIT) {
            Ok(RithmicRealtimeControl::Subscribe(demand)) if !demand.is_empty() => {
                channels.publish_realtime(RithmicRealtimeEvent::Disconnected(
                    demand.requested_generation(),
                    None,
                ));
            }
            Ok(RithmicRealtimeControl::Subscribe(_) | RithmicRealtimeControl::Stop)
            | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

enum SelectionExit {
    Replace {
        demand: RithmicRealtimeDemand,
        generation: u64,
        transport_recovery: bool,
    },
    Closed {
        generation: u64,
    },
    Idle {
        generation: u64,
    },
}

const fn reconnect_backoff_delay(transport_recovery: bool, configured: Duration) -> Duration {
    if transport_recovery {
        configured
    } else {
        Duration::ZERO
    }
}

fn publish_live_catalog_selection(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    coordinator_wake: Option<&ProviderCoordinatorWake>,
    selection: RithmicCatalogEvent,
) {
    if publications.send(selection).is_ok()
        && let Some(coordinator_wake) = coordinator_wake
    {
        coordinator_wake.notify();
    }
}

fn handle_live_catalog(
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    channels: ProviderChannels<'_>,
    generation: u64,
    searches: &mut BTreeMap<usize, u64>,
    selections: &mut BTreeMap<usize, u64>,
) -> Option<SelectionExit> {
    loop {
        match channels.catalog_controls.try_recv() {
            Ok(control) => dispatch_catalog_control(
                runtime,
                events,
                channels.catalog_publications,
                generation,
                searches,
                selections,
                control,
                true,
            ),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                let _ = runtime.stop();
                return Some(SelectionExit::Closed { generation });
            }
        }
    }
    while let Some(callback) = events.try_recv_catalog() {
        if active_generation(runtime) != Some(callback.generation) {
            continue;
        }
        if let Some(selection) = publish_catalog_callback(
            channels.catalog_publications,
            generation,
            callback.event,
            searches,
            selections,
        ) {
            publish_live_catalog_selection(
                channels.catalog_publications,
                Some(channels.coordinator_wake),
                selection,
            );
        }
    }
    None
}

/// Session state entering live demand: either the catalog login handed over
/// intact or a fresh login that still has to connect.
struct LiveSessionStart {
    runtime: Runtime,
    events: RithmicProviderEvents,
    retries: RithmicRetryScheduler,
    searches: BTreeMap<usize, u64>,
    selections: BTreeMap<usize, u64>,
    /// Adapter generation already streaming, when the login is handed over.
    streaming: Option<SessionGeneration>,
}

fn live_session_start(handoff: Option<Box<CatalogHandoff>>) -> Result<LiveSessionStart, String> {
    if let Some(handoff) = handoff {
        let handoff = *handoff;
        let streaming = match handoff.runtime.state() {
            Ok(RithmicProviderRuntimeState::Streaming { generation }) => Some(generation),
            _ => None,
        };
        Ok(LiveSessionStart {
            runtime: handoff.runtime,
            events: handoff.events,
            retries: handoff.retries,
            searches: handoff.searches,
            selections: handoff.selections,
            streaming,
        })
    } else {
        let (runtime, events) = open_catalog_runtime()?;
        Ok(LiveSessionStart {
            runtime,
            events,
            retries: RithmicRetryScheduler::default(),
            searches: BTreeMap::new(),
            selections: BTreeMap::new(),
            streaming: None,
        })
    }
}

#[allow(clippy::too_many_lines)]
fn run_demand(
    mut demand: RithmicRealtimeDemand,
    mut generation: u64,
    handoff: Option<Box<CatalogHandoff>>,
    channels: ProviderChannels<'_>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
) -> SelectionExit {
    let handed_over = handoff.is_some();
    let Ok(start) = live_session_start(handoff) else {
        channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
        return wait_for_replacement(
            channels.realtime_controls,
            environment,
            environment_state,
            generation,
            true,
        );
    };
    let LiveSessionStart {
        mut runtime,
        events,
        mut retries,
        mut searches,
        mut selections,
        streaming,
    } = start;
    channels.publish_realtime(RithmicRealtimeEvent::Connecting(generation));
    let mut subscription_generation = streaming;
    let mut subscription_dirty = true;
    if streaming.is_some() {
        // The handed-over login already discovered its instruments; the
        // catalog loop consumed that callback, so report readiness here.
        channels.publish_realtime(RithmicRealtimeEvent::Connected(generation));
    }
    if !handed_over
        && apply_current_environment(&mut runtime, &events, &mut retries, *environment_state)
            .is_err()
    {
        channels.publish_realtime(RithmicRealtimeEvent::Recovering(generation, None));
    }
    loop {
        if channels.coordinator_wake.overflowed("rithmic", generation) {
            let _ = runtime.stop();
            return SelectionExit::Replace {
                demand,
                generation,
                transport_recovery: true,
            };
        }
        match poll_environment(
            environment,
            environment_state,
            &mut runtime,
            &events,
            &mut retries,
            channels,
            generation,
        ) {
            Ok(Some(updated)) => {
                generation = updated;
                subscription_generation = None;
                subscription_dirty = true;
            }
            Ok(None) => {}
            Err(()) => {
                let _ = runtime.stop();
                return SelectionExit::Closed { generation };
            }
        }
        loop {
            match channels.realtime_controls.try_recv() {
                Ok(RithmicRealtimeControl::Subscribe(replacement)) => {
                    demand = replacement;
                    subscription_dirty = true;
                    if demand.is_empty() {
                        return stop_selection(&mut runtime, generation);
                    }
                }
                Ok(RithmicRealtimeControl::Stop) => {
                    return stop_selection(&mut runtime, generation);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = runtime.stop();
                    return SelectionExit::Closed { generation };
                }
            }
        }
        if let Some(exit) = handle_live_catalog(
            &mut runtime,
            &events,
            channels,
            generation,
            &mut searches,
            &mut selections,
        ) {
            return exit;
        }
        if let Some(exit) = drain_live_events(
            &mut runtime,
            &events,
            &mut retries,
            channels,
            (environment, &mut *environment_state),
            generation,
            &mut subscription_generation,
        ) {
            return exit;
        }
        if subscription_dirty
            && subscription_generation
                .is_some_and(|ready| active_generation(&runtime) == Some(ready))
        {
            match replace_live_subscriptions(&events, subscription_generation, &demand) {
                Ok(()) => subscription_dirty = false,
                Err(RithmicProviderCommandError::QueueFull) => {}
                Err(
                    RithmicProviderCommandError::SessionUnavailable
                    | RithmicProviderCommandError::StaleGeneration,
                ) => subscription_generation = None,
                Err(RithmicProviderCommandError::InvalidRequest) => {
                    channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
                    let _ = runtime.stop();
                    return wait_for_replacement(
                        channels.realtime_controls,
                        environment,
                        environment_state,
                        generation,
                        false,
                    );
                }
            }
        }
        let restarted = match retries.retry_due(&mut runtime, Instant::now()) {
            Ok(started) => started.is_some(),
            Err(error) => {
                eprintln!("Aeris Rithmic reconnect start failed: {error}");
                if retries.ticket().is_none() {
                    channels.publish_realtime(RithmicRealtimeEvent::Failed(generation, error));
                    let _ = runtime.stop();
                    return wait_for_replacement(
                        channels.realtime_controls,
                        environment,
                        environment_state,
                        generation,
                        false,
                    );
                }
                false
            }
        };
        if restarted {
            generation = retire_pending_catalog_generation(
                channels.catalog_publications,
                generation,
                &mut searches,
                &mut selections,
            );
            subscription_generation = None;
            subscription_dirty = true;
            channels.publish_realtime(RithmicRealtimeEvent::Connecting(generation));
        }
        std::thread::sleep(EVENT_WAIT);
    }
}

fn drain_live_events(
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    channels: ProviderChannels<'_>,
    environment: (&Receiver<EnvironmentMessage>, &mut EnvironmentState),
    generation: u64,
    subscription_generation: &mut Option<SessionGeneration>,
) -> Option<SelectionExit> {
    let (environment, environment_state) = environment;
    let mut pending_depth = None;
    for _ in (0..CALLBACK_CAPACITY).take_while(|_| {
        events.has_ready() && !channels.coordinator_wake.overflowed("rithmic", generation)
    }) {
        match try_recv_rithmic_event(runtime, events, retries, Instant::now()) {
            Ok(Some(AppliedRithmicEvent::Semantic(event))) => match event {
                ProviderSessionEvent::InstrumentsDiscovered {
                    generation: ready_generation,
                    ..
                } => {
                    publish_pending_depth(channels, generation, &mut pending_depth);
                    *subscription_generation = Some(ready_generation);
                    channels.publish_realtime(RithmicRealtimeEvent::Connected(generation));
                }
                ProviderSessionEvent::Market {
                    event: MarketEvent::Trade(mut trade),
                    ..
                } => {
                    publish_pending_depth(channels, generation, &mut pending_depth);
                    trade.metadata.session_generation = generation;
                    channels.publish_realtime(RithmicRealtimeEvent::Trade(generation, trade));
                }
                ProviderSessionEvent::Market {
                    event: MarketEvent::Quote(mut quote),
                    ..
                } => {
                    publish_pending_depth(channels, generation, &mut pending_depth);
                    quote.metadata.session_generation = generation;
                    channels.publish_realtime(RithmicRealtimeEvent::Quote(generation, quote));
                }
                ProviderSessionEvent::Market {
                    event: MarketEvent::DepthSnapshot(mut snapshot),
                    ..
                } => {
                    snapshot.metadata.session_generation = generation;
                    if let Some(previous) = queue_depth_snapshot(&mut pending_depth, snapshot) {
                        channels
                            .publish_realtime(RithmicRealtimeEvent::Depth(generation, previous));
                    }
                }
                ProviderSessionEvent::Heartbeat {
                    transport_rtt_nanos,
                    ..
                } => {
                    publish_pending_depth(channels, generation, &mut pending_depth);
                    publish_heartbeat(channels, generation, transport_rtt_nanos);
                }
                ProviderSessionEvent::DiscoveryStarted
                | ProviderSessionEvent::SystemsDiscovered { .. }
                | ProviderSessionEvent::AuthenticationChanged { .. }
                | ProviderSessionEvent::Market { .. }
                | ProviderSessionEvent::Invalidated { .. }
                | ProviderSessionEvent::Stopped => {
                    publish_pending_depth(channels, generation, &mut pending_depth);
                }
            },
            Ok(Some(AppliedRithmicEvent::RetryScheduled(ticket))) => {
                publish_pending_depth(channels, generation, &mut pending_depth);
                eprintln!("Aeris Rithmic live session recovering: {:?}", ticket.reason);
                *subscription_generation = None;
                let reason = Some(ticket.reason);
                channels.publish_realtime(RithmicRealtimeEvent::Recovering(generation, reason));
            }
            Ok(Some(AppliedRithmicEvent::TerminalFailure { reason, .. })) => {
                publish_pending_depth(channels, generation, &mut pending_depth);
                eprintln!("Aeris Rithmic live session failed: {reason:?}");
                channels
                    .publish_realtime(RithmicRealtimeEvent::Disconnected(generation, Some(reason)));
                let _ = runtime.stop();
                return Some(wait_for_replacement(
                    channels.realtime_controls,
                    environment,
                    environment_state,
                    generation,
                    true,
                ));
            }
            Err(error) => {
                publish_pending_depth(channels, generation, &mut pending_depth);
                eprintln!("Aeris Rithmic live callback failed: {error}");
                channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
                let _ = runtime.stop();
                return Some(wait_for_replacement(
                    channels.realtime_controls,
                    environment,
                    environment_state,
                    generation,
                    true,
                ));
            }
            Ok(None) => {
                publish_pending_depth(channels, generation, &mut pending_depth);
                break;
            }
        }
    }
    publish_pending_depth(channels, generation, &mut pending_depth);
    None
}

fn queue_depth_snapshot(
    pending: &mut Option<DepthSnapshot>,
    next: DepthSnapshot,
) -> Option<DepthSnapshot> {
    if let Some(current) = pending.as_ref()
        && current.metadata.provider_id == next.metadata.provider_id
        && current.metadata.instrument_id == next.metadata.instrument_id
        && current.metadata.entitlement_id == next.metadata.entitlement_id
        && current.metadata.session_generation == next.metadata.session_generation
    {
        if next.metadata.source_sequence > current.metadata.source_sequence {
            *pending = Some(next);
        }
        return None;
    }
    pending.replace(next)
}

fn publish_pending_depth(
    channels: ProviderChannels<'_>,
    generation: u64,
    pending: &mut Option<DepthSnapshot>,
) {
    if let Some(snapshot) = pending.take() {
        channels.publish_realtime(RithmicRealtimeEvent::Depth(generation, snapshot));
    }
}

fn publish_heartbeat(
    channels: ProviderChannels<'_>,
    generation: u64,
    transport_rtt_nanos: Option<u64>,
) {
    channels.publish_realtime(RithmicRealtimeEvent::Heartbeat(
        generation,
        transport_rtt_nanos,
    ));
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
    channels: ProviderChannels<'_>,
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
            channels.publish_realtime(RithmicRealtimeEvent::Connecting(generation));
            Ok(Some(generation))
        }
        Ok(None) => {
            channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
            Ok(None)
        }
        Err(_) => {
            channels.publish_realtime(RithmicRealtimeEvent::Recovering(generation, None));
            Ok(None)
        }
    }
}

fn wait_for_replacement(
    controls: &Receiver<RithmicRealtimeControl>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
    generation: u64,
    transport_recovery: bool,
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
            Ok(RithmicRealtimeControl::Subscribe(demand)) if !demand.is_empty() => {
                return SelectionExit::Replace {
                    demand,
                    generation,
                    transport_recovery,
                };
            }
            Ok(RithmicRealtimeControl::Subscribe(_) | RithmicRealtimeControl::Stop) => {
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
        "aeris-engine-rithmic-network-monitor",
        sender.clone(),
        move || {
            network
                .next_event()
                .map(RithmicEnvironmentEvent::Network)
                .map_err(|_| ())
        },
    )?;
    let mut power = power;
    let power_worker =
        match spawn_environment_monitor("aeris-engine-rithmic-power-monitor", sender, move || {
            power
                .next_event()
                .map(RithmicEnvironmentEvent::Power)
                .map_err(|_| ())
        }) {
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

fn provider_instrument(
    demand: &RithmicInstrumentDemand,
) -> Result<RithmicProviderInstrument, RithmicProviderCommandError> {
    let selected = &demand.instrument;
    let descriptor = InstrumentDescriptor {
        instrument_id: selected.instrument_id.clone(),
        provider_symbol: selected.provider_symbol.clone(),
        display_symbol: selected.display_symbol.clone(),
        venue_id: selected.venue_id.clone(),
        price_scale: u8::try_from(selected.price_scale)
            .map_err(|_| RithmicProviderCommandError::InvalidRequest)?,
        quantity_scale: u8::try_from(selected.quantity_scale)
            .map_err(|_| RithmicProviderCommandError::InvalidRequest)?,
        price_increment: selected.price_increment,
        contract: selected.contract_metadata.as_deref().map(|metadata| {
            Box::new(InstrumentContractMetadata {
                point_value: metadata
                    .point_value
                    .zip(metadata.point_value_scale)
                    .and_then(|(value, scale)| {
                        u8::try_from(scale).ok().map(|scale| (value, scale))
                    }),
                currency: metadata.currency.clone(),
                expiration_date: metadata.contract_expiry.clone(),
                first_notice_date: metadata.first_notice_date.clone(),
                last_trade_date: metadata.last_trade_date.clone(),
            })
        }),
    };
    Ok(RithmicProviderInstrument {
        descriptor,
        entitlement_id: selected.entitlement_id.clone(),
        trades: demand.trades,
        quotes: demand.quotes,
        order_book: demand.order_book,
    })
}

fn replace_live_subscriptions(
    events: &RithmicProviderEvents,
    generation: Option<SessionGeneration>,
    demand: &RithmicRealtimeDemand,
) -> Result<(), RithmicProviderCommandError> {
    let generation = generation.ok_or(RithmicProviderCommandError::SessionUnavailable)?;
    let instruments = demand
        .instruments
        .iter()
        .map(provider_instrument)
        .collect::<Result<Vec<_>, _>>()?;
    events.replace_subscriptions(generation, instruments)
}

fn open_catalog_runtime() -> Result<(Runtime, RithmicProviderEvents), String> {
    open_runtime_with_instruments(Vec::new())
}

fn open_runtime_with_instruments(
    instruments: Vec<RithmicProviderInstrument>,
) -> Result<(Runtime, RithmicProviderEvents), String> {
    let provider = RithmicProviderConfig::try_new(
        RITHMIC_APPLICATION_NAME,
        env!("CARGO_PKG_VERSION"),
        RithmicSessionLimits::default(),
        MESSAGE_SILENCE,
        instruments,
    )
    .map_err(|_| "Rithmic live provider configuration is invalid".to_string())?;
    let limits = RithmicCallbackLimits::try_new(
        nonzero(CALLBACK_CAPACITY),
        nonzero(CALLBACK_BYTES),
        nonzero(CALLBACK_DEPTH_LEVELS),
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
    use std::{
        collections::BTreeMap,
        num::{NonZeroU64, NonZeroUsize},
        sync::mpsc,
    };

    use super::{
        EnvironmentState, MAXIMUM_RITHMIC_SEARCH_RESULTS, RithmicCatalogEvent,
        bounded_search_results, catalog_selection_subscription, next_generation,
        publish_catalog_callback, queue_depth_snapshot, reject_catalog_generation,
        reject_pending_catalog, retire_pending_catalog_generation,
    };
    use aeris_market_data::{DepthSnapshot, EventMetadata, QualifiedTimestamp};
    use aeris_platform_runtime::{NetworkEvent, PowerEvent};
    use aeris_rithmic_protocol_adapter::{
        RithmicCatalogEvent as AdapterCatalogEvent, RithmicEnvironmentEvent,
    };

    #[test]
    fn oversized_consumer_search_bound_is_capped_at_the_session_limit() {
        assert_eq!(
            bounded_search_results(256).map(NonZeroUsize::get),
            Some(MAXIMUM_RITHMIC_SEARCH_RESULTS)
        );
        assert_eq!(bounded_search_results(32).map(NonZeroUsize::get), Some(32));
        assert_eq!(bounded_search_results(0), None);
    }

    #[test]
    fn engine_generation_never_regresses_across_catalog_replacements_and_retries() {
        assert_eq!(next_generation(0, 7), 7);
        assert_eq!(next_generation(7, 7), 8);
        assert_eq!(next_generation(8, 12), 12);
        assert_eq!(next_generation(u64::MAX, 1), u64::MAX);
    }

    fn depth_snapshot(instrument_id: &str, source_sequence: u64) -> DepthSnapshot {
        DepthSnapshot {
            metadata: EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: instrument_id.to_string(),
                entitlement_id: format!("rithmic-test:{instrument_id}"),
                source_sequence,
                session_generation: 7,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(
                        i64::try_from(source_sequence).expect("test sequence"),
                    ),
                    provider_unix_nanos: None,
                    received_unix_nanos: i64::try_from(source_sequence).expect("test sequence") + 1,
                },
            },
            bids: Vec::new(),
            asks: Vec::new(),
        }
    }

    #[test]
    fn consecutive_depth_snapshots_keep_only_newest_complete_image_per_instrument() {
        let mut pending = None;
        assert!(queue_depth_snapshot(&mut pending, depth_snapshot("MNQ", 1)).is_none());
        assert!(queue_depth_snapshot(&mut pending, depth_snapshot("MNQ", 2)).is_none());
        assert!(
            queue_depth_snapshot(&mut pending, depth_snapshot("MNQ", 1)).is_none(),
            "a stale complete image must not replace the newer pending image"
        );
        assert_eq!(
            pending
                .as_ref()
                .map(|snapshot| snapshot.metadata.source_sequence),
            Some(2)
        );

        let emitted = queue_depth_snapshot(&mut pending, depth_snapshot("ES", 3))
            .expect("a different instrument flushes the previous complete image");
        assert_eq!(emitted.metadata.instrument_id, "MNQ");
        assert_eq!(emitted.metadata.source_sequence, 2);
        assert_eq!(
            pending
                .as_ref()
                .map(|snapshot| snapshot.metadata.instrument_id.as_str()),
            Some("ES")
        );
    }

    #[test]
    fn resolved_selection_is_deferred_to_the_session_owner() {
        let (publications, published) = mpsc::sync_channel(1);
        let publications = super::CatalogPublisher::new(
            publications,
            "rithmic",
            crate::market_service::ProviderCoordinatorWake::for_tests(),
        );
        let mut searches = BTreeMap::new();
        let mut selections = BTreeMap::from([(1, 41)]);
        let callback = AdapterCatalogEvent::selection_installed(
            NonZeroU64::MIN,
            NonZeroUsize::MIN,
            "MNQ-CME".to_string(),
            "MNQU6".to_string(),
            "MNQ Sep 2026".to_string(),
            "CME".to_string(),
            2,
            0,
            "rithmic-test:CME:MNQU6".to_string(),
        )
        .expect("valid selection callback");

        let deferred =
            publish_catalog_callback(&publications, 7, callback, &mut searches, &mut selections)
                .expect("selection is deferred to the session owner");

        assert!(published.try_recv().is_err());
        assert!(matches!(
            deferred,
            RithmicCatalogEvent::SelectionResolved {
                consumer_id: 41,
                command_generation: 1,
                instrument
            } if instrument.session_generation == 7
                && instrument.provider_symbol == "MNQU6"
        ));
    }

    #[test]
    fn live_catalog_selection_uses_full_realtime_subscription() {
        let live = catalog_selection_subscription(true).expect("live subscription validates");
        assert!(live.trades());
        assert!(live.quotes());
        assert!(live.order_book());

        let catalog =
            catalog_selection_subscription(false).expect("catalog-only subscription validates");
        assert!(!catalog.trades());
        assert!(catalog.quotes());
        assert!(!catalog.order_book());
    }

    #[test]
    fn unconfirmed_catalog_close_rejects_the_deferred_selection() {
        let (publications, published) = mpsc::sync_channel(1);
        let publications = super::CatalogPublisher::new(
            publications,
            "rithmic",
            crate::market_service::ProviderCoordinatorWake::for_tests(),
        );

        reject_catalog_generation(&publications, 41, Some(7), 3, true);

        assert!(matches!(
            published.recv().expect("close failure is published"),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: true,
            } if rejection.consumer_id == 41
                && rejection.provider_generation == Some(7)
                && rejection.command_generation == 3
        ));
    }

    #[test]
    fn retired_generation_rejects_pending_searches_and_selections() {
        let (publications, published) = mpsc::sync_channel(4);
        let publications = super::CatalogPublisher::new(
            publications,
            "rithmic",
            crate::market_service::ProviderCoordinatorWake::for_tests(),
        );
        let mut searches = BTreeMap::from([(2, 41)]);
        let mut selections = BTreeMap::from([(3, 42)]);

        reject_pending_catalog(&publications, 9, &mut searches, &mut selections);

        assert!(searches.is_empty());
        assert!(selections.is_empty());
        let mut rejections = [
            published.recv().expect("search rejection is published"),
            published.recv().expect("selection rejection is published"),
        ];
        rejections.sort_by_key(|event| match event {
            RithmicCatalogEvent::Rejected { selection, .. } => *selection,
            _ => true,
        });
        assert!(matches!(
            &rejections[0],
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: false,
            } if rejection.consumer_id == 41
                && rejection.provider_generation == Some(9)
                && rejection.command_generation == 2
        ));
        assert!(matches!(
            &rejections[1],
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: true,
            } if rejection.consumer_id == 42
                && rejection.provider_generation == Some(9)
                && rejection.command_generation == 3
        ));
        assert!(published.try_recv().is_err());
    }

    #[test]
    fn retry_advances_generation_and_rejects_every_pending_catalog_command() {
        let (publications, published) = mpsc::sync_channel(2);
        let publications = super::CatalogPublisher::new(
            publications,
            "rithmic",
            crate::market_service::ProviderCoordinatorWake::for_tests(),
        );
        let mut searches = BTreeMap::from([(2, 41)]);
        let mut selections = BTreeMap::from([(3, 42)]);

        let generation =
            retire_pending_catalog_generation(&publications, 9, &mut searches, &mut selections);

        assert_eq!(generation, 10);
        assert!(searches.is_empty());
        assert!(selections.is_empty());
        let rejections = [
            published.recv().expect("search rejection is published"),
            published.recv().expect("selection rejection is published"),
        ];
        assert!(rejections.iter().all(|event| matches!(
            event,
            RithmicCatalogEvent::Rejected { rejection, .. }
                if rejection.provider_generation == Some(9)
        )));
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
