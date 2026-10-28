//! Engine-owned Rithmic trade-session lifecycle.

use aeris_observability::diagnostic;
use std::{
    collections::{BTreeMap, VecDeque},
    mem,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderInstrumentKind,
    ProviderInstrumentSearchResult, ProviderInstrumentSummary, SearchProviderInstruments,
    SelectProviderInstrument,
};
use aeris_contracts::{InstallProviderInstrument, ProviderContractMetadata};
use aeris_market_data::{
    DepthSnapshot, MarketEvent, MarketTrade, OrderLevelEvent, OrderLevelUpdate, TopOfBookQuote,
};
use aeris_platform_runtime::{
    NativeCredentialVault, NativeNetworkMonitor, NativeNetworkMonitorCancellation,
    NativePowerMonitor, NativePowerMonitorCancellation, NetworkEvent, PowerEvent,
};
use aeris_rithmic_protocol_adapter::{
    AppliedRithmicEvent, InstrumentContractMetadata, InstrumentDescriptor, InstrumentType,
    MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, MAXIMUM_RITHMIC_RETAINED_SEARCHES,
    MAXIMUM_RITHMIC_SEARCH_RESULTS, ProductCodeResult, ProviderInvalidationReason,
    ProviderSessionEvent, RITHMIC_APPLICATION_NAME, RITHMIC_LOGIN_VAULT_KEY, RITHMIC_VAULT_SERVICE,
    RithmicCallbackLimits, RithmicCatalogEvent as AdapterCatalogEvent, RithmicCatalogRejection,
    RithmicEnvironmentEvent, RithmicInstrumentSelection, RithmicProductListing,
    RithmicProviderCommandError, RithmicProviderConfig, RithmicProviderDriver,
    RithmicProviderEvents, RithmicProviderInstrument, RithmicProviderRuntime,
    RithmicProviderRuntimeConfig, RithmicProviderRuntimeError, RithmicProviderRuntimeState,
    RithmicReadOnlySubscription, RithmicRetryScheduler, RithmicSessionLimits, RithmicSymbolSearch,
    SearchPattern, SessionGeneration, SymbolSearchResult, apply_rithmic_environment_event,
    try_recv_rithmic_event,
};

use crate::market_service::{CatalogPublisher, ProviderCoordinatorWake};

const CALLBACK_CAPACITY: usize = 256;
pub(crate) const WORKER_NAME: &str = "aeris-rithmic-provider";
const CALLBACK_BYTES: usize = 8 * 1024 * 1024;
const CALLBACK_DEPTH_LEVELS: usize = 4_096;
/// Tight bound for adapter/control queues that cannot wake the blocking
/// provider loop directly. Warm symbol replacement should not inherit a frame-
/// sized scheduling delay before it reaches the authenticated session.
// Rithmic's adapter-owned control and transport channels remain independent
// of the waitable desktop consumer outbox.
const ADAPTER_CONTROL_WAIT: Duration = Duration::from_millis(2);
const MESSAGE_SILENCE: Duration = Duration::from_mins(2);
const ENVIRONMENT_CAPACITY: usize = 8;
/// Aeris charts Rithmic futures only, and contract searches filter to them.
const LISTED_PRODUCT_ASSET_CLASS: &str = "futures";

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // Four independent provider stream subscriptions.
pub(crate) struct RithmicInstrumentDemand {
    pub(crate) instrument: InstallProviderInstrument,
    pub(crate) trades: bool,
    pub(crate) quotes: bool,
    pub(crate) order_book: bool,
    pub(crate) order_levels: bool,
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
    OrderLevels(u64, OrderLevelEvent),
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
            | RithmicRealtimeEvent::Depth(g, _)
            | RithmicRealtimeEvent::OrderLevels(g, _) => *g,
        }
    }
}

type Runtime = RithmicProviderRuntime<NativeCredentialVault, RithmicProviderDriver>;

/// Revision of the saved Rithmic login. Saving or forgetting the login
/// advances it; the provider worker then retires any session that signed in
/// under an older login, so the next connection reads the vault again. A
/// counter rather than a queued control coalesces repeated saves and can never
/// be dropped by a full control queue.
#[derive(Clone, Debug, Default)]
pub(crate) struct RithmicLoginRevision(Arc<AtomicU64>);

impl RithmicLoginRevision {
    pub(crate) fn advance(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy)]
struct ProviderChannels<'a> {
    catalog_controls: &'a Receiver<RithmicCatalogControl>,
    catalog_publications: &'a CatalogPublisher<RithmicCatalogEvent>,
    realtime_controls: &'a Receiver<RithmicRealtimeControl>,
    realtime_publications: &'a SyncSender<RithmicRealtimeEvent>,
    coordinator_wake: &'a ProviderCoordinatorWake,
    reconnect_delay: Duration,
    login: &'a RithmicLoginRevision,
}

impl ProviderChannels<'_> {
    /// Whether the saved login changed after a session read revision `seen`.
    fn login_changed(&self, seen: u64) -> bool {
        self.login.current() != seen
    }

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
/// Rithmic permits one ticker-plant login per account. Stopping the catalog
/// session and opening a second login for realtime left a window in which
/// every catalog command was rejected and two logins overlapped, so the
/// session is handed over intact instead.
struct CatalogHandoff {
    runtime: Runtime,
    events: RithmicProviderEvents,
    retries: RithmicRetryScheduler,
    /// Saved-login revision the session signed in under.
    login: u64,
    commands: CatalogCommands,
}

/// The consumer that issued one catalog command and that command's own
/// generation, which every publication answering it carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogIssuer {
    consumer_id: u64,
    generation: u64,
}

/// Why a catalog command was not sent to the adapter session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogDispatchFailure {
    NoActiveSession,
    InvalidRequest,
    WireGenerationExhausted,
    /// The selection names a search the adapter session no longer retains.
    UnretainedSearch,
    Command(RithmicProviderCommandError),
}

impl CatalogDispatchFailure {
    const fn reason(self) -> ProviderCatalogRejectionReason {
        match self {
            Self::UnretainedSearch => ProviderCatalogRejectionReason::SupersededSearch,
            Self::NoActiveSession
            | Self::InvalidRequest
            | Self::WireGenerationExhausted
            | Self::Command(_) => ProviderCatalogRejectionReason::DispatchUnavailable,
        }
    }
}

/// The ticker-login command that serves one consumer search.
#[derive(Debug, Eq, PartialEq)]
enum CatalogSearchRequest {
    Symbols(RithmicSymbolSearch),
    /// An empty query lists products; their contracts are a later symbol search.
    Products(RithmicProductListing),
}

impl CatalogSearchRequest {
    const fn generation(&self) -> NonZeroUsize {
        match self {
            Self::Symbols(search) => search.generation(),
            Self::Products(listing) => listing.generation(),
        }
    }

    const fn lane(&self) -> SearchLane {
        match self {
            Self::Symbols(_) => SearchLane::Symbols,
            Self::Products(_) => SearchLane::Listing,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchLane {
    Symbols,
    Listing,
}

impl SearchLane {
    const fn name(self) -> &'static str {
        match self {
            Self::Symbols => "symbols",
            Self::Listing => "listing",
        }
    }
}

/// The one consumer search the adapter session is answering.
#[derive(Debug)]
struct SentSearch {
    issuer: CatalogIssuer,
    wire: NonZeroUsize,
    session: SessionGeneration,
    lane: SearchLane,
    /// The consumer's exchange and result bound. A listing is requested
    /// unfiltered at the session limit so it can be cached, and these are
    /// applied to its answer instead.
    exchange: Option<String>,
    maximum_results: usize,
    sent: Instant,
}

impl SentSearch {
    fn log_answered(&self, results: usize) {
        diagnostic!(
            "Aeris Rithmic catalog {} search answered in {} ms with {results} results",
            self.lane.name(),
            self.sent.elapsed().as_millis()
        );
    }

    fn log_unanswered(&self) {
        diagnostic!(
            "Aeris Rithmic catalog {} search rejected after {} ms",
            self.lane.name(),
            self.sent.elapsed().as_millis()
        );
    }
}

/// The unfiltered product listing of one adapter session.
struct CachedListing {
    session: SessionGeneration,
    products: Vec<ProviderInstrumentSummary>,
}

/// A rejected catalog command, by lane.
enum RejectedCommand {
    Search(SentSearch),
    Selection(CatalogIssuer),
}

/// The listing products for one consumer: its exchange, then its bound.
fn listing_answer(
    products: &[ProviderInstrumentSummary],
    exchange: Option<&str>,
    maximum_results: usize,
) -> Vec<ProviderInstrumentSummary> {
    products
        .iter()
        .filter(|product| exchange.is_none_or(|exchange| product.exchange == exchange))
        .take(maximum_results)
        .cloned()
        .collect()
}

/// Catalog commands sent on the one shared ticker login.
///
/// Every consumer numbers its own searches and selections, so different
/// consumers reuse the same generations. Each command sent to the adapter
/// therefore carries a unique wire generation, and adapter callbacks are
/// routed only by that wire generation.
///
/// The adapter session answers one catalog command at a time, so at most one
/// search is sent to it. Searches arriving meanwhile wait here, at most one per
/// consumer (bounded by the coordinator's consumer registry): a consumer's
/// newer search replaces its waiting one, so typing never queues requests
/// behind a slow answer. Selections are sent immediately.
#[derive(Default)]
struct CatalogCommands {
    last_wire: Option<NonZeroUsize>,
    search: Option<SentSearch>,
    queued_searches: VecDeque<SearchProviderInstruments>,
    /// Answers empty queries without a session command until the session ends.
    listing: Option<CachedListing>,
    selections: BTreeMap<NonZeroUsize, CatalogIssuer>,
    /// Completed searches a selection may still name, oldest first. The
    /// adapter session retains the same number of completed searches.
    completed_searches: VecDeque<(CatalogIssuer, NonZeroUsize)>,
}

impl CatalogCommands {
    fn mint(&mut self) -> Result<NonZeroUsize, CatalogDispatchFailure> {
        let wire = match self.last_wire {
            None => NonZeroUsize::MIN,
            Some(last) => last
                .checked_add(1)
                .ok_or(CatalogDispatchFailure::WireGenerationExhausted)?,
        };
        self.last_wire = Some(wire);
        Ok(wire)
    }

    fn search_request(
        &mut self,
        search: &SearchProviderInstruments,
    ) -> Result<CatalogSearchRequest, CatalogDispatchFailure> {
        let maximum_results = bounded_search_results(search.maximum_results)
            .ok_or(CatalogDispatchFailure::InvalidRequest)?;
        let wire = self.mint()?;
        if search.query.trim().is_empty() {
            return RithmicProductListing::try_new(wire, nonzero(MAXIMUM_RITHMIC_SEARCH_RESULTS))
                .map(CatalogSearchRequest::Products)
                .map_err(|_| CatalogDispatchFailure::InvalidRequest);
        }
        // Rithmic symbols are upper case and the search matches them exactly.
        // Aeris charts Rithmic futures only; filtering at the provider keeps a
        // broad query's options and strategies from filling the bounded result
        // set ahead of the futures contracts.
        RithmicSymbolSearch::try_new(
            wire,
            search.query.to_ascii_uppercase(),
            search.exchange.clone(),
            None,
            Some(InstrumentType::Future),
            SearchPattern::Equals,
            maximum_results,
        )
        .map(CatalogSearchRequest::Symbols)
        .map_err(|_| CatalogDispatchFailure::InvalidRequest)
    }

    fn selection_request(
        &mut self,
        selection: &SelectProviderInstrument,
        realtime_selection: bool,
    ) -> Result<RithmicInstrumentSelection, CatalogDispatchFailure> {
        let search = self
            .completed_search(selection.consumer_id, selection.search_generation)
            .ok_or(CatalogDispatchFailure::UnretainedSearch)?;
        let subscription = catalog_selection_subscription(realtime_selection)
            .ok_or(CatalogDispatchFailure::InvalidRequest)?;
        let wire = self.mint()?;
        let constructor = if realtime_selection {
            RithmicInstrumentSelection::try_new_reference_only
        } else {
            RithmicInstrumentSelection::try_new
        };
        constructor(
            wire,
            search,
            selection.symbol.as_str(),
            selection.exchange.as_str(),
            selection.entitlement_id.as_str(),
            subscription,
        )
        .map_err(|_| CatalogDispatchFailure::InvalidRequest)
    }

    fn search_sent(
        &mut self,
        wire: NonZeroUsize,
        lane: SearchLane,
        session: SessionGeneration,
        search: &SearchProviderInstruments,
        sent: Instant,
    ) {
        self.search = Some(SentSearch {
            issuer: search_issuer(search),
            wire,
            session,
            lane,
            exchange: search.exchange.clone(),
            maximum_results: bounded_search_results(search.maximum_results)
                .map_or(0, NonZeroUsize::get),
            sent,
        });
    }

    const fn search_in_flight(&self) -> bool {
        self.search.is_some()
    }

    /// Holds a search until the one in flight is answered, returning the
    /// consumer's waiting search it replaces.
    fn queue_search(
        &mut self,
        search: SearchProviderInstruments,
    ) -> Option<SearchProviderInstruments> {
        if let Some(queued) = self
            .queued_searches
            .iter_mut()
            .find(|queued| queued.consumer_id == search.consumer_id)
        {
            return Some(mem::replace(queued, search));
        }
        self.queued_searches.push_back(search);
        None
    }

    fn remove_queued_search(&mut self, consumer_id: u64) -> Option<SearchProviderInstruments> {
        let index = self
            .queued_searches
            .iter()
            .position(|queued| queued.consumer_id == consumer_id)?;
        self.queued_searches.remove(index)
    }

    fn next_queued_search(&mut self) -> Option<SearchProviderInstruments> {
        self.queued_searches.pop_front()
    }

    /// The cached listing's answer to an empty-query search on `session`.
    fn cached_listing(
        &self,
        session: Option<SessionGeneration>,
        search: &SearchProviderInstruments,
    ) -> Option<Vec<ProviderInstrumentSummary>> {
        let listing = self.listing.as_ref()?;
        if !search.query.trim().is_empty() || session != Some(listing.session) {
            return None;
        }
        let maximum_results = bounded_search_results(search.maximum_results)?;
        Some(listing_answer(
            &listing.products,
            search.exchange.as_deref(),
            maximum_results.get(),
        ))
    }

    /// Releases the search in flight when its session is no longer active,
    /// because no callback from that session can answer it.
    fn take_lost_search(&mut self, active: Option<SessionGeneration>) -> Option<SentSearch> {
        if self
            .search
            .as_ref()
            .is_some_and(|search| Some(search.session) != active)
        {
            return self.search.take();
        }
        None
    }

    fn take_search(&mut self, wire: NonZeroUsize) -> Option<SentSearch> {
        if self
            .search
            .as_ref()
            .is_some_and(|search| search.wire == wire)
        {
            return self.search.take();
        }
        None
    }

    fn selection_sent(&mut self, wire: NonZeroUsize, selection: &SelectProviderInstrument) {
        self.selections.insert(
            wire,
            CatalogIssuer {
                consumer_id: selection.consumer_id,
                generation: selection.selection_generation,
            },
        );
    }

    /// Wire generation of the newest retained search this consumer issued
    /// under `search_generation`.
    fn completed_search(&self, consumer_id: u64, search_generation: u64) -> Option<NonZeroUsize> {
        self.completed_searches
            .iter()
            .rev()
            .find(|(issuer, _)| {
                issuer.consumer_id == consumer_id && issuer.generation == search_generation
            })
            .map(|(_, wire)| *wire)
    }

    fn search_completed(&mut self, wire: NonZeroUsize) -> Option<SentSearch> {
        let search = self.take_search(wire)?;
        if self.completed_searches.len() >= MAXIMUM_RITHMIC_RETAINED_SEARCHES {
            self.completed_searches.pop_front();
        }
        self.completed_searches.push_back((search.issuer, wire));
        Some(search)
    }

    /// Caches the session's unfiltered listing and returns the consumer's
    /// answer. A product listing holds no selectable instruments, so it is not
    /// retained and cannot evict a completed search a selection may name.
    fn listing_completed(
        &mut self,
        wire: NonZeroUsize,
        session: SessionGeneration,
        products: Vec<ProviderInstrumentSummary>,
    ) -> Option<(SentSearch, Vec<ProviderInstrumentSummary>)> {
        let search = self.take_search(wire)?;
        let answer = listing_answer(
            &products,
            search.exchange.as_deref(),
            search.maximum_results,
        );
        self.listing = Some(CachedListing { session, products });
        Some((search, answer))
    }

    fn selection_installed(&mut self, wire: NonZeroUsize) -> Option<CatalogIssuer> {
        self.selections.remove(&wire)
    }

    fn rejected(&mut self, wire: NonZeroUsize) -> Option<RejectedCommand> {
        if let Some(search) = self.take_search(wire) {
            return Some(RejectedCommand::Search(search));
        }
        self.selections
            .remove(&wire)
            .map(RejectedCommand::Selection)
    }

    fn has_pending(&self) -> bool {
        self.search.is_some() || !self.queued_searches.is_empty() || !self.selections.is_empty()
    }

    /// Ends the adapter session the commands were sent on. Returns every
    /// pending command, searches first, flagging selections, and forgets the
    /// completed searches and the cached listing because a new session
    /// retains neither.
    fn retire(&mut self) -> Vec<(CatalogIssuer, bool)> {
        self.completed_searches.clear();
        self.listing = None;
        let sent = self.search.take().map(|search| {
            search.log_unanswered();
            (search.issuer, false)
        });
        let queued = mem::take(&mut self.queued_searches)
            .into_iter()
            .map(|search| (search_issuer(&search), false));
        let selections = mem::take(&mut self.selections)
            .into_values()
            .map(|issuer| (issuer, true));
        sent.into_iter().chain(queued).chain(selections).collect()
    }
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
    let login = channels.login.current();
    let mut retries = RithmicRetryScheduler::default();
    let mut commands = CatalogCommands::default();
    let mut initial_control = Some(initial_control);
    if let Err(error) =
        apply_current_environment(&mut runtime, &events, &mut retries, *environment_state)
    {
        diagnostic!("Aeris Rithmic catalog session could not start: {error}");
        if let Some(control) = initial_control.take() {
            reject_catalog_control(channels.catalog_publications, &control, Some(generation));
        }
        return retry_catalog_session(&mut runtime, generation);
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
                let pending = commands.has_pending();
                generation = retire_pending_catalog_generation(
                    channels.catalog_publications,
                    generation,
                    &mut commands,
                );
                if pending {
                    // Restart an offline-born session after retiring its
                    // pending demand so consumers re-demand on the fresh
                    // generation.
                    return retry_catalog_session(&mut runtime, generation);
                }
                continue;
            }
            Ok(false) => {}
            Err(()) => {
                let _ = runtime.stop();
                return CatalogSessionExit::Closed;
            }
        }
        if channels.login_changed(login) {
            generation = retire_pending_catalog_generation(
                channels.catalog_publications,
                generation,
                &mut commands,
            );
            return retry_catalog_session(&mut runtime, generation);
        }
        match channels.realtime_controls.try_recv() {
            Ok(RithmicRealtimeControl::Subscribe(demand)) if !demand.is_empty() => {
                return CatalogSessionExit::Demand {
                    handoff: Box::new(CatalogHandoff {
                        runtime,
                        events,
                        retries,
                        login,
                        commands,
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
                    &AdapterSession {
                        runtime: &runtime,
                        events: &events,
                    },
                    channels.catalog_publications,
                    generation,
                    &mut commands,
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
                &mut commands,
            ) {
                // The login stays open: the coordinator answers a resolved
                // selection with a realtime demand for this same generation,
                // and further catalog commands keep working meanwhile.
                channels.publish_catalog(selection);
            }
        }
        dispatch_queued_searches(
            &AdapterSession {
                runtime: &runtime,
                events: &events,
            },
            channels.catalog_publications,
            generation,
            &mut commands,
        );
        if catalog_session_failed(&mut runtime, &events, &mut retries) {
            generation = retire_pending_catalog_generation(
                channels.catalog_publications,
                generation,
                &mut commands,
            );
            return retry_catalog_session(&mut runtime, generation);
        }
        let restarted = match retries.retry_due(&mut runtime, Instant::now()) {
            Ok(started) => started.is_some(),
            Err(error) => {
                diagnostic!("Aeris Rithmic reconnect start failed: {error}");
                if retries.ticket().is_none() {
                    reject_pending_catalog(
                        channels.catalog_publications,
                        generation,
                        &mut commands,
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
                &mut commands,
            );
        }
        thread::sleep(ADAPTER_CONTROL_WAIT);
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
            Ok(Some(AppliedRithmicEvent::TerminalFailure { reason, .. })) => {
                diagnostic!("Aeris Rithmic catalog session failed: {reason:?}");
                return true;
            }
            Err(error) => {
                diagnostic!("Aeris Rithmic catalog callback failed: {error}");
                return true;
            }
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

/// The adapter session catalog commands are sent on.
trait CatalogSession {
    fn active_generation(&self) -> Option<SessionGeneration>;

    fn search_symbols(
        &self,
        generation: SessionGeneration,
        request: RithmicSymbolSearch,
    ) -> Result<(), RithmicProviderCommandError>;

    fn list_products(
        &self,
        generation: SessionGeneration,
        request: RithmicProductListing,
    ) -> Result<(), RithmicProviderCommandError>;

    fn select_instrument(
        &self,
        generation: SessionGeneration,
        request: RithmicInstrumentSelection,
    ) -> Result<(), RithmicProviderCommandError>;
}

struct AdapterSession<'a> {
    runtime: &'a Runtime,
    events: &'a RithmicProviderEvents,
}

impl CatalogSession for AdapterSession<'_> {
    fn active_generation(&self) -> Option<SessionGeneration> {
        active_generation(self.runtime)
    }

    fn search_symbols(
        &self,
        generation: SessionGeneration,
        request: RithmicSymbolSearch,
    ) -> Result<(), RithmicProviderCommandError> {
        self.events.search_symbols(generation, request)
    }

    fn list_products(
        &self,
        generation: SessionGeneration,
        request: RithmicProductListing,
    ) -> Result<(), RithmicProviderCommandError> {
        self.events.list_products(generation, request)
    }

    fn select_instrument(
        &self,
        generation: SessionGeneration,
        request: RithmicInstrumentSelection,
    ) -> Result<(), RithmicProviderCommandError> {
        self.events.select_instrument(generation, request)
    }
}

fn dispatch_catalog_control(
    session: &impl CatalogSession,
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    commands: &mut CatalogCommands,
    control: RithmicCatalogControl,
    realtime_selection: bool,
) {
    match control {
        RithmicCatalogControl::Search(search) => {
            submit_search(session, publications, provider_generation, commands, search);
        }
        RithmicCatalogControl::Select(selection) => {
            if let Err(failure) = send_selection(session, commands, &selection, realtime_selection)
            {
                reject_undispatched(
                    publications,
                    provider_generation,
                    &RithmicCatalogControl::Select(selection),
                    failure,
                );
            }
        }
    }
}

/// Answers a search from the cached listing, holds it behind the search in
/// flight, or sends it to the session.
fn submit_search(
    session: &impl CatalogSession,
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    commands: &mut CatalogCommands,
    search: SearchProviderInstruments,
) {
    release_lost_search(session, publications, provider_generation, commands);
    if let Some(instruments) = commands.cached_listing(session.active_generation(), &search) {
        if let Some(replaced) = commands.remove_queued_search(search.consumer_id) {
            reject_superseded_search(publications, provider_generation, &replaced);
        }
        diagnostic!(
            "Aeris Rithmic catalog listing search answered from cache with {} results",
            instruments.len()
        );
        publish_search_result(
            publications,
            provider_generation,
            search_issuer(&search),
            instruments,
        );
        return;
    }
    if commands.search_in_flight() {
        if let Some(replaced) = commands.queue_search(search) {
            reject_superseded_search(publications, provider_generation, &replaced);
        }
        return;
    }
    if let Err(failure) = send_search(session, commands, &search) {
        reject_undispatched(
            publications,
            provider_generation,
            &RithmicCatalogControl::Search(search),
            failure,
        );
    }
}

/// Sends waiting searches, oldest consumer first, until one is in flight.
fn dispatch_queued_searches(
    session: &impl CatalogSession,
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    commands: &mut CatalogCommands,
) {
    if !commands.search_in_flight() && commands.queued_searches.is_empty() {
        return;
    }
    release_lost_search(session, publications, provider_generation, commands);
    while !commands.search_in_flight()
        && let Some(search) = commands.next_queued_search()
    {
        submit_search(session, publications, provider_generation, commands, search);
    }
}

fn release_lost_search(
    session: &impl CatalogSession,
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    commands: &mut CatalogCommands,
) {
    if let Some(lost) = commands.take_lost_search(session.active_generation()) {
        lost.log_unanswered();
        reject_catalog_generation(
            publications,
            lost.issuer,
            Some(provider_generation),
            false,
            ProviderCatalogRejectionReason::DispatchUnavailable,
        );
    }
}

fn reject_superseded_search(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    search: &SearchProviderInstruments,
) {
    reject_catalog_generation(
        publications,
        search_issuer(search),
        Some(provider_generation),
        false,
        ProviderCatalogRejectionReason::SupersededSearch,
    );
}

fn send_search(
    session: &impl CatalogSession,
    commands: &mut CatalogCommands,
    search: &SearchProviderInstruments,
) -> Result<(), CatalogDispatchFailure> {
    let session_generation = session
        .active_generation()
        .ok_or(CatalogDispatchFailure::NoActiveSession)?;
    let request = commands.search_request(search)?;
    let (wire, lane) = (request.generation(), request.lane());
    let sent = Instant::now();
    match request {
        CatalogSearchRequest::Symbols(request) => {
            session.search_symbols(session_generation, request)
        }
        CatalogSearchRequest::Products(request) => {
            session.list_products(session_generation, request)
        }
    }
    .map_err(CatalogDispatchFailure::Command)?;
    commands.search_sent(wire, lane, session_generation, search, sent);
    Ok(())
}

fn send_selection(
    session: &impl CatalogSession,
    commands: &mut CatalogCommands,
    selection: &SelectProviderInstrument,
    realtime_selection: bool,
) -> Result<(), CatalogDispatchFailure> {
    let session_generation = session
        .active_generation()
        .ok_or(CatalogDispatchFailure::NoActiveSession)?;
    let request = commands.selection_request(selection, realtime_selection)?;
    let wire = request.generation();
    session
        .select_instrument(session_generation, request)
        .map_err(CatalogDispatchFailure::Command)?;
    commands.selection_sent(wire, selection);
    Ok(())
}

fn publish_search_result(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    issuer: CatalogIssuer,
    instruments: Vec<ProviderInstrumentSummary>,
) {
    let _ = publications.send(RithmicCatalogEvent::SearchCompleted(
        ProviderInstrumentSearchResult {
            consumer_id: issuer.consumer_id,
            provider: "rithmic".to_string(),
            provider_generation,
            search_generation: issuer.generation,
            instruments,
        },
    ));
}

fn reject_undispatched(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    control: &RithmicCatalogControl,
    failure: CatalogDispatchFailure,
) {
    let (issuer, selection) = catalog_issuer(control);
    let lane = if selection { "selection" } else { "search" };
    diagnostic!("Aeris Rithmic catalog {lane} was not dispatched: {failure:?}");
    reject_catalog_generation(
        publications,
        issuer,
        Some(provider_generation),
        selection,
        failure.reason(),
    );
}

fn contract_summary(result: SymbolSearchResult) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        kind: if is_front_month_alias(&result) {
            ProviderInstrumentKind::FrontMonthAlias
        } else {
            ProviderInstrumentKind::Instrument
        },
        display_symbol: result.symbol.clone(),
        symbol: result.symbol,
        exchange: result.exchange,
        name: result.name,
        product_code: result.product_code,
        instrument_type: result.instrument_type,
        expiration_date: result.expiration_date,
        asset_class: None,
    }
}

fn product_summary(product: ProductCodeResult) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Product,
        symbol: product.product_code.clone(),
        display_symbol: product.product_code.clone(),
        exchange: product.exchange,
        name: product.name,
        product_code: Some(product.product_code),
        instrument_type: None,
        expiration_date: None,
        asset_class: Some(LISTED_PRODUCT_ASSET_CLASS.to_string()),
    }
}

fn publish_catalog_callback(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    provider_generation: u64,
    event: AdapterCatalogEvent,
    commands: &mut CatalogCommands,
) -> Option<RithmicCatalogEvent> {
    match event {
        AdapterCatalogEvent::SearchCompleted {
            search_generation,
            symbols,
            ..
        } => {
            let search = commands.search_completed(search_generation)?;
            let mut instruments = symbols
                .results
                .into_iter()
                .map(contract_summary)
                .collect::<Vec<_>>();
            instruments.sort_by(front_month_first);
            search.log_answered(instruments.len());
            publish_search_result(
                publications,
                provider_generation,
                search.issuer,
                instruments,
            );
            None
        }
        AdapterCatalogEvent::ProductsListed {
            session_generation,
            listing_generation,
            products,
        } => {
            // The adapter orders featured products first, then the rest by code.
            let products = products.products.into_iter().map(product_summary).collect();
            let (search, instruments) =
                commands.listing_completed(listing_generation, session_generation, products)?;
            search.log_answered(instruments.len());
            publish_search_result(
                publications,
                provider_generation,
                search.issuer,
                instruments,
            );
            None
        }
        AdapterCatalogEvent::SelectionInstalled {
            selection_generation,
            instrument,
            entitlement_id,
            ..
        } => {
            let issuer = commands.selection_installed(selection_generation)?;
            Some(RithmicCatalogEvent::SelectionResolved {
                consumer_id: issuer.consumer_id,
                command_generation: issuer.generation,
                instrument: protocol_instrument(
                    provider_generation,
                    issuer.generation,
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
            let (issuer, selection) = match commands.rejected(command_generation)? {
                RejectedCommand::Search(search) => {
                    search.log_unanswered();
                    (search.issuer, false)
                }
                RejectedCommand::Selection(issuer) => (issuer, true),
            };
            reject_catalog_generation(
                publications,
                issuer,
                Some(provider_generation),
                selection,
                protocol_rejection(reason),
            );
            None
        }
    }
}

/// Rithmic lists a product's front month a second time under the bare product
/// code ("MNQ", "Front Month for MNQ - MNQZ6.CME"). It is a moving alias, not a
/// contract, and the dated contract it points at is listed beside it.
fn is_front_month_alias(result: &SymbolSearchResult) -> bool {
    result.product_code.as_deref() == Some(result.symbol.as_str())
}

/// Orders search results by product, then each exchange's contracts front month
/// first. Rithmic returns them alphabetically, which lists far-dated contracts
/// before the active one. Its expiration dates are `YYYYMMDD`, so text order is
/// date order, and a real-time exchange such as `CME` lists all its contracts
/// before its `CME-Delayed` variant.
fn front_month_first(
    left: &ProviderInstrumentSummary,
    right: &ProviderInstrumentSummary,
) -> std::cmp::Ordering {
    left.product_code
        .cmp(&right.product_code)
        .then_with(|| left.exchange.cmp(&right.exchange))
        .then_with(|| {
            left.expiration_date
                .is_none()
                .cmp(&right.expiration_date.is_none())
        })
        .then_with(|| left.expiration_date.cmp(&right.expiration_date))
        .then_with(|| left.symbol.cmp(&right.symbol))
}

fn protocol_instrument(
    provider_generation: u64,
    selection_generation: u64,
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
        selection_generation,
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
                ..Default::default()
            })
        }),
    }
}

const fn search_issuer(search: &SearchProviderInstruments) -> CatalogIssuer {
    CatalogIssuer {
        consumer_id: search.consumer_id,
        generation: search.search_generation,
    }
}

/// The issuer of a catalog control and whether it is a selection.
const fn catalog_issuer(control: &RithmicCatalogControl) -> (CatalogIssuer, bool) {
    match control {
        RithmicCatalogControl::Search(search) => (search_issuer(search), false),
        RithmicCatalogControl::Select(selection) => (
            CatalogIssuer {
                consumer_id: selection.consumer_id,
                generation: selection.selection_generation,
            },
            true,
        ),
    }
}

fn reject_catalog_control(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    control: &RithmicCatalogControl,
    provider_generation: Option<u64>,
) {
    let (issuer, selection) = catalog_issuer(control);
    reject_catalog_generation(
        publications,
        issuer,
        provider_generation,
        selection,
        ProviderCatalogRejectionReason::DispatchUnavailable,
    );
}

fn reject_catalog_generation(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    issuer: CatalogIssuer,
    provider_generation: Option<u64>,
    selection: bool,
    reason: ProviderCatalogRejectionReason,
) {
    let _ = publications.send(RithmicCatalogEvent::Rejected {
        rejection: ProviderCatalogRejected {
            consumer_id: issuer.consumer_id,
            provider: "rithmic".to_string(),
            provider_generation,
            command_generation: issuer.generation,
            reason,
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
    commands: &mut CatalogCommands,
) {
    for (issuer, selection) in commands.retire() {
        reject_catalog_generation(
            publications,
            issuer,
            Some(provider_generation),
            selection,
            ProviderCatalogRejectionReason::DispatchUnavailable,
        );
    }
}

/// Retires one provider generation and resolves every command that can no
/// longer receive a callback from it. Consumers can then re-demand on the
/// fresh generation instead of waiting for their outer deadline.
fn retire_pending_catalog_generation(
    publications: &CatalogPublisher<RithmicCatalogEvent>,
    generation: u64,
    commands: &mut CatalogCommands,
) -> u64 {
    reject_pending_catalog(publications, generation, commands);
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

pub(crate) fn run(
    catalog_controls: &Receiver<RithmicCatalogControl>,
    catalog_publications: &CatalogPublisher<RithmicCatalogEvent>,
    realtime_controls: &Receiver<RithmicRealtimeControl>,
    realtime_publications: &SyncSender<RithmicRealtimeEvent>,
    coordinator_wake: &ProviderCoordinatorWake,
    reconnect_delay: Duration,
    login: &RithmicLoginRevision,
) {
    let channels = ProviderChannels {
        catalog_controls,
        catalog_publications,
        realtime_controls,
        realtime_publications,
        coordinator_wake,
        reconnect_delay,
        login,
    };
    let mut environment = match start_environment_monitors() {
        Ok(environment) => environment,
        Err(error) => {
            diagnostic!("Aeris Rithmic provider is unavailable: {error}");
            reject_unavailable_provider(channels);
            return;
        }
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
    let mut replacing = false;
    let mut recovery = crate::market_service::reconnect::Backoff::new(
        crate::market_service::reconnect::ReconnectPolicy::rithmic_recovery(),
        &crate::market_service::clock::SystemClock,
        0x5249_5448,
    );
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
                    &mut handoff.commands,
                );
                let _ = handoff.runtime.stop();
            }
            return DemandExit::Idle(generation);
        }
        // The adapter scheduler alone retries transport. Non-transport
        // replacements are bounded here and never race an immediate login.
        if let Some(delay) = replacement_delay(replacing, transport_recovery, &mut recovery) {
            thread::park_timeout(delay);
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
                replacing = true;
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

fn replacement_delay(
    replacing: bool,
    transport_recovery: bool,
    recovery: &mut crate::market_service::reconnect::Backoff,
) -> Option<Duration> {
    if !replacing || transport_recovery {
        return None;
    }
    let crate::market_service::reconnect::RetryDecision::Wait(delay) =
        recovery.failure(crate::market_service::reconnect::RetryClass::Transient)
    else {
        unreachable!("Rithmic non-transport recovery has no budget");
    };
    Some(delay.max(Duration::from_secs(1)))
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
    let control = match channels.catalog_controls.recv_timeout(ADAPTER_CONTROL_WAIT) {
        Ok(control) => control,
        Err(RecvTimeoutError::Timeout) => return IdleCatalogOutcome::Unchanged,
        Err(RecvTimeoutError::Disconnected) => return IdleCatalogOutcome::Closed,
    };
    let (runtime, events) = match open_catalog_runtime() {
        Ok(opened) => opened,
        Err(error) => {
            diagnostic!("Aeris Rithmic catalog runtime could not open: {error}");
            reject_catalog_control(channels.catalog_publications, &control, None);
            thread::park_timeout(channels.reconnect_delay);
            return IdleCatalogOutcome::Unchanged;
        }
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

fn reject_unavailable_provider(channels: ProviderChannels<'_>) {
    loop {
        while let Ok(control) = channels.catalog_controls.try_recv() {
            reject_catalog_control(channels.catalog_publications, &control, None);
        }
        match channels
            .realtime_controls
            .recv_timeout(ADAPTER_CONTROL_WAIT)
        {
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
    commands: &mut CatalogCommands,
) -> Option<SelectionExit> {
    loop {
        match channels.catalog_controls.try_recv() {
            Ok(control) => dispatch_catalog_control(
                &AdapterSession { runtime, events },
                channels.catalog_publications,
                generation,
                commands,
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
            commands,
        ) {
            publish_live_catalog_selection(
                channels.catalog_publications,
                Some(channels.coordinator_wake),
                selection,
            );
        }
    }
    dispatch_queued_searches(
        &AdapterSession { runtime, events },
        channels.catalog_publications,
        generation,
        commands,
    );
    None
}

/// Session state entering live demand: either the catalog login handed over
/// intact or a fresh login that still has to connect.
struct LiveSessionStart {
    runtime: Runtime,
    events: RithmicProviderEvents,
    retries: RithmicRetryScheduler,
    commands: CatalogCommands,
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
            commands: handoff.commands,
            streaming,
        })
    } else {
        let (runtime, events) = open_catalog_runtime()?;
        Ok(LiveSessionStart {
            runtime,
            events,
            retries: RithmicRetryScheduler::default(),
            commands: CatalogCommands::default(),
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
    // Read before a fresh session loads the vault, so a save racing the
    // connection still retires it.
    let login = handoff
        .as_ref()
        .map_or_else(|| channels.login.current(), |handoff| handoff.login);
    let Ok(start) = live_session_start(handoff) else {
        channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
        return wait_for_replacement(
            channels,
            login,
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
        mut commands,
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
    let end = 'session: loop {
        if channels.coordinator_wake.overflowed("rithmic", generation) {
            let _ = runtime.stop();
            break LiveSessionEnd::Exit(SelectionExit::Replace {
                demand,
                generation,
                transport_recovery: true,
            });
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
                // The restarted connection is a new adapter session that can
                // answer none of the commands sent on the previous one.
                reject_pending_catalog(channels.catalog_publications, generation, &mut commands);
                generation = updated;
                subscription_generation = None;
                subscription_dirty = true;
            }
            Ok(None) => {}
            Err(()) => {
                let _ = runtime.stop();
                break LiveSessionEnd::Exit(SelectionExit::Closed { generation });
            }
        }
        if channels.login_changed(login) {
            // Idle publishes a recoverable disconnect, so the coordinator
            // reissues this demand on a session that reads the new login.
            break LiveSessionEnd::Exit(stop_selection(&mut runtime, generation));
        }
        loop {
            match channels.realtime_controls.try_recv() {
                Ok(RithmicRealtimeControl::Subscribe(replacement)) => {
                    demand = replacement;
                    subscription_dirty = true;
                    if demand.is_empty() {
                        break 'session LiveSessionEnd::Exit(stop_selection(
                            &mut runtime,
                            generation,
                        ));
                    }
                }
                Ok(RithmicRealtimeControl::Stop) => {
                    break 'session LiveSessionEnd::Exit(stop_selection(&mut runtime, generation));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = runtime.stop();
                    break 'session LiveSessionEnd::Exit(SelectionExit::Closed { generation });
                }
            }
        }
        if let Some(exit) =
            handle_live_catalog(&mut runtime, &events, channels, generation, &mut commands)
        {
            break LiveSessionEnd::Exit(exit);
        }
        if drain_live_events(
            &mut runtime,
            &events,
            &mut retries,
            channels,
            generation,
            &mut subscription_generation,
        ) == LiveDrain::SessionEnded
        {
            break LiveSessionEnd::Failed {
                transport_recovery: true,
            };
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
                    break LiveSessionEnd::Failed {
                        transport_recovery: false,
                    };
                }
            }
        }
        let restarted = match retries.retry_due(&mut runtime, Instant::now()) {
            Ok(started) => started.is_some(),
            Err(error) => {
                diagnostic!("Aeris Rithmic reconnect start failed: {error}");
                if retries.ticket().is_none() {
                    channels.publish_realtime(RithmicRealtimeEvent::Failed(generation, error));
                    let _ = runtime.stop();
                    break LiveSessionEnd::Failed {
                        transport_recovery: false,
                    };
                }
                false
            }
        };
        if restarted {
            generation = retire_pending_catalog_generation(
                channels.catalog_publications,
                generation,
                &mut commands,
            );
            subscription_generation = None;
            subscription_dirty = true;
            channels.publish_realtime(RithmicRealtimeEvent::Connecting(generation));
        }
        std::thread::sleep(ADAPTER_CONTROL_WAIT);
    };
    // Every exit has stopped the session, so no callback can still answer
    // its searches or selections.
    reject_pending_catalog(channels.catalog_publications, generation, &mut commands);
    match end {
        LiveSessionEnd::Exit(exit) => exit,
        LiveSessionEnd::Failed { transport_recovery } => wait_for_replacement(
            channels,
            login,
            environment,
            environment_state,
            generation,
            transport_recovery,
        ),
    }
}

/// How one live session loop ended. The session is stopped in both cases.
enum LiveSessionEnd {
    Exit(SelectionExit),
    /// The session failed; wait for the coordinator's replacement demand.
    Failed {
        transport_recovery: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveDrain {
    Open,
    /// The session failed and was stopped; wait for replacement demand.
    SessionEnded,
}

fn drain_live_events(
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    channels: ProviderChannels<'_>,
    generation: u64,
    subscription_generation: &mut Option<SessionGeneration>,
) -> LiveDrain {
    let mut pending = PendingBooks::default();
    for _ in (0..CALLBACK_CAPACITY).take_while(|_| {
        events.has_ready() && !channels.coordinator_wake.overflowed("rithmic", generation)
    }) {
        match try_recv_rithmic_event(runtime, events, retries, Instant::now()) {
            Ok(Some(AppliedRithmicEvent::Semantic(event))) => match event {
                ProviderSessionEvent::InstrumentsDiscovered {
                    generation: ready_generation,
                    ..
                } => {
                    pending.flush(channels, generation);
                    *subscription_generation = Some(ready_generation);
                    channels.publish_realtime(RithmicRealtimeEvent::Connected(generation));
                }
                ProviderSessionEvent::Market {
                    event: MarketEvent::Trade(mut trade),
                    ..
                } => {
                    pending.flush(channels, generation);
                    trade.metadata.session_generation = generation;
                    channels.publish_realtime(RithmicRealtimeEvent::Trade(generation, trade));
                }
                ProviderSessionEvent::Market {
                    event: MarketEvent::Quote(mut quote),
                    ..
                } => {
                    pending.flush(channels, generation);
                    quote.metadata.session_generation = generation;
                    channels.publish_realtime(RithmicRealtimeEvent::Quote(generation, quote));
                }
                ProviderSessionEvent::Market {
                    event: MarketEvent::DepthSnapshot(mut snapshot),
                    ..
                } => {
                    snapshot.metadata.session_generation = generation;
                    pending.queue_depth(channels, generation, snapshot);
                }
                ProviderSessionEvent::Market {
                    event: MarketEvent::OrderLevels(mut event),
                    ..
                } => {
                    let metadata = match &mut event {
                        OrderLevelEvent::Snapshot(snapshot) => &mut snapshot.metadata,
                        OrderLevelEvent::Update(update) => &mut update.metadata,
                        OrderLevelEvent::Reset(reset) => &mut reset.metadata,
                    };
                    metadata.session_generation = generation;
                    pending.queue_order_levels(channels, generation, event);
                }
                ProviderSessionEvent::Heartbeat {
                    transport_rtt_nanos,
                    ..
                } => {
                    pending.flush(channels, generation);
                    publish_heartbeat(channels, generation, transport_rtt_nanos);
                }
                ProviderSessionEvent::DiscoveryStarted
                | ProviderSessionEvent::SystemsDiscovered { .. }
                | ProviderSessionEvent::AuthenticationChanged { .. }
                | ProviderSessionEvent::Market { .. }
                | ProviderSessionEvent::Invalidated { .. }
                | ProviderSessionEvent::Stopped => {
                    pending.flush(channels, generation);
                }
            },
            Ok(Some(AppliedRithmicEvent::RetryScheduled(ticket))) => {
                pending.flush(channels, generation);
                diagnostic!("Aeris Rithmic live session recovering: {:?}", ticket.reason);
                *subscription_generation = None;
                let reason = Some(ticket.reason);
                channels.publish_realtime(RithmicRealtimeEvent::Recovering(generation, reason));
            }
            Ok(Some(AppliedRithmicEvent::TerminalFailure { reason, .. })) => {
                pending.flush(channels, generation);
                diagnostic!("Aeris Rithmic live session failed: {reason:?}");
                channels
                    .publish_realtime(RithmicRealtimeEvent::Disconnected(generation, Some(reason)));
                let _ = runtime.stop();
                return LiveDrain::SessionEnded;
            }
            Err(error) => {
                pending.flush(channels, generation);
                diagnostic!("Aeris Rithmic live callback failed: {error}");
                channels.publish_realtime(RithmicRealtimeEvent::Disconnected(generation, None));
                let _ = runtime.stop();
                return LiveDrain::SessionEnded;
            }
            Ok(None) => {
                pending.flush(channels, generation);
                break;
            }
        }
    }
    pending.flush(channels, generation);
    LiveDrain::Open
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

/// Book publications coalesced within one drain round: a newer aggregate
/// image supersedes the pending one and contiguous order-level updates of one
/// image merge. The two books are independent, so they coalesce independently;
/// every other event flushes both first.
#[derive(Default)]
struct PendingBooks {
    depth: Option<DepthSnapshot>,
    order_levels: Option<OrderLevelUpdate>,
}

impl PendingBooks {
    fn queue_depth(
        &mut self,
        channels: ProviderChannels<'_>,
        generation: u64,
        next: DepthSnapshot,
    ) {
        if let Some(previous) = queue_depth_snapshot(&mut self.depth, next) {
            channels.publish_realtime(RithmicRealtimeEvent::Depth(generation, previous));
        }
    }

    fn queue_order_levels(
        &mut self,
        channels: ProviderChannels<'_>,
        generation: u64,
        event: OrderLevelEvent,
    ) {
        let OrderLevelEvent::Update(update) = event else {
            self.flush_order_levels(channels, generation);
            channels.publish_realtime(RithmicRealtimeEvent::OrderLevels(generation, event));
            return;
        };
        let unmerged = match self.order_levels.as_mut() {
            Some(pending) => pending.merge(update),
            None => Some(update),
        };
        if let Some(next) = unmerged {
            self.flush_order_levels(channels, generation);
            self.order_levels = Some(next);
        }
    }

    fn flush_order_levels(&mut self, channels: ProviderChannels<'_>, generation: u64) {
        if let Some(update) = self.order_levels.take() {
            channels.publish_realtime(RithmicRealtimeEvent::OrderLevels(
                generation,
                OrderLevelEvent::Update(update),
            ));
        }
    }

    fn flush(&mut self, channels: ProviderChannels<'_>, generation: u64) {
        if let Some(snapshot) = self.depth.take() {
            channels.publish_realtime(RithmicRealtimeEvent::Depth(generation, snapshot));
        }
        self.flush_order_levels(channels, generation);
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

/// Waits after a failed session for replacement demand. A login saved since
/// the failed session read the vault ends the wait as idle: that publishes a
/// recoverable disconnect, and the coordinator reissues its demand. No session
/// exists meanwhile, so catalog commands are rejected instead of left waiting.
fn wait_for_replacement(
    channels: ProviderChannels<'_>,
    login: u64,
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
        loop {
            match channels.catalog_controls.try_recv() {
                Ok(control) => {
                    reject_catalog_control(
                        channels.catalog_publications,
                        &control,
                        Some(generation),
                    );
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return SelectionExit::Closed { generation },
            }
        }
        if channels.login_changed(login) {
            return SelectionExit::Idle { generation };
        }
        match channels
            .realtime_controls
            .recv_timeout(ADAPTER_CONTROL_WAIT)
        {
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
            .map_err(|error| format!("Rithmic connection could not start: {error}"))?;
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
        order_levels: demand.order_levels,
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
    let vault = NativeCredentialVault::new(RITHMIC_VAULT_SERVICE)
        .map_err(|_| "Rithmic live credential vault is unavailable".to_string())?;
    let runtime = RithmicProviderRuntime::try_new(
        vault,
        driver,
        RITHMIC_LOGIN_VAULT_KEY,
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
        cell::{Cell, RefCell},
        num::{NonZeroU64, NonZeroUsize},
        sync::mpsc,
        time::Instant,
    };

    use super::{
        CatalogCommands, CatalogDispatchFailure, CatalogIssuer, CatalogSearchRequest,
        CatalogSession, EnvironmentState, MAXIMUM_RITHMIC_RETAINED_SEARCHES,
        MAXIMUM_RITHMIC_SEARCH_RESULTS, RithmicCatalogControl, RithmicCatalogEvent,
        RithmicInstrumentSelection, RithmicProductListing, RithmicProviderCommandError,
        RithmicRealtimeControl, RithmicRealtimeEvent, RithmicSymbolSearch, SearchLane, SentSearch,
        SessionGeneration, bounded_search_results, catalog_selection_subscription,
        dispatch_catalog_control, dispatch_queued_searches, next_generation,
        publish_catalog_callback, queue_depth_snapshot, reject_catalog_generation,
        reject_pending_catalog, reject_undispatched, retire_pending_catalog_generation,
    };
    use crate::{
        InstrumentSearchCategories, ProviderCatalogRejectionReason, ProviderInstrumentKind,
        ProviderInstrumentSearchResult, SearchProviderInstruments, SelectProviderInstrument,
    };
    use aeris_market_data::{
        BookSide, DepthSnapshot, EventMetadata, OrderLevelChange, OrderLevelEvent,
        OrderLevelSnapshot, OrderLevelUpdate, QualifiedTimestamp,
    };
    use aeris_platform_runtime::{NetworkEvent, PowerEvent};
    use aeris_rithmic_protocol_adapter::{
        CollectedProducts, CollectedSymbols, InstrumentType, ProductCodeResult,
        RithmicCatalogEvent as AdapterCatalogEvent, RithmicCatalogRejection,
        RithmicEnvironmentEvent, SearchPattern, SymbolSearchResult,
    };

    #[test]
    fn reconnect_transport_uses_only_adapter_and_nontransport_is_bounded() {
        use crate::market_service::{
            clock::ScriptedClock,
            reconnect::{Backoff, ReconnectPolicy},
        };
        let clock = ScriptedClock::new(42);
        let mut backoff = Backoff::new(ReconnectPolicy::rithmic_recovery(), &clock, 10);
        assert_eq!(super::replacement_delay(false, false, &mut backoff), None);
        assert_eq!(super::replacement_delay(true, true, &mut backoff), None);
        assert_eq!(
            backoff.attempts(),
            0,
            "transport must not stack another retry"
        );
        for _ in 0..20 {
            let delay = super::replacement_delay(true, false, &mut backoff).unwrap();
            assert!(delay >= std::time::Duration::from_secs(1));
            assert!(delay <= std::time::Duration::from_secs(30));
        }
    }

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

    fn order_level_update(previous_revision: u64) -> OrderLevelUpdate {
        OrderLevelUpdate {
            metadata: depth_snapshot("MNQ", 10 + previous_revision).metadata,
            image: 1,
            previous_revision,
            revision: previous_revision + 1,
            provider_sequence: Some(10 + previous_revision),
            changes: vec![OrderLevelChange::Delete {
                order_id: format!("order-{previous_revision}"),
                side: BookSide::Bid,
                price: 100,
            }],
        }
    }

    #[test]
    fn a_drain_round_merges_contiguous_order_level_updates_independently_of_depth() {
        let (_catalog_sender, catalog_controls) = mpsc::sync_channel(1);
        let (catalog_events, _catalog_published) = mpsc::sync_channel(1);
        let catalog_publications = super::CatalogPublisher::new(
            catalog_events,
            "rithmic",
            crate::market_service::ProviderCoordinatorWake::for_tests(),
        );
        let (_realtime_sender, realtime_controls) = mpsc::sync_channel(1);
        let (realtime_publications, published) = mpsc::sync_channel(8);
        let coordinator_wake = crate::market_service::ProviderCoordinatorWake::for_tests();
        let login = super::RithmicLoginRevision::default();
        let channels = super::ProviderChannels {
            catalog_controls: &catalog_controls,
            catalog_publications: &catalog_publications,
            realtime_controls: &realtime_controls,
            realtime_publications: &realtime_publications,
            coordinator_wake: &coordinator_wake,
            reconnect_delay: std::time::Duration::ZERO,
            login: &login,
        };
        let mut pending = super::PendingBooks::default();

        pending.queue_order_levels(channels, 7, OrderLevelEvent::Update(order_level_update(0)));
        pending.queue_depth(channels, 7, depth_snapshot("MNQ", 5));
        pending.queue_order_levels(channels, 7, OrderLevelEvent::Update(order_level_update(1)));
        pending.queue_depth(channels, 7, depth_snapshot("MNQ", 6));
        assert!(
            published.try_recv().is_err(),
            "contiguous updates and a newer aggregate image stay pending"
        );

        pending.queue_order_levels(channels, 7, OrderLevelEvent::Update(order_level_update(3)));
        let Ok(RithmicRealtimeEvent::OrderLevels(7, OrderLevelEvent::Update(merged))) =
            published.try_recv()
        else {
            panic!("a discontinuous update flushes the merged one first");
        };
        assert_eq!(
            (
                merged.previous_revision,
                merged.revision,
                merged.changes.len(),
                merged.provider_sequence
            ),
            (0, 2, 2, Some(11))
        );

        pending.queue_order_levels(
            channels,
            7,
            OrderLevelEvent::Snapshot(OrderLevelSnapshot {
                metadata: depth_snapshot("MNQ", 20).metadata,
                image: 2,
                revision: 0,
                provider_sequence: Some(20),
                orders: Vec::new(),
            }),
        );
        assert!(matches!(
            published.try_recv(),
            Ok(RithmicRealtimeEvent::OrderLevels(7, OrderLevelEvent::Update(update)))
                if (update.previous_revision, update.revision) == (3, 4)
        ));
        assert!(
            matches!(
                published.try_recv(),
                Ok(RithmicRealtimeEvent::OrderLevels(7, OrderLevelEvent::Snapshot(snapshot)))
                    if snapshot.image == 2
            ),
            "a covering image is never held behind an older update"
        );

        pending.flush(channels, 7);
        assert!(matches!(
            published.try_recv(),
            Ok(RithmicRealtimeEvent::Depth(7, snapshot)) if snapshot.metadata.source_sequence == 6
        ));
        assert!(published.try_recv().is_err());
    }
    type Publications = (
        super::CatalogPublisher<RithmicCatalogEvent>,
        mpsc::Receiver<RithmicCatalogEvent>,
    );

    fn publications(capacity: usize) -> Publications {
        let (sender, published) = mpsc::sync_channel(capacity);
        (
            super::CatalogPublisher::new(
                sender,
                "rithmic",
                crate::market_service::ProviderCoordinatorWake::for_tests(),
            ),
            published,
        )
    }

    const fn issuer(consumer_id: u64, generation: u64) -> CatalogIssuer {
        CatalogIssuer {
            consumer_id,
            generation,
        }
    }

    fn wire(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("wire generations are non-zero")
    }

    fn search_control(consumer_id: u64, generation: u64, query: &str) -> SearchProviderInstruments {
        SearchProviderInstruments {
            consumer_id,
            search_generation: generation,
            startup_resolve: false,
            provider: "rithmic".into(),
            query: query.into(),
            maximum_results: 32,
            categories: InstrumentSearchCategories::ALL,
            exchange: None,
        }
    }

    fn selection_control(
        consumer_id: u64,
        selection_generation: u64,
        search_generation: u64,
        symbol: &str,
    ) -> SelectProviderInstrument {
        SelectProviderInstrument {
            consumer_id,
            selection_generation,
            search_generation,
            provider: "rithmic".into(),
            symbol: symbol.into(),
            exchange: "CME".into(),
            entitlement_id: format!("rithmic-test:CME:{symbol}"),
            ..SelectProviderInstrument::default()
        }
    }

    fn session_generation(value: u64) -> SessionGeneration {
        SessionGeneration::new(NonZeroU64::new(value).expect("session generations are non-zero"))
    }

    fn sent_search(issuer: CatalogIssuer, wire: NonZeroUsize) -> SentSearch {
        SentSearch {
            issuer,
            wire,
            session: session_generation(1),
            lane: SearchLane::Symbols,
            exchange: None,
            maximum_results: 32,
            sent: Instant::now(),
        }
    }

    fn issue_search(
        commands: &mut CatalogCommands,
        search: &SearchProviderInstruments,
    ) -> NonZeroUsize {
        let request = commands.search_request(search).expect("search validates");
        let wire = request.generation();
        commands.search_sent(
            wire,
            request.lane(),
            session_generation(1),
            search,
            Instant::now(),
        );
        wire
    }

    /// A command the adapter session received.
    #[derive(Debug, Eq, PartialEq)]
    enum SessionCommand {
        Symbols {
            query: String,
            exchange: Option<String>,
        },
        Listing {
            maximum_results: usize,
        },
        Selection,
    }

    /// An adapter session that records what reaches it.
    struct RecordingSession {
        generation: Cell<Option<SessionGeneration>>,
        received: RefCell<Vec<(NonZeroUsize, SessionCommand)>>,
    }

    impl RecordingSession {
        fn on(generation: u64) -> Self {
            Self {
                generation: Cell::new(Some(session_generation(generation))),
                received: RefCell::new(Vec::new()),
            }
        }

        fn take(&self) -> Vec<(NonZeroUsize, SessionCommand)> {
            self.received.take()
        }
    }

    impl CatalogSession for RecordingSession {
        fn active_generation(&self) -> Option<SessionGeneration> {
            self.generation.get()
        }

        fn search_symbols(
            &self,
            _: SessionGeneration,
            request: RithmicSymbolSearch,
        ) -> Result<(), RithmicProviderCommandError> {
            self.received.borrow_mut().push((
                request.generation(),
                SessionCommand::Symbols {
                    query: request.query().to_string(),
                    exchange: request.exchange().map(str::to_string),
                },
            ));
            Ok(())
        }

        fn list_products(
            &self,
            _: SessionGeneration,
            request: RithmicProductListing,
        ) -> Result<(), RithmicProviderCommandError> {
            self.received.borrow_mut().push((
                request.generation(),
                SessionCommand::Listing {
                    maximum_results: request.maximum_results().get(),
                },
            ));
            Ok(())
        }

        fn select_instrument(
            &self,
            _: SessionGeneration,
            request: RithmicInstrumentSelection,
        ) -> Result<(), RithmicProviderCommandError> {
            self.received
                .borrow_mut()
                .push((request.generation(), SessionCommand::Selection));
            Ok(())
        }
    }

    fn submit(
        session: &RecordingSession,
        publications: &super::CatalogPublisher<RithmicCatalogEvent>,
        commands: &mut CatalogCommands,
        search: SearchProviderInstruments,
    ) {
        dispatch_catalog_control(
            session,
            publications,
            7,
            commands,
            RithmicCatalogControl::Search(search),
            false,
        );
    }

    fn symbols(query: &str) -> SessionCommand {
        SessionCommand::Symbols {
            query: query.to_string(),
            exchange: None,
        }
    }

    fn listed(products: &[(&str, &str)]) -> CollectedProducts {
        CollectedProducts {
            products: products
                .iter()
                .map(|(product_code, exchange)| ProductCodeResult {
                    product_code: (*product_code).to_string(),
                    exchange: (*exchange).to_string(),
                    name: None,
                })
                .collect(),
            duplicate_count: 0,
        }
    }

    fn complete_listing(
        publications: &super::CatalogPublisher<RithmicCatalogEvent>,
        commands: &mut CatalogCommands,
        session_generation: u64,
        wire: NonZeroUsize,
        products: CollectedProducts,
    ) {
        let callback = AdapterCatalogEvent::products_listed(
            NonZeroU64::new(session_generation).expect("non-zero session"),
            wire,
            products,
        );
        assert!(publish_catalog_callback(publications, 7, callback, commands).is_none());
    }

    /// Consumer, search generation and symbols of one published answer.
    fn answer(event: RithmicCatalogEvent) -> (u64, u64, Vec<String>) {
        let result = search_result(event);
        (
            result.consumer_id,
            result.search_generation,
            result
                .instruments
                .into_iter()
                .map(|instrument| format!("{}@{}", instrument.symbol, instrument.exchange))
                .collect(),
        )
    }

    /// Consumer, command generation and reason of one published search rejection.
    fn search_rejection(event: RithmicCatalogEvent) -> (u64, u64, ProviderCatalogRejectionReason) {
        match event {
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: false,
            } => (
                rejection.consumer_id,
                rejection.command_generation,
                rejection.reason,
            ),
            _ => panic!("expected a search rejection"),
        }
    }

    fn found(symbols: &[(&str, &str, &str, Option<&str>)]) -> CollectedSymbols {
        CollectedSymbols {
            results: symbols
                .iter()
                .map(
                    |(symbol, exchange, product_code, expiration_date)| SymbolSearchResult {
                        symbol: (*symbol).to_string(),
                        exchange: (*exchange).to_string(),
                        name: None,
                        product_code: Some((*product_code).to_string()),
                        instrument_type: Some("FUTURE".to_string()),
                        expiration_date: expiration_date.map(str::to_string),
                    },
                )
                .collect(),
            duplicate_count: 0,
        }
    }

    fn complete_search(
        publications: &super::CatalogPublisher<RithmicCatalogEvent>,
        commands: &mut CatalogCommands,
        wire: NonZeroUsize,
        symbols: CollectedSymbols,
    ) {
        let callback = AdapterCatalogEvent::search_completed(NonZeroU64::MIN, wire, symbols);
        assert!(publish_catalog_callback(publications, 7, callback, commands).is_none());
    }

    fn installed(wire: NonZeroUsize, symbol: &str) -> AdapterCatalogEvent {
        AdapterCatalogEvent::selection_installed(
            NonZeroU64::MIN,
            wire,
            format!("{symbol}-CME"),
            symbol.to_string(),
            symbol.to_string(),
            "CME".to_string(),
            2,
            0,
            format!("rithmic-test:CME:{symbol}"),
        )
        .expect("valid selection callback")
    }

    fn search_result(event: RithmicCatalogEvent) -> ProviderInstrumentSearchResult {
        match event {
            RithmicCatalogEvent::SearchCompleted(result) => result,
            _ => panic!("expected a search result"),
        }
    }

    #[test]
    fn resolved_selection_is_deferred_to_the_session_owner() {
        let (publications, published) = publications(1);
        let mut commands = CatalogCommands::default();
        commands.selections.insert(wire(5), issuer(41, 1));

        let deferred =
            publish_catalog_callback(&publications, 7, installed(wire(5), "MNQU6"), &mut commands)
                .expect("selection is deferred to the session owner");

        assert!(published.try_recv().is_err());
        assert!(matches!(
            deferred,
            RithmicCatalogEvent::SelectionResolved {
                consumer_id: 41,
                command_generation: 1,
                instrument
            } if instrument.session_generation == 7
                && instrument.selection_generation == 1
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
        let (publications, published) = publications(1);

        reject_catalog_generation(
            &publications,
            issuer(41, 3),
            Some(7),
            true,
            ProviderCatalogRejectionReason::DispatchUnavailable,
        );

        assert!(matches!(
            published.recv().expect("close failure is published"),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: true,
            } if rejection.consumer_id == 41
                && rejection.provider_generation == Some(7)
                && rejection.command_generation == 3
                && rejection.reason == ProviderCatalogRejectionReason::DispatchUnavailable
        ));
    }

    #[test]
    fn retired_generation_rejects_pending_searches_and_selections() {
        let (publications, published) = publications(4);
        let mut commands = CatalogCommands {
            search: Some(sent_search(issuer(41, 2), wire(2))),
            ..CatalogCommands::default()
        };
        commands.selections.insert(wire(3), issuer(42, 3));
        commands
            .completed_searches
            .push_back((issuer(43, 1), wire(1)));

        reject_pending_catalog(&publications, 9, &mut commands);

        assert!(!commands.has_pending());
        assert!(commands.completed_searches.is_empty());
        assert!(matches!(
            published.recv().expect("search rejection is published"),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: false,
            } if rejection.consumer_id == 41
                && rejection.provider_generation == Some(9)
                && rejection.command_generation == 2
        ));
        assert!(matches!(
            published.recv().expect("selection rejection is published"),
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
        let (publications, published) = publications(2);
        let mut commands = CatalogCommands {
            search: Some(sent_search(issuer(41, 2), wire(2))),
            ..CatalogCommands::default()
        };
        commands.selections.insert(wire(3), issuer(42, 3));
        commands
            .completed_searches
            .push_back((issuer(43, 1), wire(1)));

        let generation = retire_pending_catalog_generation(&publications, 9, &mut commands);

        assert_eq!(generation, 10);
        assert!(!commands.has_pending());
        assert!(commands.completed_searches.is_empty());
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
    fn rejection_lane_comes_from_the_command_that_holds_the_wire_generation() {
        let (publications, published) = publications(2);
        let mut commands = CatalogCommands {
            search: Some(sent_search(issuer(41, 2), wire(2))),
            ..CatalogCommands::default()
        };
        commands.selections.insert(wire(3), issuer(42, 2));
        let rejected = |wire, reason| {
            AdapterCatalogEvent::command_rejected(Some(NonZeroU64::MIN), wire, reason)
        };

        for (command, reason) in [
            (wire(3), RithmicCatalogRejection::SupersededSearch),
            (wire(2), RithmicCatalogRejection::SubscriptionRejected),
            (wire(9), RithmicCatalogRejection::SearchRejected),
        ] {
            assert!(
                publish_catalog_callback(
                    &publications,
                    7,
                    rejected(command, reason),
                    &mut commands
                )
                .is_none()
            );
        }

        assert!(matches!(
            published.recv().expect("selection rejection is published"),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: true,
            } if rejection.consumer_id == 42
                && rejection.command_generation == 2
                && rejection.reason == ProviderCatalogRejectionReason::SupersededSearch
        ));
        assert!(matches!(
            published.recv().expect("search rejection is published"),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: false,
            } if rejection.consumer_id == 41
                && rejection.command_generation == 2
                && rejection.reason == ProviderCatalogRejectionReason::SubscriptionRejected
        ));
        assert!(
            published.try_recv().is_err(),
            "an unknown wire generation answers nobody"
        );
        assert!(!commands.has_pending());
    }

    #[test]
    fn consumers_reusing_a_search_generation_each_receive_their_own_result() {
        let (publications, published) = publications(2);
        let mut commands = CatalogCommands::default();
        let first = issue_search(&mut commands, &search_control(41, 1, "MNQ"));
        complete_search(
            &publications,
            &mut commands,
            first,
            found(&[("MNQZ6", "CME", "MNQ", Some("20261218"))]),
        );
        let second = issue_search(&mut commands, &search_control(42, 1, "ES"));
        assert_ne!(first, second);
        complete_search(
            &publications,
            &mut commands,
            first,
            found(&[("ESZ6", "CME", "ES", Some("20261218"))]),
        );
        complete_search(
            &publications,
            &mut commands,
            second,
            found(&[("ESZ6", "CME", "ES", Some("20261218"))]),
        );

        let mnq = search_result(published.recv().expect("first consumer's result"));
        let es = search_result(published.recv().expect("second consumer's result"));
        assert!(
            published.try_recv().is_err(),
            "an answered wire generation answers nobody again"
        );
        assert_eq!((mnq.consumer_id, mnq.search_generation), (41, 1));
        assert_eq!(mnq.instruments[0].symbol, "MNQZ6");
        assert_eq!((es.consumer_id, es.search_generation), (42, 1));
        assert_eq!(es.instruments[0].symbol, "ESZ6");
        assert!(!commands.has_pending());
    }

    #[test]
    fn selection_after_another_consumers_search_names_its_own_search_wire_generation() {
        let (publications, published) = publications(2);
        let mut commands = CatalogCommands::default();
        let own = issue_search(&mut commands, &search_control(41, 1, "MNQ"));
        complete_search(
            &publications,
            &mut commands,
            own,
            found(&[("MNQZ6", "CME", "MNQ", Some("20261218"))]),
        );
        let other = issue_search(&mut commands, &search_control(0, 1, "ES"));
        complete_search(
            &publications,
            &mut commands,
            other,
            found(&[("ESZ6", "CME", "ES", Some("20261218"))]),
        );

        let selection = selection_control(41, 3, 1, "MNQZ6");
        let request = commands
            .selection_request(&selection, false)
            .expect("the consumer's retained search is still selectable");
        assert_eq!(request.search_generation(), own);
        let selection_wire = request.generation();
        assert!(selection_wire > other);
        commands.selection_sent(selection_wire, &selection);

        let resolved = publish_catalog_callback(
            &publications,
            7,
            installed(selection_wire, "MNQZ6"),
            &mut commands,
        );
        assert!(matches!(
            resolved,
            Some(RithmicCatalogEvent::SelectionResolved {
                consumer_id: 41,
                command_generation: 3,
                instrument,
            }) if instrument.selection_generation == 3
        ));
        assert_eq!(
            published.try_iter().count(),
            2,
            "only the two search results"
        );
    }

    #[test]
    fn selection_naming_an_unretained_search_is_rejected_as_superseded() {
        let (publications, published) = publications(MAXIMUM_RITHMIC_RETAINED_SEARCHES + 2);
        let mut commands = CatalogCommands::default();
        let never_searched = selection_control(41, 2, 1, "MNQZ6");
        let failure = commands
            .selection_request(&never_searched, true)
            .expect_err("no search was retained");
        assert_eq!(failure, CatalogDispatchFailure::UnretainedSearch);

        reject_undispatched(
            &publications,
            7,
            &RithmicCatalogControl::Select(never_searched),
            failure,
        );
        assert!(matches!(
            published.recv().expect("rejection is published"),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: true,
            } if rejection.consumer_id == 41
                && rejection.command_generation == 2
                && rejection.provider_generation == Some(7)
                && rejection.reason == ProviderCatalogRejectionReason::SupersededSearch
        ));

        let evicted = issue_search(&mut commands, &search_control(41, 1, "MNQ"));
        complete_search(&publications, &mut commands, evicted, found(&[]));
        for generation in 1..=MAXIMUM_RITHMIC_RETAINED_SEARCHES {
            let generation = u64::try_from(generation).expect("small generation");
            let wire = issue_search(&mut commands, &search_control(42, generation, "ES"));
            complete_search(&publications, &mut commands, wire, found(&[]));
        }
        assert_eq!(
            commands.selection_request(&selection_control(41, 3, 1, "MNQZ6"), true),
            Err(CatalogDispatchFailure::UnretainedSearch)
        );
        assert!(
            commands
                .selection_request(&selection_control(42, 4, 1, "ESZ6"), true)
                .is_ok(),
            "the oldest search inside the bound stays selectable"
        );
    }

    #[test]
    fn completed_searches_are_forgotten_when_the_session_retires() {
        let (publications, published) = publications(1);
        let mut commands = CatalogCommands::default();
        let searched = issue_search(&mut commands, &search_control(41, 1, "MNQ"));
        complete_search(&publications, &mut commands, searched, found(&[]));
        let _ = published.recv().expect("search result is published");

        reject_pending_catalog(&publications, 7, &mut commands);

        assert!(published.try_recv().is_err(), "nothing was pending");
        assert_eq!(
            commands.selection_request(&selection_control(41, 2, 1, "MNQZ6"), false),
            Err(CatalogDispatchFailure::UnretainedSearch)
        );
        let next = issue_search(&mut commands, &search_control(41, 2, "MNQ"));
        assert!(next > searched, "wire generations never repeat");
    }

    #[test]
    fn search_query_is_upper_cased_for_the_exact_symbol_match() {
        let mut commands = CatalogCommands::default();
        let Ok(CatalogSearchRequest::Symbols(request)) =
            commands.search_request(&search_control(41, 1, "mnq"))
        else {
            panic!("a typed query searches symbols");
        };

        assert_eq!(request.query(), "MNQ");
        assert_eq!(request.pattern(), SearchPattern::Equals);
        assert_eq!(request.instrument_type(), Some(InstrumentType::Future));
    }

    #[test]
    fn empty_query_lists_products_within_the_session_result_bound() {
        let mut commands = CatalogCommands::default();
        for query in ["", "  "] {
            let mut search = search_control(41, 1, query);
            search.maximum_results = 1_000;
            let Ok(CatalogSearchRequest::Products(listing)) = commands.search_request(&search)
            else {
                panic!("an empty query lists products");
            };
            assert_eq!(
                listing.maximum_results().get(),
                MAXIMUM_RITHMIC_SEARCH_RESULTS
            );
        }
    }

    #[test]
    fn product_listing_publishes_unselectable_products_without_evicting_searches() {
        let (publications, published) = publications(1);
        let mut commands = CatalogCommands::default();
        let searched = issue_search(&mut commands, &search_control(41, 1, "MNQ"));
        complete_search(
            &publications,
            &mut commands,
            searched,
            found(&[("MNQZ6", "CME", "MNQ", Some("20261218"))]),
        );
        let _ = published.recv().expect("search result is published");

        for generation in
            2..=u64::try_from(MAXIMUM_RITHMIC_RETAINED_SEARCHES + 1).expect("retention bound fits")
        {
            let listed = issue_search(&mut commands, &search_control(41, generation, ""));
            let products = CollectedProducts {
                products: vec![
                    ProductCodeResult {
                        product_code: "MNQ".to_string(),
                        exchange: "CME".to_string(),
                        name: Some("Micro E-mini Nasdaq-100".to_string()),
                    },
                    ProductCodeResult {
                        product_code: "NQ".to_string(),
                        exchange: "CME".to_string(),
                        name: None,
                    },
                ],
                duplicate_count: 0,
            };
            let callback = AdapterCatalogEvent::products_listed(NonZeroU64::MIN, listed, products);
            assert!(publish_catalog_callback(&publications, 7, callback, &mut commands).is_none());
            let result = search_result(published.recv().expect("listing is published"));
            assert_eq!(result.search_generation, generation);
            let rows = result
                .instruments
                .iter()
                .map(|product| {
                    (
                        product.kind,
                        product.symbol.as_str(),
                        product.product_code.as_deref(),
                        product.name.as_deref(),
                        product.asset_class.as_deref(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                rows,
                [
                    (
                        ProviderInstrumentKind::Product,
                        "MNQ",
                        Some("MNQ"),
                        Some("Micro E-mini Nasdaq-100"),
                        Some("futures"),
                    ),
                    (
                        ProviderInstrumentKind::Product,
                        "NQ",
                        Some("NQ"),
                        None,
                        Some("futures"),
                    ),
                ]
            );
            assert_eq!(
                commands.selection_request(&selection_control(41, 100, generation, "MNQ"), false),
                Err(CatalogDispatchFailure::UnretainedSearch),
                "a product listing is never a selection source"
            );
        }
        assert!(
            commands
                .selection_request(&selection_control(41, 101, 1, "MNQZ6"), false)
                .is_ok(),
            "listings never evict a retained contract search"
        );
        assert!(!commands.has_pending());
    }

    #[test]
    fn exhausted_wire_generation_rejects_the_command() {
        let mut commands = CatalogCommands {
            last_wire: Some(NonZeroUsize::MAX),
            ..CatalogCommands::default()
        };

        let failure = commands
            .search_request(&search_control(41, 1, "MNQ"))
            .expect_err("no wire generation is left");

        assert_eq!(failure, CatalogDispatchFailure::WireGenerationExhausted);
        assert_eq!(
            failure.reason(),
            ProviderCatalogRejectionReason::DispatchUnavailable
        );
    }

    #[test]
    fn search_results_list_each_exchange_front_month_first_and_mark_the_alias() {
        let (publications, published) = publications(1);
        let mut commands = CatalogCommands::default();
        let searched = issue_search(&mut commands, &search_control(41, 1, "MNQ"));

        complete_search(
            &publications,
            &mut commands,
            searched,
            found(&[
                ("ESZ6", "CME", "ES", Some("20261218")),
                ("MNQ", "CME-Delayed", "MNQ", Some("20261218")),
                ("MNQH7", "CME", "MNQ", Some("20270319")),
                ("MNQSPREAD", "CME", "MNQ", None),
                ("MNQZ6", "CME-Delayed", "MNQ", Some("20261218")),
                ("MNQZ6", "CME", "MNQ", Some("20261218")),
            ]),
        );

        let ordered = search_result(published.recv().expect("search result is published"))
            .instruments
            .into_iter()
            .map(|instrument| {
                let alias = instrument.kind == ProviderInstrumentKind::FrontMonthAlias;
                format!(
                    "{}@{}{}",
                    instrument.symbol,
                    instrument.exchange,
                    if alias { " alias" } else { "" }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            [
                "ESZ6@CME",
                "MNQZ6@CME",
                "MNQH7@CME",
                "MNQSPREAD@CME",
                "MNQ@CME-Delayed alias",
                "MNQZ6@CME-Delayed",
            ]
        );
    }

    #[test]
    fn catalog_commands_are_rejected_while_waiting_for_a_replacement_session() {
        let (catalog_sender, catalog_controls) = mpsc::sync_channel(2);
        let (publications, published) = mpsc::sync_channel(2);
        let catalog_publications = super::CatalogPublisher::new(
            publications,
            "rithmic",
            crate::market_service::ProviderCoordinatorWake::for_tests(),
        );
        let (realtime_sender, realtime_controls) = mpsc::sync_channel(1);
        let (realtime_publications, _realtime_events) = mpsc::sync_channel(1);
        let coordinator_wake = crate::market_service::ProviderCoordinatorWake::for_tests();
        let login = super::RithmicLoginRevision::default();
        let channels = super::ProviderChannels {
            catalog_controls: &catalog_controls,
            catalog_publications: &catalog_publications,
            realtime_controls: &realtime_controls,
            realtime_publications: &realtime_publications,
            coordinator_wake: &coordinator_wake,
            reconnect_delay: std::time::Duration::ZERO,
            login: &login,
        };
        let (_environment_sender, environment) = mpsc::sync_channel(1);
        let mut environment_state = EnvironmentState {
            network: NetworkEvent::Available,
            suspended: false,
        };
        catalog_sender
            .send(RithmicCatalogControl::Search(SearchProviderInstruments {
                consumer_id: 41,
                search_generation: 5,
                startup_resolve: false,
                provider: "rithmic".into(),
                query: "MNQ".into(),
                maximum_results: 32,
                categories: InstrumentSearchCategories::ALL,
                exchange: None,
            }))
            .expect("search queued");
        realtime_sender
            .send(RithmicRealtimeControl::Stop)
            .expect("stop queued");

        let exit = super::wait_for_replacement(
            channels,
            login.current(),
            &environment,
            &mut environment_state,
            9,
            false,
        );

        assert!(matches!(exit, super::SelectionExit::Idle { generation: 9 }));
        assert!(matches!(
            published.try_recv().expect("search rejection is published"),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection: false,
            } if rejection.consumer_id == 41
                && rejection.command_generation == 5
                && rejection.provider_generation == Some(9)
        ));
    }

    fn commands_of(sent: &[(NonZeroUsize, SessionCommand)]) -> Vec<&SessionCommand> {
        sent.iter().map(|(_, command)| command).collect()
    }

    #[test]
    fn only_the_latest_waiting_search_follows_the_one_in_flight() {
        let (publications, published) = publications(8);
        let session = RecordingSession::on(1);
        let mut commands = CatalogCommands::default();

        for (generation, query) in [(1, "MNQ"), (2, "MN"), (3, "M2K")] {
            submit(
                &session,
                &publications,
                &mut commands,
                search_control(41, generation, query),
            );
        }

        let first = session.take();
        assert_eq!(commands_of(&first), [&symbols("MNQ")]);
        assert_eq!(
            search_rejection(published.try_recv().expect("replaced search is rejected")),
            (41, 2, ProviderCatalogRejectionReason::SupersededSearch)
        );
        assert!(published.try_recv().is_err());

        complete_search(
            &publications,
            &mut commands,
            first[0].0,
            found(&[("MNQZ6", "CME", "MNQ", Some("20261218"))]),
        );
        dispatch_queued_searches(&session, &publications, 7, &mut commands);
        assert_eq!(
            answer(published.try_recv().expect("first search is answered")),
            (41, 1, vec!["MNQZ6@CME".to_string()])
        );
        let last = session.take();
        assert_eq!(commands_of(&last), [&symbols("M2K")]);

        dispatch_catalog_control(
            &session,
            &publications,
            7,
            &mut commands,
            RithmicCatalogControl::Select(selection_control(41, 9, 1, "MNQZ6")),
            false,
        );
        assert_eq!(
            commands_of(&session.take()),
            [&SessionCommand::Selection],
            "a selection never waits behind a search"
        );

        complete_search(&publications, &mut commands, last[0].0, found(&[]));
        dispatch_queued_searches(&session, &publications, 7, &mut commands);
        assert_eq!(
            answer(published.try_recv().expect("latest search is answered")),
            (41, 3, Vec::new())
        );
        assert_eq!(session.take(), [] as [(NonZeroUsize, SessionCommand); 0]);
        assert!(commands.search.is_none() && commands.queued_searches.is_empty());
    }

    #[test]
    fn waiting_searches_are_sent_in_consumer_order_after_a_rejection() {
        let (publications, published) = publications(8);
        let session = RecordingSession::on(1);
        let mut commands = CatalogCommands::default();
        for (consumer, query) in [(40, "ES"), (41, "MNQ"), (42, "NQ")] {
            submit(
                &session,
                &publications,
                &mut commands,
                search_control(consumer, 1, query),
            );
        }
        let in_flight = session.take();
        assert_eq!(commands_of(&in_flight), [&symbols("ES")]);

        let rejected = AdapterCatalogEvent::command_rejected(
            Some(NonZeroU64::MIN),
            in_flight[0].0,
            RithmicCatalogRejection::SearchRejected,
        );
        assert!(publish_catalog_callback(&publications, 7, rejected, &mut commands).is_none());
        dispatch_queued_searches(&session, &publications, 7, &mut commands);
        assert_eq!(
            search_rejection(published.try_recv().expect("rejection is published")),
            (40, 1, ProviderCatalogRejectionReason::SearchRejected)
        );
        let second = session.take();
        assert_eq!(commands_of(&second), [&symbols("MNQ")]);

        complete_search(&publications, &mut commands, second[0].0, found(&[]));
        dispatch_queued_searches(&session, &publications, 7, &mut commands);
        assert_eq!(answer(published.try_recv().expect("answer")).0, 41);
        assert_eq!(commands_of(&session.take()), [&symbols("NQ")]);
    }

    #[test]
    fn listing_is_cached_unfiltered_and_answers_empty_queries_without_the_session() {
        let (publications, published) = publications(8);
        let session = RecordingSession::on(1);
        let mut commands = CatalogCommands::default();
        let mut truncated = search_control(41, 1, "");
        truncated.maximum_results = 1;
        submit(&session, &publications, &mut commands, truncated);
        let listing = session.take();
        assert_eq!(
            commands_of(&listing),
            [&SessionCommand::Listing {
                maximum_results: MAXIMUM_RITHMIC_SEARCH_RESULTS
            }],
            "the listing is requested at the session limit, not the consumer bound"
        );
        complete_listing(
            &publications,
            &mut commands,
            1,
            listing[0].0,
            listed(&[("MNQ", "CME"), ("NQ", "CME"), ("CL", "NYMEX")]),
        );
        assert_eq!(
            answer(published.try_recv().expect("listing answer")),
            (41, 1, vec!["MNQ@CME".to_string()])
        );

        let mut nymex = search_control(42, 1, "  ");
        nymex.exchange = Some("NYMEX".into());
        submit(&session, &publications, &mut commands, nymex);
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(43, 1, ""),
        );
        assert!(session.take().is_empty(), "cached listings send no command");
        assert_eq!(
            answer(published.try_recv().expect("filtered answer")),
            (42, 1, vec!["CL@NYMEX".to_string()])
        );
        assert_eq!(
            answer(published.try_recv().expect("unfiltered answer")).2,
            ["MNQ@CME", "NQ@CME", "CL@NYMEX"]
        );

        submit(
            &session,
            &publications,
            &mut commands,
            search_control(44, 1, "ES"),
        );
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(45, 1, "NQ"),
        );
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(45, 2, ""),
        );
        assert_eq!(commands_of(&session.take()), [&symbols("ES")]);
        assert_eq!(
            search_rejection(published.try_recv().expect("waiting search is replaced")),
            (45, 1, ProviderCatalogRejectionReason::SupersededSearch)
        );
        assert_eq!(
            answer(
                published
                    .try_recv()
                    .expect("cached answer while a search is in flight")
            )
            .1,
            2
        );
        assert!(commands.queued_searches.is_empty());
    }

    #[test]
    fn cached_listing_is_not_reused_by_another_session_generation() {
        let (publications, published) = publications(8);
        let session = RecordingSession::on(1);
        let mut commands = CatalogCommands::default();
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(41, 1, ""),
        );
        let listing = session.take();
        complete_listing(
            &publications,
            &mut commands,
            1,
            listing[0].0,
            listed(&[("MNQ", "CME")]),
        );
        let _ = published.try_recv().expect("listing answer");

        session.generation.set(Some(session_generation(2)));
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(41, 2, ""),
        );
        assert_eq!(
            commands_of(&session.take()),
            [&SessionCommand::Listing {
                maximum_results: MAXIMUM_RITHMIC_SEARCH_RESULTS
            }]
        );
        assert!(published.try_recv().is_err());

        reject_pending_catalog(&publications, 7, &mut commands);
        assert!(
            commands.listing.is_none(),
            "a retired session keeps no listing"
        );
    }

    #[test]
    fn search_in_flight_on_a_replaced_session_is_rejected_and_the_queue_continues() {
        let (publications, published) = publications(8);
        let session = RecordingSession::on(1);
        let mut commands = CatalogCommands::default();
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(41, 1, "MNQ"),
        );
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(42, 1, "ES"),
        );
        assert_eq!(commands_of(&session.take()), [&symbols("MNQ")]);

        session.generation.set(Some(session_generation(2)));
        dispatch_queued_searches(&session, &publications, 7, &mut commands);

        assert_eq!(
            search_rejection(published.try_recv().expect("lost search is rejected")),
            (41, 1, ProviderCatalogRejectionReason::DispatchUnavailable)
        );
        assert_eq!(commands_of(&session.take()), [&symbols("ES")]);
        assert_eq!(
            commands.search.as_ref().map(|search| search.session),
            Some(session_generation(2))
        );
    }

    #[test]
    fn waiting_searches_are_rejected_when_the_session_retires() {
        let (publications, published) = publications(8);
        let session = RecordingSession::on(1);
        let mut commands = CatalogCommands::default();
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(41, 1, "MNQ"),
        );
        submit(
            &session,
            &publications,
            &mut commands,
            search_control(42, 1, "ES"),
        );

        reject_pending_catalog(&publications, 7, &mut commands);

        let rejected = published
            .try_iter()
            .map(search_rejection)
            .collect::<Vec<_>>();
        assert_eq!(
            rejected,
            [
                (41, 1, ProviderCatalogRejectionReason::DispatchUnavailable),
                (42, 1, ProviderCatalogRejectionReason::DispatchUnavailable),
            ]
        );
        assert!(!commands.has_pending());
    }

    #[test]
    fn symbol_search_carries_the_exchange_filter_to_the_session() {
        let session = RecordingSession::on(1);
        let (publications, _published) = publications(1);
        let mut commands = CatalogCommands::default();
        let mut search = search_control(41, 1, "mnq");
        search.exchange = Some("CME".into());
        submit(&session, &publications, &mut commands, search);

        assert_eq!(
            commands_of(&session.take()),
            [&SessionCommand::Symbols {
                query: "MNQ".into(),
                exchange: Some("CME".into()),
            }]
        );
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
