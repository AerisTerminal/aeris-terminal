use crate::network::ConnectionAbort;
use crate::{
    AggregateBookAssembler, AggregateBookLimits, AggregateBookOutcome, CollectedSymbols,
    CollectionProgress, CollectorError, DecodedCatalogMessage, DecodedControlMessage,
    DecodedMarketMessage, InstrumentReference, InstrumentReferenceRequest, InstrumentType,
    MarketDataSubscription, MarketIdentity, OrderBookLevel, ProviderTimestamp, QuoteLevel,
    QuoteSideUpdate, RetryDisposition, RithmicApplication, RithmicCredentialBytes,
    RithmicSessionError, RithmicSessionLimits, RithmicSessionMessage, RithmicTestSession,
    SearchPattern, SubscriptionAction, SymbolSearchCollectionRequest, SymbolSearchCollector,
    SymbolSearchRequest, SymbolSearchResult, TradeAggressor,
};
use axiusflow_desktop_provider_runtime::{
    AuthenticationState, ConnectTrigger, DesktopMarketWorker, DesktopMarketWorkerError,
    DesktopProviderState, InstrumentDescriptor, NetworkEvent, ProviderEnvironment,
    ProviderInvalidationReason, ProviderSessionDriver, ProviderSessionEvent, RecoveryReason,
    SessionGeneration,
};
use axiusflow_market_data::{
    AggressorSide, DepthLevel, DepthSnapshot, EventMetadata, MarketEvent, MarketTrade,
    QualifiedTimestamp, TopOfBookQuote,
};
use axiusflow_platform_runtime::{CredentialVault, PowerEvent};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    mem::size_of,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROVIDER_ID: &str = "rithmic";
const SYSTEM_ID: &str = "RITHMIC_TEST";
const ENVIRONMENT: &str = "Test";
/// Native credential-vault service for the `AxiusFlow` terminal.
pub const RITHMIC_TEST_VAULT_SERVICE: &str = "com.axiusflow.terminal";
/// Non-secret vault key for the default Rithmic Test account.
pub const RITHMIC_TEST_VAULT_KEY: &str = "provider-rithmic-test-default-v1";
const MAXIMUM_INSTRUMENTS: usize = 128;
const MAXIMUM_IDENTITY_BYTES: usize = 256;
const SESSION_COMMAND_CAPACITY: usize = 8;
const SESSION_COMMAND_BATCH: usize = 4;
const SESSION_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAXIMUM_CALLBACK_EVENTS: usize = 4_096;
const MAXIMUM_CALLBACK_BYTES: usize = 256 * 1024 * 1024;
const MAXIMUM_SILENCE_TIMEOUT: Duration = Duration::from_mins(5);
const RETRY_DELAYS: [Duration; 7] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(8),
];

/// Explicit provider-to-canonical registry entry and data selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicProviderInstrument {
    pub descriptor: InstrumentDescriptor,
    pub entitlement_id: String,
    pub trades: bool,
    pub quotes: bool,
    pub order_book: bool,
}

/// Bounded, owned symbol-search intent for one UI search generation.
#[derive(Clone, Eq, PartialEq)]
pub struct RithmicSymbolSearch {
    search_generation: NonZeroUsize,
    search_text: String,
    exchange: Option<String>,
    product_code: Option<String>,
    instrument_type: Option<InstrumentType>,
    pattern: SearchPattern,
    maximum_results: NonZeroUsize,
}

impl fmt::Debug for RithmicSymbolSearch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicSymbolSearch")
            .field("search_generation", &self.search_generation)
            .field("maximum_results", &self.maximum_results)
            .field("query", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl RithmicSymbolSearch {
    /// Creates one bounded search without retaining provider text in diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, or control-bearing field, or
    /// for a result bound above the direct-session instrument limit.
    pub fn try_new(
        search_generation: NonZeroUsize,
        search_text: impl Into<String>,
        exchange: Option<String>,
        product_code: Option<String>,
        instrument_type: Option<InstrumentType>,
        pattern: SearchPattern,
        maximum_results: NonZeroUsize,
    ) -> Result<Self, RithmicProviderCommandError> {
        let search_text = search_text.into();
        if maximum_results.get() > MAXIMUM_INSTRUMENTS
            || !valid_command_field(&search_text)
            || exchange
                .as_ref()
                .is_some_and(|value| !valid_command_field(value))
            || product_code
                .as_ref()
                .is_some_and(|value| !valid_command_field(value))
        {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
        Ok(Self {
            search_generation,
            search_text,
            exchange,
            product_code,
            instrument_type,
            pattern,
            maximum_results,
        })
    }

    fn protocol_request(&self) -> SymbolSearchRequest<'_> {
        SymbolSearchRequest {
            search_text: &self.search_text,
            exchange: self.exchange.as_deref(),
            product_code: self.product_code.as_deref(),
            instrument_type: self.instrument_type,
            pattern: self.pattern,
        }
    }

    fn collection_request(&self) -> SymbolSearchCollectionRequest {
        SymbolSearchCollectionRequest {
            exchange: self.exchange.clone(),
            product_code: self.product_code.clone(),
            instrument_type: self
                .instrument_type
                .map(instrument_type_name)
                .map(str::to_string),
            maximum_results: self.maximum_results,
        }
    }
}

/// Exact selection from the latest completed search.
#[derive(Clone, Eq, PartialEq)]
pub struct RithmicInstrumentSelection {
    selection_generation: NonZeroUsize,
    search_generation: NonZeroUsize,
    symbol: String,
    exchange: String,
    entitlement_id: String,
    trades: bool,
    quotes: bool,
    order_book: bool,
}

/// Read-only market-data families enabled for one selected instrument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicReadOnlySubscription {
    trades: bool,
    quotes: bool,
    order_book: bool,
}

impl RithmicReadOnlySubscription {
    /// Creates a non-empty read-only market-data selection.
    ///
    /// # Errors
    ///
    /// Returns an error when every market-data family is disabled.
    pub fn try_new(
        trades: bool,
        quotes: bool,
        order_book: bool,
    ) -> Result<Self, RithmicProviderCommandError> {
        if !(trades || quotes || order_book) {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
        Ok(Self {
            trades,
            quotes,
            order_book,
        })
    }

    const fn is_empty(self) -> bool {
        !(self.trades || self.quotes || self.order_book)
    }
}

impl fmt::Debug for RithmicInstrumentSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicInstrumentSelection")
            .field("selection_generation", &self.selection_generation)
            .field("search_generation", &self.search_generation)
            .field("instrument", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl RithmicInstrumentSelection {
    /// Creates one bounded read-only selection.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities or an empty market-data intent.
    pub fn try_new(
        selection_generation: NonZeroUsize,
        search_generation: NonZeroUsize,
        symbol: impl Into<String>,
        exchange: impl Into<String>,
        entitlement_id: impl Into<String>,
        subscription: RithmicReadOnlySubscription,
    ) -> Result<Self, RithmicProviderCommandError> {
        let symbol = symbol.into();
        let exchange = exchange.into();
        let entitlement_id = entitlement_id.into();
        if !valid_command_field(&symbol)
            || !valid_command_field(&exchange)
            || !valid_command_field(&entitlement_id)
            || subscription.is_empty()
        {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
        Ok(Self {
            selection_generation,
            search_generation,
            symbol,
            exchange,
            entitlement_id,
            trades: subscription.trades,
            quotes: subscription.quotes,
            order_book: subscription.order_book,
        })
    }
}

/// Coarse asynchronous rejection for a bounded catalog command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicCatalogRejection {
    SearchRejected,
    SupersededSearch,
    InstrumentUnavailable,
    SubscriptionRejected,
}

/// Adapter-local catalog output, fenced by session and UI generations.
#[derive(Clone, Eq, PartialEq)]
pub enum RithmicCatalogEvent {
    SearchCompleted {
        session_generation: SessionGeneration,
        search_generation: NonZeroUsize,
        symbols: CollectedSymbols,
    },
    SelectionInstalled {
        session_generation: SessionGeneration,
        selection_generation: NonZeroUsize,
        instrument: InstrumentDescriptor,
    },
    CommandRejected {
        session_generation: SessionGeneration,
        command_generation: NonZeroUsize,
        reason: RithmicCatalogRejection,
    },
}

impl fmt::Debug for RithmicCatalogEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SearchCompleted {
                session_generation,
                search_generation,
                symbols,
            } => formatter
                .debug_struct("SearchCompleted")
                .field("session_generation", session_generation)
                .field("search_generation", search_generation)
                .field("result_count", &symbols.results.len())
                .finish(),
            Self::SelectionInstalled {
                session_generation,
                selection_generation,
                ..
            } => formatter
                .debug_struct("SelectionInstalled")
                .field("session_generation", session_generation)
                .field("selection_generation", selection_generation)
                .field("instrument", &"[REDACTED]")
                .finish(),
            Self::CommandRejected {
                session_generation,
                command_generation,
                reason,
            } => formatter
                .debug_struct("CommandRejected")
                .field("session_generation", session_generation)
                .field("command_generation", command_generation)
                .field("reason", reason)
                .finish(),
        }
    }
}

/// Redacted command-validation and delivery failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicProviderCommandError {
    InvalidRequest,
    SessionUnavailable,
    StaleGeneration,
    QueueFull,
}

impl fmt::Display for RithmicProviderCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic catalog command failed: {self:?}")
    }
}

impl Error for RithmicProviderCommandError {}

fn valid_command_field(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAXIMUM_IDENTITY_BYTES
        && !value.chars().any(char::is_control)
}

const fn instrument_type_name(instrument_type: InstrumentType) -> &'static str {
    match instrument_type {
        InstrumentType::Future => "FUTURE",
        InstrumentType::FutureOption => "FUTURE_OPTION",
        InstrumentType::FutureStrategy => "FUTURE_STRATEGY",
        InstrumentType::Equity => "EQUITY",
        InstrumentType::EquityOption => "EQUITY_OPTION",
        InstrumentType::EquityStrategy => "EQUITY_STRATEGY",
        InstrumentType::Index => "INDEX",
        InstrumentType::IndexOption => "INDEX_OPTION",
        InstrumentType::Spread => "SPREAD",
        InstrumentType::Synthetic => "SYNTHETIC",
    }
}

impl RithmicProviderInstrument {
    fn as_request(&self) -> MarketDataSubscription<'_> {
        MarketDataSubscription {
            symbol: &self.descriptor.provider_symbol,
            exchange: &self.descriptor.venue_id,
            action: SubscriptionAction::Subscribe,
            trades: self.trades,
            quotes: self.quotes,
            order_book: self.order_book,
        }
    }

    fn identity_matches(&self, identity: &MarketIdentity) -> bool {
        self.descriptor.provider_symbol == identity.symbol
            && self.descriptor.venue_id == identity.exchange
    }
}

/// Invalid direct Rithmic driver configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicProviderConfigError;

impl fmt::Display for RithmicProviderConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid direct Rithmic provider configuration")
    }
}

impl Error for RithmicProviderConfigError {}

/// Fixed application identity, session limits, and registered instruments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicProviderConfig {
    application_name: String,
    application_version: String,
    session_limits: RithmicSessionLimits,
    message_silence_timeout: Duration,
    instruments: Vec<RithmicProviderInstrument>,
    authorized_silence_evidence_faults:
        Vec<(SessionGeneration, RithmicAuthorizedSilenceEvidenceFault)>,
}

/// Client-local inbound suppression used only by the authorized headless resilience smoke.
///
/// This never changes provider behavior and is not evidence of a provider-observed loss.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicAuthorizedSilenceEvidenceFault {
    Heartbeat,
    Message,
}

impl RithmicProviderConfig {
    /// Creates a bounded direct-session configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid application identity, session bounds,
    /// canonical registry metadata, or duplicate identities.
    pub fn try_new(
        application_name: impl Into<String>,
        application_version: impl Into<String>,
        session_limits: RithmicSessionLimits,
        message_silence_timeout: Duration,
        instruments: Vec<RithmicProviderInstrument>,
    ) -> Result<Self, RithmicProviderConfigError> {
        let application_name = application_name.into();
        let application_version = application_version.into();
        if !valid_identity(&application_name)
            || !valid_identity(&application_version)
            || session_limits.validate().is_err()
            || message_silence_timeout < session_limits.response_timeout
            || message_silence_timeout > MAXIMUM_SILENCE_TIMEOUT
            || instruments.len() > MAXIMUM_INSTRUMENTS
        {
            return Err(RithmicProviderConfigError);
        }
        let mut provider_identities = BTreeSet::new();
        let mut canonical_identities = BTreeSet::new();
        for instrument in &instruments {
            if instrument.descriptor.validate().is_err()
                || !valid_identity(&instrument.entitlement_id)
                || !(instrument.trades || instrument.quotes || instrument.order_book)
                || !provider_identities.insert((
                    &instrument.descriptor.venue_id,
                    &instrument.descriptor.provider_symbol,
                ))
                || !canonical_identities.insert(&instrument.descriptor.instrument_id)
            {
                return Err(RithmicProviderConfigError);
            }
        }
        Ok(Self {
            application_name,
            application_version,
            session_limits,
            message_silence_timeout,
            instruments,
            authorized_silence_evidence_faults: Vec::new(),
        })
    }

    /// Suppresses inbound processing after authentication so the
    /// production silence detector can be exercised without disrupting the
    /// provider or exposing credentials outside the native-vault process.
    #[must_use]
    #[doc(hidden)]
    pub fn with_authorized_silence_evidence_fault(
        mut self,
        generation: SessionGeneration,
        fault: RithmicAuthorizedSilenceEvidenceFault,
    ) -> Self {
        self.authorized_silence_evidence_faults
            .retain(|(configured, _)| *configured != generation);
        if self.authorized_silence_evidence_faults.len() < 2 {
            self.authorized_silence_evidence_faults
                .push((generation, fault));
        }
        self
    }

    fn silence_evidence_fault(
        &self,
        generation: SessionGeneration,
    ) -> Option<RithmicAuthorizedSilenceEvidenceFault> {
        self.authorized_silence_evidence_faults
            .iter()
            .find_map(|(configured, fault)| (*configured == generation).then_some(*fault))
    }

    /// Returns the fixed non-secret Rithmic Test provider profile.
    #[must_use]
    pub fn environment() -> ProviderEnvironment {
        ProviderEnvironment {
            provider_id: PROVIDER_ID.to_string(),
            system_id: SYSTEM_ID.to_string(),
            environment: ENVIRONMENT.to_string(),
        }
    }
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAXIMUM_IDENTITY_BYTES
        && !value.chars().any(char::is_control)
}

/// Item, retained-byte, and canonical depth bounds for adapter callbacks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicCallbackLimits {
    event_capacity: NonZeroUsize,
    byte_capacity: NonZeroUsize,
    maximum_depth: NonZeroUsize,
}

impl RithmicCallbackLimits {
    /// Creates validated callback bounds.
    ///
    /// # Errors
    ///
    /// Returns an error above hard protocol/runtime bounds.
    pub fn try_new(
        maximum_events: NonZeroUsize,
        maximum_bytes: NonZeroUsize,
        maximum_depth_levels: NonZeroUsize,
    ) -> Result<Self, RithmicProviderConfigError> {
        if maximum_events.get() > MAXIMUM_CALLBACK_EVENTS
            || maximum_bytes.get() > MAXIMUM_CALLBACK_BYTES
            || maximum_depth_levels.get() > 4_096
        {
            return Err(RithmicProviderConfigError);
        }
        Ok(Self {
            event_capacity: maximum_events,
            byte_capacity: maximum_bytes,
            maximum_depth: maximum_depth_levels,
        })
    }
}

/// Provider-neutral semantic callback plus adapter retry classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicProviderCallback {
    pub generation: SessionGeneration,
    pub event: ProviderSessionEvent,
    pub retry: Option<RetryDisposition>,
}

/// Receiving half of the bounded direct Rithmic callback channel.
pub struct RithmicProviderEvents {
    callbacks: Arc<SharedCallbacks>,
}

/// One bounded adapter-local catalog callback.
#[derive(Clone, Eq, PartialEq)]
pub struct RithmicCatalogCallback {
    pub generation: SessionGeneration,
    pub event: RithmicCatalogEvent,
}

impl fmt::Debug for RithmicCatalogCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicCatalogCallback")
            .field("generation", &self.generation)
            .field("event", &self.event)
            .finish()
    }
}

impl RithmicProviderEvents {
    #[must_use]
    pub fn has_ready(&self) -> bool {
        let state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.queue.is_empty() || state.terminal.is_some() {
            return true;
        }
        drop(state);
        !self
            .callbacks
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .queue
            .is_empty()
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<RithmicProviderCallback> {
        let mut state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(queued) = state.queue.pop_front() {
            state.queued_bytes = state.queued_bytes.saturating_sub(queued.retained_bytes);
            return Some(RithmicProviderCallback {
                generation: queued.generation,
                event: queued.event,
                retry: queued.retry,
            });
        }
        state
            .terminal
            .take()
            .map(|terminal| RithmicProviderCallback {
                generation: terminal.generation,
                event: terminal.event,
                retry: terminal.retry,
            })
    }

    /// Receives at most one bounded catalog callback.
    #[must_use]
    pub fn try_recv_catalog(&self) -> Option<RithmicCatalogCallback> {
        let mut catalog = self
            .callbacks
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let queued = catalog.queue.pop_front()?;
        catalog.queued_bytes = catalog.queued_bytes.saturating_sub(queued.retained_bytes);
        Some(RithmicCatalogCallback {
            generation: queued.generation,
            event: queued.event,
        })
    }

    /// Enqueues one bounded symbol search for the exact active session generation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a missing/stale session or a full command queue.
    pub fn search_symbols(
        &self,
        generation: SessionGeneration,
        request: RithmicSymbolSearch,
    ) -> Result<(), RithmicProviderCommandError> {
        self.send_command(generation, RithmicSessionCommand::Search(request))
    }

    /// Enqueues one read-only selection from the latest completed symbol search.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a missing/stale session or a full command queue.
    pub fn select_instrument(
        &self,
        generation: SessionGeneration,
        request: RithmicInstrumentSelection,
    ) -> Result<(), RithmicProviderCommandError> {
        self.send_command(generation, RithmicSessionCommand::Select(request))
    }

    fn send_command(
        &self,
        generation: SessionGeneration,
        command: RithmicSessionCommand,
    ) -> Result<(), RithmicProviderCommandError> {
        let commands = self
            .callbacks
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (active_generation, sender) = commands
            .as_ref()
            .ok_or(RithmicProviderCommandError::SessionUnavailable)?;
        if *active_generation != generation {
            return Err(RithmicProviderCommandError::StaleGeneration);
        }
        sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => RithmicProviderCommandError::QueueFull,
            TrySendError::Disconnected(_) => RithmicProviderCommandError::SessionUnavailable,
        })
    }
}

/// Redacted direct-driver lifecycle failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicProviderDriverError {
    SessionAlreadyActive,
    InvalidCredentials,
    StaleGeneration,
    ThreadUnavailable,
    StopUnconfirmed,
}

impl fmt::Display for RithmicProviderDriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic desktop driver failed: {self:?}")
    }
}

impl Error for RithmicProviderDriverError {}

type SessionTask = dyn Fn(
        RithmicProviderConfig,
        SessionGeneration,
        RithmicCredentialBytes,
        Arc<AtomicBool>,
        Arc<ConnectionAbort>,
        Receiver<RithmicSessionCommand>,
        SessionEmitter,
    ) + Send
    + Sync
    + 'static;

struct ActiveSession {
    generation: SessionGeneration,
    stop: Arc<AtomicBool>,
    abort: Arc<ConnectionAbort>,
    handle: JoinHandle<()>,
}

enum RithmicSessionCommand {
    Search(RithmicSymbolSearch),
    Select(RithmicInstrumentSelection),
}

impl fmt::Debug for RithmicSessionCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Search(request) => formatter.debug_tuple("Search").field(request).finish(),
            Self::Select(request) => formatter.debug_tuple("Select").field(request).finish(),
        }
    }
}

struct QueuedCallback {
    generation: SessionGeneration,
    event: ProviderSessionEvent,
    retry: Option<RetryDisposition>,
    retained_bytes: usize,
}

struct CallbackState {
    queue: VecDeque<QueuedCallback>,
    queued_bytes: usize,
    terminal: Option<QueuedCallback>,
    failed: bool,
}

struct QueuedCatalogCallback {
    generation: SessionGeneration,
    event: RithmicCatalogEvent,
    retained_bytes: usize,
}

struct CatalogCallbackState {
    queue: VecDeque<QueuedCatalogCallback>,
    queued_bytes: usize,
}

struct SharedCallbacks {
    limits: RithmicCallbackLimits,
    maximum_instruments: NonZeroUsize,
    state: Mutex<CallbackState>,
    catalog: Mutex<CatalogCallbackState>,
    commands: Mutex<Option<(SessionGeneration, SyncSender<RithmicSessionCommand>)>>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone)]
struct SessionEmitter {
    generation: SessionGeneration,
    callbacks: Arc<SharedCallbacks>,
}

impl SessionEmitter {
    fn send(&self, event: ProviderSessionEvent) -> bool {
        if event_generation(&event).is_some_and(|generation| generation != self.generation)
            || event
                .validate(
                    NonZeroUsize::MIN,
                    self.callbacks.maximum_instruments,
                    self.callbacks.limits.maximum_depth.get(),
                )
                .is_err()
        {
            self.invalid(
                ProviderInvalidationReason::MalformedMessage,
                RetryDisposition::Terminal,
            );
            return false;
        }
        let Some(retained_bytes) = retained_event_bytes(&event) else {
            self.invalid(
                ProviderInvalidationReason::QueueOverflow,
                RetryDisposition::Transient,
            );
            return false;
        };
        let mut state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_bytes = state.queued_bytes.checked_add(retained_bytes);
        let (accepted, notify) = if state.failed {
            (false, false)
        } else if state.queue.len() >= self.callbacks.limits.event_capacity.get()
            || next_bytes.is_none_or(|bytes| bytes > self.callbacks.limits.byte_capacity.get())
        {
            latch_terminal(
                &mut state,
                self.generation,
                ProviderInvalidationReason::QueueOverflow,
                RetryDisposition::Transient,
            );
            (false, true)
        } else {
            state.queued_bytes = next_bytes.unwrap_or(state.queued_bytes);
            state.queue.push_back(QueuedCallback {
                generation: self.generation,
                event,
                retry: None,
                retained_bytes,
            });
            (true, true)
        };
        drop(state);
        if notify {
            self.wake();
        }
        accepted
    }

    fn invalid(&self, reason: ProviderInvalidationReason, retry: RetryDisposition) {
        let mut state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let notify = if state.failed {
            false
        } else {
            latch_terminal(&mut state, self.generation, reason, retry);
            true
        };
        drop(state);
        if notify {
            self.wake();
        }
    }

    fn send_catalog(&self, event: RithmicCatalogEvent) -> bool {
        if catalog_event_generation(&event) != self.generation {
            self.invalid(
                ProviderInvalidationReason::MalformedMessage,
                RetryDisposition::Terminal,
            );
            return false;
        }
        let Some(retained_bytes) = retained_catalog_event_bytes(&event) else {
            self.invalid(
                ProviderInvalidationReason::QueueOverflow,
                RetryDisposition::Transient,
            );
            return false;
        };
        let mut catalog = self
            .callbacks
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_bytes = catalog.queued_bytes.checked_add(retained_bytes);
        let accepted = catalog.queue.len() < self.callbacks.limits.event_capacity.get()
            && next_bytes.is_some_and(|bytes| bytes <= self.callbacks.limits.byte_capacity.get());
        if accepted {
            catalog.queued_bytes = next_bytes.unwrap_or(catalog.queued_bytes);
            catalog.queue.push_back(QueuedCatalogCallback {
                generation: self.generation,
                event,
                retained_bytes,
            });
        }
        drop(catalog);
        if accepted {
            self.wake();
        } else {
            self.invalid(
                ProviderInvalidationReason::QueueOverflow,
                RetryDisposition::Transient,
            );
        }
        accepted
    }

    fn wake(&self) {
        if let Some(wake) = &self.callbacks.wake {
            wake();
        }
    }
}

fn latch_terminal(
    state: &mut CallbackState,
    generation: SessionGeneration,
    reason: ProviderInvalidationReason,
    retry: RetryDisposition,
) {
    state.failed = true;
    state.terminal = Some(QueuedCallback {
        generation,
        event: ProviderSessionEvent::Invalidated {
            generation: Some(generation),
            reason,
        },
        retry: Some(retry),
        retained_bytes: 0,
    });
}

/// Cancellable one-generation-at-a-time Rithmic session driver.
pub struct RithmicProviderDriver {
    config: RithmicProviderConfig,
    callbacks: Arc<SharedCallbacks>,
    task: Arc<SessionTask>,
    active: Option<ActiveSession>,
}

impl RithmicProviderDriver {
    #[must_use]
    pub fn new(
        config: RithmicProviderConfig,
        callback_limits: RithmicCallbackLimits,
    ) -> (Self, RithmicProviderEvents) {
        Self::with_task_and_wake(config, callback_limits, direct_session_task(), None)
    }

    #[must_use]
    pub fn new_with_wake(
        config: RithmicProviderConfig,
        callback_limits: RithmicCallbackLimits,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> (Self, RithmicProviderEvents) {
        Self::with_task_and_wake(config, callback_limits, direct_session_task(), Some(wake))
    }

    #[cfg(test)]
    fn with_task(
        config: RithmicProviderConfig,
        callback_limits: RithmicCallbackLimits,
        task: Arc<SessionTask>,
    ) -> (Self, RithmicProviderEvents) {
        Self::with_task_and_wake(config, callback_limits, task, None)
    }

    fn with_task_and_wake(
        config: RithmicProviderConfig,
        callback_limits: RithmicCallbackLimits,
        task: Arc<SessionTask>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> (Self, RithmicProviderEvents) {
        let maximum_instruments =
            NonZeroUsize::new(config.instruments.len()).unwrap_or(NonZeroUsize::MIN);
        let callbacks = Arc::new(SharedCallbacks {
            limits: callback_limits,
            maximum_instruments,
            state: Mutex::new(CallbackState {
                queue: VecDeque::with_capacity(callback_limits.event_capacity.get()),
                queued_bytes: 0,
                terminal: None,
                failed: false,
            }),
            catalog: Mutex::new(CatalogCallbackState {
                queue: VecDeque::with_capacity(callback_limits.event_capacity.get()),
                queued_bytes: 0,
            }),
            commands: Mutex::new(None),
            wake,
        });
        (
            Self {
                config,
                callbacks: Arc::clone(&callbacks),
                task,
                active: None,
            },
            RithmicProviderEvents { callbacks },
        )
    }

    fn reap_finished(&mut self) -> Result<(), RithmicProviderDriverError> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.handle.is_finished())
        {
            let active = self
                .active
                .take()
                .ok_or(RithmicProviderDriverError::StopUnconfirmed)?;
            self.clear_commands(active.generation);
            let _ = active.handle.join();
        }
        Ok(())
    }

    fn owns_events(&self, events: &RithmicProviderEvents) -> bool {
        Arc::ptr_eq(&self.callbacks, &events.callbacks)
    }

    fn clear_commands(&self, generation: SessionGeneration) {
        let mut commands = self
            .callbacks
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if commands
            .as_ref()
            .is_some_and(|(active, _)| *active == generation)
        {
            *commands = None;
        }
    }
}

impl ProviderSessionDriver for RithmicProviderDriver {
    type Error = RithmicProviderDriverError;

    fn start_session(
        &mut self,
        generation: SessionGeneration,
        credentials: &[u8],
    ) -> Result<(), Self::Error> {
        self.reap_finished()?;
        if self.active.is_some() {
            return Err(RithmicProviderDriverError::SessionAlreadyActive);
        }
        let credentials = RithmicCredentialBytes::try_copy_from_vault(credentials)
            .map_err(|_| RithmicProviderDriverError::InvalidCredentials)?;
        {
            let mut callbacks = self
                .callbacks
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            callbacks.queue.clear();
            callbacks.queued_bytes = 0;
            callbacks.terminal = None;
            callbacks.failed = false;
        }
        {
            let mut catalog = self
                .callbacks
                .catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            catalog.queue.clear();
            catalog.queued_bytes = 0;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let abort = Arc::new(ConnectionAbort::default());
        let (command_tx, command_rx) = sync_channel(SESSION_COMMAND_CAPACITY);
        *self
            .callbacks
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((generation, command_tx));
        let task = Arc::clone(&self.task);
        let config = self.config.clone();
        let emitter = SessionEmitter {
            generation,
            callbacks: Arc::clone(&self.callbacks),
        };
        let panic_emitter = emitter.clone();
        let task_stop = Arc::clone(&stop);
        let task_abort = Arc::clone(&abort);
        let handle = thread::Builder::new()
            .name(format!("rithmic-session-{}", generation.get()))
            .spawn(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    task(
                        config,
                        generation,
                        credentials,
                        task_stop,
                        task_abort,
                        command_rx,
                        emitter,
                    );
                }))
                .is_err()
                {
                    panic_emitter.invalid(
                        ProviderInvalidationReason::MalformedMessage,
                        RetryDisposition::Terminal,
                    );
                }
            })
            .map_err(|_| {
                self.clear_commands(generation);
                RithmicProviderDriverError::ThreadUnavailable
            })?;
        self.active = Some(ActiveSession {
            generation,
            stop,
            abort,
            handle,
        });
        Ok(())
    }

    fn stop_session(&mut self, generation: SessionGeneration) -> Result<(), Self::Error> {
        let active = self
            .active
            .take()
            .ok_or(RithmicProviderDriverError::StaleGeneration)?;
        if active.generation != generation {
            self.active = Some(active);
            return Err(RithmicProviderDriverError::StaleGeneration);
        }
        active.stop.store(true, Ordering::Release);
        active.abort.abort();
        self.clear_commands(generation);
        let _ = active.handle.join();
        Ok(())
    }
}

impl Drop for RithmicProviderDriver {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.stop.store(true, Ordering::Release);
            active.abort.abort();
            self.clear_commands(active.generation);
            let _ = active.handle.join();
        }
    }
}

/// Retry ticket fenced to the exact generation that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicRetryTicket {
    pub failed_generation: SessionGeneration,
    pub due_at: Instant,
}

/// Nonblocking capped retry scheduler owned by the desktop worker loop.
#[derive(Clone, Debug, Default)]
pub struct RithmicRetryScheduler {
    next_delay: usize,
    ticket: Option<RithmicRetryTicket>,
}

impl RithmicRetryScheduler {
    #[must_use]
    pub const fn ticket(&self) -> Option<RithmicRetryTicket> {
        self.ticket
    }

    pub fn record_invalid(
        &mut self,
        generation: SessionGeneration,
        retry: RetryDisposition,
        now: Instant,
    ) -> Option<RithmicRetryTicket> {
        if retry == RetryDisposition::Terminal {
            self.ticket = None;
            return None;
        }
        let delay = RETRY_DELAYS[self.next_delay.min(RETRY_DELAYS.len() - 1)];
        self.next_delay = self
            .next_delay
            .saturating_add(1)
            .min(RETRY_DELAYS.len() - 1);
        let ticket = RithmicRetryTicket {
            failed_generation: generation,
            due_at: now + delay,
        };
        self.ticket = Some(ticket);
        Some(ticket)
    }

    pub fn established(&mut self) {
        self.next_delay = 0;
        self.ticket = None;
    }

    pub fn clear(&mut self) {
        self.ticket = None;
    }

    /// Starts a fresh generation only for an exact, due recovery ticket.
    ///
    /// # Errors
    ///
    /// Returns a shared runtime state, vault, or driver error.
    pub fn retry_due<T: Clone, V: CredentialVault>(
        &mut self,
        worker: &mut DesktopMarketWorker<T, V, RithmicProviderDriver>,
        now: Instant,
    ) -> Result<Option<SessionGeneration>, DesktopMarketWorkerError> {
        let state = worker.provider_state()?;
        if !self.take_due(state, now) {
            return Ok(None);
        }
        worker.connect(ConnectTrigger::Retry).map(Some)
    }

    fn take_due(&mut self, state: DesktopProviderState, now: Instant) -> bool {
        let Some(ticket) = self.ticket else {
            return false;
        };
        if now < ticket.due_at {
            return false;
        }
        let exact_recovery = matches!(
            state,
            DesktopProviderState::RecoveryRequired {
                generation: Some(generation),
                reason: RecoveryReason::TransportInvalid | RecoveryReason::SemanticQueueOverflow,
            } if generation == ticket.failed_generation
        );
        self.ticket = None;
        exact_recovery
    }
}

/// One native environmental transition delivered to the Rithmic worker owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicEnvironmentEvent {
    Network(NetworkEvent),
    Power(PowerEvent),
}

/// Applies one native environmental transition and fences adapter-local work.
///
/// Any pending transport retry belongs to the retired environment and is cleared.
/// Provider and catalog callbacks are discarded only after the shared runtime has
/// synchronously stopped the old session, so no stale callback can race the drain.
///
/// # Errors
///
/// Returns a redacted source, lifecycle, vault, driver, or history-retirement failure.
pub fn apply_rithmic_environment_event<T: Clone, V: CredentialVault>(
    worker: &mut DesktopMarketWorker<T, V, RithmicProviderDriver>,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    event: RithmicEnvironmentEvent,
) -> Result<Option<SessionGeneration>, RithmicDesktopEventError> {
    if !worker.callback_source_matches(|driver| driver.owns_events(events))? {
        return Err(DesktopMarketWorkerError::CallbackSourceMismatch.into());
    }
    retries.clear();
    let discard = || {
        while events.try_recv().is_some() {}
        while events.try_recv_catalog().is_some() {}
    };
    let generation = match event {
        RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable) => {
            let generation = worker.handle_network_event(NetworkEvent::Unavailable)?;
            discard();
            generation
        }
        RithmicEnvironmentEvent::Power(PowerEvent::Suspending) => {
            let generation = worker.handle_power_event(PowerEvent::Suspending)?;
            discard();
            generation
        }
        RithmicEnvironmentEvent::Network(NetworkEvent::Available) => {
            discard();
            worker.handle_network_event(NetworkEvent::Available)?
        }
        RithmicEnvironmentEvent::Power(PowerEvent::Resumed) => {
            discard();
            worker.handle_power_event(PowerEvent::Resumed)?
        }
    };
    Ok(generation)
}

/// One callback after shared lifecycle state has been updated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppliedRithmicEvent {
    Semantic(ProviderSessionEvent),
    RetryScheduled(RithmicRetryTicket),
    TerminalFailure {
        generation: SessionGeneration,
        reason: ProviderInvalidationReason,
    },
}

/// Redacted failures while applying Rithmic callbacks to the shared runtime.
#[derive(Debug)]
pub enum RithmicDesktopEventError {
    Runtime(DesktopMarketWorkerError),
    MissingRetryDisposition,
}

impl fmt::Display for RithmicDesktopEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "Rithmic callback failed: {error}"),
            Self::MissingRetryDisposition => {
                formatter.write_str("Rithmic invalidation omitted retry classification")
            }
        }
    }
}

impl Error for RithmicDesktopEventError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::MissingRetryDisposition => None,
        }
    }
}

impl From<DesktopMarketWorkerError> for RithmicDesktopEventError {
    fn from(error: DesktopMarketWorkerError) -> Self {
        Self::Runtime(error)
    }
}

/// Applies at most one generation-fenced semantic callback.
///
/// # Errors
///
/// Returns a redacted source, event-shape, or lifecycle failure.
pub fn try_recv_rithmic_event<T: Clone, V: CredentialVault>(
    worker: &mut DesktopMarketWorker<T, V, RithmicProviderDriver>,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    now: Instant,
) -> Result<Option<AppliedRithmicEvent>, RithmicDesktopEventError> {
    if !worker.callback_source_matches(|driver| driver.owns_events(events))? {
        return Err(DesktopMarketWorkerError::CallbackSourceMismatch.into());
    }
    let Some(callback) = events.try_recv() else {
        return Ok(None);
    };
    match worker.provider_state()? {
        DesktopProviderState::Connecting { generation, .. }
        | DesktopProviderState::Streaming { generation }
            if generation == callback.generation => {}
        DesktopProviderState::Connecting { .. } | DesktopProviderState::Streaming { .. } => {
            return Err(DesktopMarketWorkerError::HandoffGenerationMismatch.into());
        }
        _ => return Err(DesktopMarketWorkerError::ProviderNotStreaming.into()),
    }
    match &callback.event {
        ProviderSessionEvent::InstrumentsDiscovered { generation, .. } => {
            worker.session_established(*generation)?;
            retries.established();
        }
        ProviderSessionEvent::Market { generation, event } => {
            worker.ensure_streaming_generation(*generation)?;
            if let MarketEvent::Trade(trade) = event {
                worker.record_trade_diagnostics(
                    *generation,
                    trade.metadata.timestamps.exchange_unix_nanos,
                )?;
            }
        }
        ProviderSessionEvent::Heartbeat { generation, .. } => {
            worker.ensure_streaming_generation(*generation)?;
            worker.record_heartbeat_diagnostics(*generation)?;
        }
        ProviderSessionEvent::Invalidated {
            generation: Some(generation),
            reason,
        } => {
            if *reason == ProviderInvalidationReason::QueueOverflow {
                worker.session_callback_queue_overflow(*generation)?;
            } else {
                worker.session_invalid(*generation)?;
            }
            let retry = callback
                .retry
                .ok_or(RithmicDesktopEventError::MissingRetryDisposition)?;
            return if let Some(ticket) = retries.record_invalid(*generation, retry, now) {
                Ok(Some(AppliedRithmicEvent::RetryScheduled(ticket)))
            } else {
                Ok(Some(AppliedRithmicEvent::TerminalFailure {
                    generation: *generation,
                    reason: *reason,
                }))
            };
        }
        ProviderSessionEvent::DiscoveryStarted
        | ProviderSessionEvent::SystemsDiscovered { .. }
        | ProviderSessionEvent::AuthenticationChanged { .. }
        | ProviderSessionEvent::Invalidated {
            generation: None, ..
        }
        | ProviderSessionEvent::Stopped => {}
    }
    Ok(Some(AppliedRithmicEvent::Semantic(callback.event)))
}

fn direct_session_task() -> Arc<SessionTask> {
    Arc::new(
        |config, generation, credential_bytes, stop, abort, commands, emitter| {
            if !emitter.send(ProviderSessionEvent::DiscoveryStarted) {
                return;
            }
            let connection = {
                let Ok(credentials) = credential_bytes.credentials() else {
                    emitter.invalid(
                        ProviderInvalidationReason::Authentication,
                        RetryDisposition::Terminal,
                    );
                    return;
                };
                RithmicTestSession::discover_and_login_with_abort(
                    credentials,
                    RithmicApplication {
                        name: &config.application_name,
                        version: &config.application_version,
                    },
                    config.session_limits,
                    Arc::clone(&stop),
                    abort.as_ref(),
                )
            };
            drop(credential_bytes);
            let mut connection = match connection {
                Ok(connection) => connection,
                Err(error) => {
                    if error != RithmicSessionError::Cancelled && !stop.load(Ordering::Acquire) {
                        emit_connection_failure(&emitter, generation, error);
                    }
                    return;
                }
            };
            if !emitter.send(ProviderSessionEvent::SystemsDiscovered {
                environments: vec![RithmicProviderConfig::environment()],
            }) || !emitter.send(ProviderSessionEvent::AuthenticationChanged {
                generation,
                state: AuthenticationState::Accepted,
            }) {
                stop.store(true, Ordering::Release);
            }
            if stop.load(Ordering::Acquire) {
                let _ = connection.close();
                return;
            }
            if let Err(error) = install_subscriptions(&mut connection, &config, &stop) {
                if error != RithmicSessionError::Cancelled && !stop.load(Ordering::Acquire) {
                    let (reason, retry) = session_failure(error);
                    emitter.invalid(reason, retry);
                }
                return;
            }
            if !emitter.send(ProviderSessionEvent::InstrumentsDiscovered {
                generation,
                instruments: config
                    .instruments
                    .iter()
                    .map(|instrument| instrument.descriptor.clone())
                    .collect(),
            }) {
                stop.store(true, Ordering::Release);
            }
            let result = if stop.load(Ordering::Acquire) {
                Ok(())
            } else {
                collect_market(
                    &mut connection,
                    &config,
                    generation,
                    &stop,
                    &commands,
                    &emitter,
                )
            };
            if stop.load(Ordering::Acquire) {
                let _ = connection.close();
                return;
            }
            if let Err((reason, retry)) = result {
                emitter.invalid(reason, retry);
            }
        },
    )
}

fn emit_connection_failure(
    emitter: &SessionEmitter,
    generation: SessionGeneration,
    error: RithmicSessionError,
) {
    if error == RithmicSessionError::LoginRejected {
        let _ = emitter.send(ProviderSessionEvent::AuthenticationChanged {
            generation,
            state: AuthenticationState::Rejected,
        });
    }
    let (reason, retry) = session_failure(error);
    emitter.invalid(reason, retry);
}

fn install_subscriptions(
    connection: &mut crate::RithmicTickerConnection,
    config: &RithmicProviderConfig,
    stop: &AtomicBool,
) -> Result<(), RithmicSessionError> {
    for instrument in &config.instruments {
        connection.update_market_data(instrument.as_request())?;
        if stop.load(Ordering::Acquire) {
            return Err(RithmicSessionError::Cancelled);
        }
        match connection.read_next_until(Instant::now() + config.session_limits.response_timeout)? {
            RithmicSessionMessage::Control(DecodedControlMessage::MarketDataSubscription {
                accepted: true,
            }) => {}
            RithmicSessionMessage::Control(
                DecodedControlMessage::MarketDataSubscription { accepted: false }
                | DecodedControlMessage::Reject
                | DecodedControlMessage::ForcedLogout,
            ) => return Err(RithmicSessionError::Protocol),
            _ => return Err(RithmicSessionError::UnexpectedMessage),
        }
    }
    Ok(())
}

enum PendingCatalogCommand {
    Search {
        generation: NonZeroUsize,
        collector: SymbolSearchCollector,
    },
    Reference(RithmicInstrumentSelection),
}

enum SubscriptionPhase {
    Unsubscribe(usize),
    Subscribe,
}

struct PendingSubscription {
    selection: RithmicInstrumentSelection,
    instrument: RithmicProviderInstrument,
    previous: Vec<RithmicProviderInstrument>,
    phase: SubscriptionPhase,
}

type SymbolKey = (String, String);
type LatestSymbolSearch = (NonZeroUsize, BTreeMap<SymbolKey, SymbolSearchResult>);

#[derive(Default)]
struct CatalogCommandState {
    pending_catalog: Option<PendingCatalogCommand>,
    pending_subscription: Option<PendingSubscription>,
    latest_search: Option<LatestSymbolSearch>,
}

struct DirectSessionState {
    last_message: Instant,
    next_heartbeat: Instant,
    heartbeat_deadline: Option<Instant>,
    source_ordinal: u64,
    catalog: CatalogCommandState,
}

fn collect_market(
    connection: &mut crate::RithmicTickerConnection,
    config: &RithmicProviderConfig,
    generation: SessionGeneration,
    stop: &AtomicBool,
    commands: &Receiver<RithmicSessionCommand>,
    emitter: &SessionEmitter,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let mut canonical = CanonicalSessionState::try_new(config, generation)?;
    let heartbeat_interval = connection.heartbeat_interval();
    let silence_evidence_fault = config.silence_evidence_fault(generation);
    if let Some(fault) = silence_evidence_fault {
        return collect_authorized_silence_evidence(connection, config, stop, fault);
    }
    let started = Instant::now();
    let mut state = DirectSessionState {
        last_message: started,
        next_heartbeat: started + heartbeat_interval,
        heartbeat_deadline: None,
        source_ordinal: 0,
        catalog: CatalogCommandState::default(),
    };
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        if state.catalog.pending_catalog.is_none() && state.catalog.pending_subscription.is_none() {
            process_session_commands(
                connection,
                commands,
                generation,
                emitter,
                &mut state.catalog.pending_catalog,
                state.catalog.latest_search.as_ref(),
            )?;
        }
        let now = Instant::now();
        if let Some(invalidation) = silence_invalidation(&state, config, now) {
            return Err(invalidation);
        }
        if state.heartbeat_deadline.is_none() && now >= state.next_heartbeat {
            connection.send_heartbeat().map_err(session_failure)?;
            state.heartbeat_deadline = Some(now + config.session_limits.response_timeout);
            state.next_heartbeat = now + heartbeat_interval;
        }
        let mut deadline = (state.last_message + config.message_silence_timeout)
            .min(now + SESSION_COMMAND_POLL_INTERVAL);
        if let Some(pending) = state.heartbeat_deadline {
            deadline = deadline.min(pending);
        } else {
            deadline = deadline.min(state.next_heartbeat);
        }
        let message = match connection.read_next_until(deadline) {
            Ok(message) => message,
            Err(RithmicSessionError::Deadline) => continue,
            Err(RithmicSessionError::Cancelled) if stop.load(Ordering::Acquire) => return Ok(()),
            Err(error) => return Err(session_failure(error)),
        };
        state.last_message = Instant::now();
        state.source_ordinal = state.source_ordinal.checked_add(1).ok_or_else(malformed)?;
        if !handle_session_message(
            connection,
            message,
            generation,
            emitter,
            &mut state,
            &mut canonical,
            stop,
        )? {
            return Ok(());
        }
    }
}

fn collect_authorized_silence_evidence(
    connection: &mut crate::RithmicTickerConnection,
    config: &RithmicProviderConfig,
    stop: &AtomicBool,
    fault: RithmicAuthorizedSilenceEvidenceFault,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    let started = Instant::now();
    let mut state = DirectSessionState {
        last_message: started,
        next_heartbeat: started + config.message_silence_timeout,
        heartbeat_deadline: None,
        source_ordinal: 0,
        catalog: CatalogCommandState::default(),
    };
    if fault == RithmicAuthorizedSilenceEvidenceFault::Heartbeat {
        connection.send_heartbeat().map_err(session_failure)?;
        state.heartbeat_deadline = Some(started + config.session_limits.response_timeout);
    }
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let now = Instant::now();
        if let Some(invalidation) = silence_invalidation(&state, config, now) {
            return Err(invalidation);
        }
        let deadline = match fault {
            RithmicAuthorizedSilenceEvidenceFault::Heartbeat => state
                .heartbeat_deadline
                .unwrap_or(started + config.session_limits.response_timeout),
            RithmicAuthorizedSilenceEvidenceFault::Message => {
                started + config.message_silence_timeout
            }
        };
        thread::sleep(deadline.saturating_duration_since(now).min(POLL_INTERVAL));
    }
}

fn silence_invalidation(
    state: &DirectSessionState,
    config: &RithmicProviderConfig,
    now: Instant,
) -> Option<(ProviderInvalidationReason, RetryDisposition)> {
    if now.duration_since(state.last_message) >= config.message_silence_timeout {
        return Some((
            ProviderInvalidationReason::MessageSilence,
            RetryDisposition::Transient,
        ));
    }
    state
        .heartbeat_deadline
        .filter(|deadline| now >= *deadline)
        .map(|_| {
            (
                ProviderInvalidationReason::HeartbeatSilence,
                RetryDisposition::Transient,
            )
        })
}

fn handle_session_message(
    connection: &mut crate::RithmicTickerConnection,
    message: RithmicSessionMessage,
    generation: SessionGeneration,
    emitter: &SessionEmitter,
    state: &mut DirectSessionState,
    canonical: &mut CanonicalSessionState,
    stop: &AtomicBool,
) -> Result<bool, (ProviderInvalidationReason, RetryDisposition)> {
    match message {
        RithmicSessionMessage::Catalog(message) => {
            handle_catalog_message(
                connection,
                message,
                generation,
                emitter,
                &mut state.catalog,
                canonical,
            )?;
        }
        RithmicSessionMessage::Control(DecodedControlMessage::MarketDataSubscription {
            accepted,
        }) if state.catalog.pending_subscription.is_some() => {
            advance_subscription(
                connection,
                generation,
                emitter,
                accepted,
                &mut state.catalog.pending_subscription,
                canonical,
            )?;
        }
        RithmicSessionMessage::Market(message) => {
            let received_unix_nanos = unix_nanos_now()?;
            if let Some(event) =
                canonical.convert(message, state.source_ordinal, received_unix_nanos)?
                && !emitter.send(ProviderSessionEvent::Market { generation, event })
            {
                stop.store(true, Ordering::Release);
                return Ok(false);
            }
        }
        RithmicSessionMessage::Control(DecodedControlMessage::Heartbeat {
            accepted: true, ..
        }) => {
            state.heartbeat_deadline = None;
            if !emitter.send(ProviderSessionEvent::Heartbeat {
                generation,
                received_unix_nanos: unix_nanos_now()?,
            }) {
                stop.store(true, Ordering::Release);
                return Ok(false);
            }
        }
        RithmicSessionMessage::Control(DecodedControlMessage::ForcedLogout) => {
            return Err((
                ProviderInvalidationReason::Authentication,
                RetryDisposition::Terminal,
            ));
        }
        _ => return Err(malformed()),
    }
    Ok(true)
}

fn process_session_commands(
    connection: &mut crate::RithmicTickerConnection,
    commands: &Receiver<RithmicSessionCommand>,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
    pending: &mut Option<PendingCatalogCommand>,
    latest_search: Option<&LatestSymbolSearch>,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    for _ in 0..SESSION_COMMAND_BATCH {
        let command = match commands.try_recv() {
            Ok(command) => command,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(()),
        };
        match command {
            RithmicSessionCommand::Search(request) => {
                let collector = SymbolSearchCollector::try_new(request.collection_request())
                    .map_err(|_| malformed())?;
                if let Err(error) = connection.search_symbols(request.protocol_request()) {
                    let rejection = search_command_failure(error)?;
                    if !send_rejection(
                        emitter,
                        session_generation,
                        request.search_generation,
                        rejection,
                    ) {
                        return Err((
                            ProviderInvalidationReason::QueueOverflow,
                            RetryDisposition::Transient,
                        ));
                    }
                    continue;
                }
                *pending = Some(PendingCatalogCommand::Search {
                    generation: request.search_generation,
                    collector,
                });
                return Ok(());
            }
            RithmicSessionCommand::Select(selection) => {
                let available = latest_search.is_some_and(|(generation, symbols)| {
                    *generation == selection.search_generation
                        && symbols
                            .contains_key(&(selection.exchange.clone(), selection.symbol.clone()))
                });
                if !available {
                    let reason = latest_search.map_or(
                        RithmicCatalogRejection::SupersededSearch,
                        |(generation, _)| {
                            if *generation == selection.search_generation {
                                RithmicCatalogRejection::InstrumentUnavailable
                            } else {
                                RithmicCatalogRejection::SupersededSearch
                            }
                        },
                    );
                    if !send_rejection(
                        emitter,
                        session_generation,
                        selection.selection_generation,
                        reason,
                    ) {
                        return Ok(());
                    }
                    continue;
                }
                connection
                    .request_instrument_reference(InstrumentReferenceRequest {
                        symbol: &selection.symbol,
                        exchange: market_data_exchange(&selection.exchange),
                    })
                    .map_err(session_failure)?;
                *pending = Some(PendingCatalogCommand::Reference(selection));
                return Ok(());
            }
        }
    }
    Ok(())
}

fn search_command_failure(
    error: RithmicSessionError,
) -> Result<RithmicCatalogRejection, (ProviderInvalidationReason, RetryDisposition)> {
    if error == RithmicSessionError::RequestInFlight {
        Ok(RithmicCatalogRejection::SearchRejected)
    } else {
        Err(session_failure(error))
    }
}

fn handle_catalog_message(
    connection: &mut crate::RithmicTickerConnection,
    message: DecodedCatalogMessage,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
    state: &mut CatalogCommandState,
    canonical: &mut CanonicalSessionState,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let Some(pending) = state.pending_catalog.as_mut() else {
        return Err(malformed());
    };
    match pending {
        PendingCatalogCommand::Search {
            generation,
            collector,
        } => match collector.accept(message) {
            Ok(CollectionProgress::Pending) => Ok(()),
            Ok(CollectionProgress::Complete(symbols)) => {
                let search_generation = *generation;
                state.latest_search = Some((
                    search_generation,
                    symbols
                        .results
                        .iter()
                        .cloned()
                        .map(|symbol| ((symbol.exchange.clone(), symbol.symbol.clone()), symbol))
                        .collect(),
                ));
                state.pending_catalog = None;
                if !emitter.send_catalog(RithmicCatalogEvent::SearchCompleted {
                    session_generation,
                    search_generation,
                    symbols,
                }) {
                    return Err((
                        ProviderInvalidationReason::QueueOverflow,
                        RetryDisposition::Transient,
                    ));
                }
                Ok(())
            }
            Err(CollectorError::Rejected) => {
                let search_generation = *generation;
                state.pending_catalog = None;
                send_rejection(
                    emitter,
                    session_generation,
                    search_generation,
                    RithmicCatalogRejection::SearchRejected,
                );
                Ok(())
            }
            Ok(CollectionProgress::Unhandled(_)) | Err(_) => Err(malformed()),
        },
        PendingCatalogCommand::Reference(selection) => {
            let DecodedCatalogMessage::InstrumentReference(reference) = message else {
                return Err(malformed());
            };
            let selection = selection.clone();
            state.pending_catalog = None;
            let Some(reference) = reference else {
                send_rejection(
                    emitter,
                    session_generation,
                    selection.selection_generation,
                    RithmicCatalogRejection::InstrumentUnavailable,
                );
                return Ok(());
            };
            let instrument = selected_instrument(&selection, reference)?;
            let previous = canonical.instruments.clone();
            let phase = if previous.is_empty() {
                begin_subscription(connection, &instrument, canonical)?;
                SubscriptionPhase::Subscribe
            } else {
                connection
                    .update_market_data(subscription_request(
                        &previous[0],
                        SubscriptionAction::Unsubscribe,
                    ))
                    .map_err(session_failure)?;
                SubscriptionPhase::Unsubscribe(0)
            };
            state.pending_subscription = Some(PendingSubscription {
                selection,
                instrument,
                previous,
                phase,
            });
            Ok(())
        }
    }
}

fn begin_subscription(
    connection: &mut crate::RithmicTickerConnection,
    instrument: &RithmicProviderInstrument,
    canonical: &mut CanonicalSessionState,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    canonical.replace_instrument(instrument.clone())?;
    connection
        .update_market_data(subscription_request(
            instrument,
            SubscriptionAction::Subscribe,
        ))
        .map_err(session_failure)
}

fn advance_subscription(
    connection: &mut crate::RithmicTickerConnection,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
    accepted: bool,
    pending: &mut Option<PendingSubscription>,
    canonical: &mut CanonicalSessionState,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let Some(plan) = pending.as_mut() else {
        return Err(malformed());
    };
    if !accepted {
        send_rejection(
            emitter,
            session_generation,
            plan.selection.selection_generation,
            RithmicCatalogRejection::SubscriptionRejected,
        );
        return Err((
            ProviderInvalidationReason::Transport,
            RetryDisposition::Transient,
        ));
    }
    match plan.phase {
        SubscriptionPhase::Unsubscribe(index) => {
            let next = index + 1;
            if let Some(instrument) = plan.previous.get(next) {
                connection
                    .update_market_data(subscription_request(
                        instrument,
                        SubscriptionAction::Unsubscribe,
                    ))
                    .map_err(session_failure)?;
                plan.phase = SubscriptionPhase::Unsubscribe(next);
            } else {
                begin_subscription(connection, &plan.instrument, canonical)?;
                plan.phase = SubscriptionPhase::Subscribe;
            }
            Ok(())
        }
        SubscriptionPhase::Subscribe => {
            let plan = pending.take().ok_or_else(malformed)?;
            if !emitter.send_catalog(RithmicCatalogEvent::SelectionInstalled {
                session_generation,
                selection_generation: plan.selection.selection_generation,
                instrument: plan.instrument.descriptor,
            }) {
                return Err((
                    ProviderInvalidationReason::QueueOverflow,
                    RetryDisposition::Transient,
                ));
            }
            Ok(())
        }
    }
}

fn selected_instrument(
    selection: &RithmicInstrumentSelection,
    reference: InstrumentReference,
) -> Result<RithmicProviderInstrument, (ProviderInvalidationReason, RetryDisposition)> {
    if reference.symbol != selection.symbol
        || reference.exchange != market_data_exchange(&selection.exchange)
    {
        return Err(malformed());
    }
    let instrument_id = format!("rithmic:{}:{}", reference.exchange, reference.symbol);
    let descriptor = InstrumentDescriptor {
        instrument_id,
        provider_symbol: reference.symbol.clone(),
        display_symbol: reference.name.unwrap_or(reference.symbol),
        venue_id: reference.exchange,
        price_scale: reference.price_precision.unwrap_or(0),
        quantity_scale: 0,
    };
    if descriptor.validate().is_err() {
        return Err(malformed());
    }
    Ok(RithmicProviderInstrument {
        descriptor,
        entitlement_id: selection.entitlement_id.clone(),
        trades: selection.trades,
        quotes: selection.quotes,
        order_book: selection.order_book,
    })
}

fn market_data_exchange(catalog_exchange: &str) -> &str {
    catalog_exchange
        .strip_suffix("-Delayed")
        .unwrap_or(catalog_exchange)
}

fn subscription_request(
    instrument: &RithmicProviderInstrument,
    action: SubscriptionAction,
) -> MarketDataSubscription<'_> {
    MarketDataSubscription {
        symbol: &instrument.descriptor.provider_symbol,
        exchange: &instrument.descriptor.venue_id,
        action,
        trades: instrument.trades,
        quotes: instrument.quotes,
        order_book: instrument.order_book,
    }
}

fn send_rejection(
    emitter: &SessionEmitter,
    session_generation: SessionGeneration,
    command_generation: NonZeroUsize,
    reason: RithmicCatalogRejection,
) -> bool {
    emitter.send_catalog(RithmicCatalogEvent::CommandRejected {
        session_generation,
        command_generation,
        reason,
    })
}

struct CanonicalSessionState {
    instruments: Vec<RithmicProviderInstrument>,
    generation: SessionGeneration,
    quotes: BTreeMap<String, QuoteState>,
    books: BTreeMap<String, AggregateBookAssembler>,
}

#[derive(Clone, Copy, Default)]
struct QuoteState {
    bid: Option<QuoteLevel>,
    ask: Option<QuoteLevel>,
}

impl CanonicalSessionState {
    fn try_new(
        config: &RithmicProviderConfig,
        generation: SessionGeneration,
    ) -> Result<Self, (ProviderInvalidationReason, RetryDisposition)> {
        let mut books = BTreeMap::new();
        for instrument in &config.instruments {
            if instrument.order_book {
                let limits = AggregateBookLimits::try_new(
                    NonZeroUsize::new(4_096).unwrap_or(NonZeroUsize::MIN),
                    NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
                )
                .map_err(|_| malformed())?;
                books.insert(
                    instrument.descriptor.instrument_id.clone(),
                    AggregateBookAssembler::new(
                        MarketIdentity {
                            symbol: instrument.descriptor.provider_symbol.clone(),
                            exchange: instrument.descriptor.venue_id.clone(),
                        },
                        limits,
                    ),
                );
            }
        }
        Ok(Self {
            instruments: config.instruments.clone(),
            generation,
            quotes: BTreeMap::new(),
            books,
        })
    }

    fn convert(
        &mut self,
        message: DecodedMarketMessage,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        match message {
            DecodedMarketMessage::Trade(trade) => {
                let instrument = self.instrument(&trade.identity)?;
                let event = MarketEvent::Trade(MarketTrade {
                    metadata: metadata(
                        instrument,
                        self.generation,
                        source_ordinal,
                        Some(trade.timestamp),
                        received_unix_nanos,
                    )?,
                    trade_id: format!("rithmic-local-{}-{source_ordinal}", self.generation.get()),
                    price: fixed_price(trade.price, instrument.descriptor.price_scale)?,
                    quantity: fixed_quantity(trade.size, instrument.descriptor.quantity_scale)?,
                    aggressor: match trade.aggressor {
                        Some(TradeAggressor::Buy) => AggressorSide::Buy,
                        Some(TradeAggressor::Sell) => AggressorSide::Sell,
                        None => AggressorSide::Unknown,
                    },
                });
                validate_market(event).map(Some)
            }
            DecodedMarketMessage::Quote(quote) => {
                let instrument = self.instrument(&quote.identity)?.clone();
                let state = self
                    .quotes
                    .entry(instrument.descriptor.instrument_id.clone())
                    .or_default();
                if quote.is_snapshot {
                    *state = QuoteState::default();
                }
                apply_quote_side(&mut state.bid, quote.bid);
                apply_quote_side(&mut state.ask, quote.ask);
                let Some((bid, ask)) = state.bid.zip(state.ask) else {
                    return Ok(None);
                };
                let event = MarketEvent::Quote(TopOfBookQuote {
                    metadata: metadata(
                        &instrument,
                        self.generation,
                        source_ordinal,
                        Some(quote.timestamp),
                        received_unix_nanos,
                    )?,
                    bid_price: fixed_price(bid.price, instrument.descriptor.price_scale)?,
                    bid_quantity: fixed_quantity(bid.size, instrument.descriptor.quantity_scale)?,
                    ask_price: fixed_price(ask.price, instrument.descriptor.price_scale)?,
                    ask_quantity: fixed_quantity(ask.size, instrument.descriptor.quantity_scale)?,
                });
                validate_market(event).map(Some)
            }
            DecodedMarketMessage::OrderBook(update) => {
                let instrument = self.instrument(&update.identity)?.clone();
                let assembler = self
                    .books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                match assembler.accept(update, source_ordinal) {
                    AggregateBookOutcome::Pending => Ok(None),
                    AggregateBookOutcome::Snapshot(image) => {
                        let bids = canonical_levels(
                            &image.bids,
                            instrument.descriptor.price_scale,
                            instrument.descriptor.quantity_scale,
                        )?;
                        let asks = canonical_levels(
                            &image.asks,
                            instrument.descriptor.price_scale,
                            instrument.descriptor.quantity_scale,
                        )?;
                        let event = MarketEvent::DepthSnapshot(DepthSnapshot {
                            metadata: metadata(
                                &instrument,
                                self.generation,
                                image.source_ordinal,
                                image.timestamp,
                                received_unix_nanos,
                            )?,
                            bids,
                            asks,
                        });
                        validate_market(event).map(Some)
                    }
                    AggregateBookOutcome::RecoveryRequired { .. }
                    | AggregateBookOutcome::Unavailable { .. } => Err((
                        ProviderInvalidationReason::SequenceGap,
                        RetryDisposition::Transient,
                    )),
                }
            }
        }
    }

    fn instrument(
        &self,
        identity: &MarketIdentity,
    ) -> Result<&RithmicProviderInstrument, (ProviderInvalidationReason, RetryDisposition)> {
        self.instruments
            .iter()
            .find(|instrument| instrument.identity_matches(identity))
            .ok_or_else(malformed)
    }

    fn replace_instrument(
        &mut self,
        instrument: RithmicProviderInstrument,
    ) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
        self.instruments.clear();
        self.instruments.push(instrument);
        self.quotes.clear();
        self.books.clear();
        let instrument = &self.instruments[0];
        if instrument.order_book {
            let limits = AggregateBookLimits::try_new(
                NonZeroUsize::new(4_096).unwrap_or(NonZeroUsize::MIN),
                NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
            )
            .map_err(|_| malformed())?;
            self.books.insert(
                instrument.descriptor.instrument_id.clone(),
                AggregateBookAssembler::new(
                    MarketIdentity {
                        symbol: instrument.descriptor.provider_symbol.clone(),
                        exchange: instrument.descriptor.venue_id.clone(),
                    },
                    limits,
                ),
            );
        }
        Ok(())
    }
}

fn apply_quote_side(state: &mut Option<QuoteLevel>, update: QuoteSideUpdate) {
    match update {
        QuoteSideUpdate::Unchanged => {}
        QuoteSideUpdate::Cleared => *state = None,
        QuoteSideUpdate::Value(level) => *state = Some(level),
    }
}

fn metadata(
    instrument: &RithmicProviderInstrument,
    generation: SessionGeneration,
    source_sequence: u64,
    timestamp: Option<ProviderTimestamp>,
    received_unix_nanos: i64,
) -> Result<EventMetadata, (ProviderInvalidationReason, RetryDisposition)> {
    Ok(EventMetadata {
        provider_id: PROVIDER_ID.to_string(),
        instrument_id: instrument.descriptor.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        source_sequence,
        session_generation: generation.get(),
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: timestamp.map(provider_timestamp_nanos).transpose()?,
            provider_unix_nanos: None,
            received_unix_nanos,
        },
    })
}

fn canonical_levels(
    levels: &[OrderBookLevel],
    price_scale: u8,
    quantity_scale: u8,
) -> Result<Vec<DepthLevel>, (ProviderInvalidationReason, RetryDisposition)> {
    levels
        .iter()
        .map(|level| {
            Ok(DepthLevel {
                price: fixed_price(level.price, price_scale)?,
                quantity: fixed_quantity(level.size, quantity_scale)?,
                order_count: level.orders,
            })
        })
        .collect()
}

fn fixed_price(
    value: f64,
    scale: u8,
) -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    if !value.is_finite() || value <= 0.0 || scale > 18 {
        return Err(malformed());
    }
    let rendered = format!("{value:.precision$}", precision = usize::from(scale));
    let round_trip = rendered.parse::<f64>().map_err(|_| malformed())?;
    let tolerance = f64::EPSILON * value.abs().max(1.0) * 4.0;
    if (round_trip - value).abs() > tolerance {
        return Err(malformed());
    }
    let digits = rendered.replace('.', "");
    digits.parse::<i64>().map_err(|_| malformed())
}

fn fixed_quantity(
    value: u32,
    scale: u8,
) -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    if value == 0 || scale > 18 {
        return Err(malformed());
    }
    i64::from(value)
        .checked_mul(10_i64.checked_pow(u32::from(scale)).ok_or_else(malformed)?)
        .ok_or_else(malformed)
}

fn provider_timestamp_nanos(
    timestamp: ProviderTimestamp,
) -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    i64::from(timestamp.seconds)
        .checked_mul(1_000_000_000)
        .and_then(|value| {
            i64::from(timestamp.microseconds)
                .checked_mul(1_000)
                .and_then(|microseconds| value.checked_add(microseconds))
        })
        .ok_or_else(malformed)
}

fn unix_nanos_now() -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| malformed())?
        .as_nanos();
    i64::try_from(nanos).map_err(|_| malformed())
}

fn validate_market(
    event: MarketEvent,
) -> Result<MarketEvent, (ProviderInvalidationReason, RetryDisposition)> {
    event.validate(4_096).map_err(|_| malformed())?;
    Ok(event)
}

const fn malformed() -> (ProviderInvalidationReason, RetryDisposition) {
    (
        ProviderInvalidationReason::MalformedMessage,
        RetryDisposition::Terminal,
    )
}

fn session_failure(error: RithmicSessionError) -> (ProviderInvalidationReason, RetryDisposition) {
    let reason = match error {
        RithmicSessionError::LoginRejected => ProviderInvalidationReason::Authentication,
        RithmicSessionError::TestSystemUnavailable => ProviderInvalidationReason::UnsupportedSystem,
        RithmicSessionError::SchemaMismatch | RithmicSessionError::KitUnavailable => {
            ProviderInvalidationReason::SchemaMismatch
        }
        RithmicSessionError::Protocol
        | RithmicSessionError::UnexpectedMessage
        | RithmicSessionError::RequestInFlight
        | RithmicSessionError::InvalidEndpoint
        | RithmicSessionError::InvalidLimits => ProviderInvalidationReason::MalformedMessage,
        RithmicSessionError::Cancelled
        | RithmicSessionError::Resolve
        | RithmicSessionError::Connect
        | RithmicSessionError::Tls
        | RithmicSessionError::Handshake
        | RithmicSessionError::Deadline
        | RithmicSessionError::Transport
        | RithmicSessionError::DiscoveryRejected
        | RithmicSessionError::DiscoveryClose => ProviderInvalidationReason::Transport,
    };
    (reason, error.retry_disposition())
}

fn retained_event_bytes(event: &ProviderSessionEvent) -> Option<usize> {
    let mut bytes = size_of::<ProviderSessionEvent>();
    let mut add = |value: usize| {
        bytes = bytes.checked_add(value)?;
        Some(())
    };
    match event {
        ProviderSessionEvent::SystemsDiscovered { environments } => {
            add(environments
                .len()
                .checked_mul(size_of::<ProviderEnvironment>())?)?;
            for environment in environments {
                add(environment.provider_id.capacity())?;
                add(environment.system_id.capacity())?;
                add(environment.environment.capacity())?;
            }
        }
        ProviderSessionEvent::InstrumentsDiscovered { instruments, .. } => {
            add(instruments
                .len()
                .checked_mul(size_of::<InstrumentDescriptor>())?)?;
            for instrument in instruments {
                add(instrument.instrument_id.capacity())?;
                add(instrument.provider_symbol.capacity())?;
                add(instrument.display_symbol.capacity())?;
                add(instrument.venue_id.capacity())?;
            }
        }
        ProviderSessionEvent::Market { event, .. } => {
            let metadata = event.metadata();
            add(metadata.provider_id.capacity())?;
            add(metadata.instrument_id.capacity())?;
            add(metadata.entitlement_id.capacity())?;
            match event {
                MarketEvent::Trade(trade) => add(trade.trade_id.capacity())?,
                MarketEvent::DepthSnapshot(snapshot) => {
                    add(snapshot
                        .bids
                        .capacity()
                        .checked_mul(size_of::<DepthLevel>())?)?;
                    add(snapshot
                        .asks
                        .capacity()
                        .checked_mul(size_of::<DepthLevel>())?)?;
                }
                MarketEvent::Quote(_) | MarketEvent::DepthDelta(_) => {}
            }
        }
        ProviderSessionEvent::DiscoveryStarted
        | ProviderSessionEvent::AuthenticationChanged { .. }
        | ProviderSessionEvent::Heartbeat { .. }
        | ProviderSessionEvent::Invalidated { .. }
        | ProviderSessionEvent::Stopped => {}
    }
    Some(bytes)
}

fn retained_catalog_event_bytes(event: &RithmicCatalogEvent) -> Option<usize> {
    let mut bytes = size_of::<RithmicCatalogEvent>();
    let mut add = |value: usize| {
        bytes = bytes.checked_add(value)?;
        Some(())
    };
    match event {
        RithmicCatalogEvent::SearchCompleted { symbols, .. } => {
            add(symbols
                .results
                .len()
                .checked_mul(size_of::<SymbolSearchResult>())?)?;
            for symbol in &symbols.results {
                add(symbol.symbol.capacity())?;
                add(symbol.exchange.capacity())?;
                for field in [
                    &symbol.name,
                    &symbol.product_code,
                    &symbol.instrument_type,
                    &symbol.expiration_date,
                ]
                .into_iter()
                .flatten()
                {
                    add(field.capacity())?;
                }
            }
        }
        RithmicCatalogEvent::SelectionInstalled { instrument, .. } => {
            add(instrument.instrument_id.capacity())?;
            add(instrument.provider_symbol.capacity())?;
            add(instrument.display_symbol.capacity())?;
            add(instrument.venue_id.capacity())?;
        }
        RithmicCatalogEvent::CommandRejected { .. } => {}
    }
    Some(bytes)
}

const fn catalog_event_generation(event: &RithmicCatalogEvent) -> SessionGeneration {
    match event {
        RithmicCatalogEvent::SearchCompleted {
            session_generation, ..
        }
        | RithmicCatalogEvent::SelectionInstalled {
            session_generation, ..
        }
        | RithmicCatalogEvent::CommandRejected {
            session_generation, ..
        } => *session_generation,
    }
}

fn event_generation(event: &ProviderSessionEvent) -> Option<SessionGeneration> {
    match event {
        ProviderSessionEvent::AuthenticationChanged { generation, .. }
        | ProviderSessionEvent::InstrumentsDiscovered { generation, .. }
        | ProviderSessionEvent::Market { generation, .. }
        | ProviderSessionEvent::Heartbeat { generation, .. } => Some(*generation),
        ProviderSessionEvent::Invalidated { generation, .. } => *generation,
        ProviderSessionEvent::DiscoveryStarted
        | ProviderSessionEvent::SystemsDiscovered { .. }
        | ProviderSessionEvent::Stopped => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, OrderBookSides, OrderBookUpdate,
        OrderBookUpdateKind, QuoteUpdate, TradeUpdate,
    };
    use axiusflow_desktop_history::HistoryWorkerConfig;
    use axiusflow_desktop_provider_runtime::{DesktopMarketWorkerConfig, DesktopProviderConfig};
    use axiusflow_desktop_storage::CatalogKey;
    use std::{
        fs,
        io::Read,
        net::{TcpListener, TcpStream},
        num::NonZeroU64,
        path::PathBuf,
        sync::Barrier,
        thread,
    };

    struct MemoryVault(Vec<u8>);

    impl CredentialVault for MemoryVault {
        type Error = ();

        fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            Ok(Some(self.0.clone()))
        }

        fn delete(&self, _key: &str) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
    }

    fn generation(value: u64) -> SessionGeneration {
        SessionGeneration::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN))
    }

    fn identity() -> MarketIdentity {
        MarketIdentity {
            symbol: "ESM7".to_string(),
            exchange: "CME".to_string(),
        }
    }

    fn timestamp() -> ProviderTimestamp {
        ProviderTimestamp {
            seconds: 1_700_000_000,
            microseconds: 123_456,
        }
    }

    fn instrument() -> RithmicProviderInstrument {
        RithmicProviderInstrument {
            descriptor: InstrumentDescriptor {
                instrument_id: "future-cme-es-2027-06".to_string(),
                provider_symbol: "ESM7".to_string(),
                display_symbol: "ES Jun 2027".to_string(),
                venue_id: "CME".to_string(),
                price_scale: 2,
                quantity_scale: 2,
            },
            entitlement_id: "rithmic-test-cme".to_string(),
            trades: true,
            quotes: true,
            order_book: true,
        }
    }

    fn config() -> RithmicProviderConfig {
        RithmicProviderConfig::try_new(
            "AxiusFlow",
            "0.1.0",
            RithmicSessionLimits::default(),
            Duration::from_secs(30),
            vec![instrument()],
        )
        .expect("fixture configuration validates")
    }

    #[test]
    fn discovery_only_configuration_does_not_require_a_guessed_instrument() {
        let config = RithmicProviderConfig::try_new(
            "AxiusFlow",
            "0.1.0",
            RithmicSessionLimits::default(),
            Duration::from_secs(30),
            Vec::new(),
        )
        .expect("discovery-only configuration validates");

        assert!(config.instruments.is_empty());
        assert_eq!(
            RithmicProviderConfig::environment(),
            ProviderEnvironment {
                provider_id: "rithmic".to_string(),
                system_id: "RITHMIC_TEST".to_string(),
                environment: "Test".to_string(),
            }
        );
    }

    #[test]
    fn silence_deadlines_select_exact_transient_invalidation_reason() {
        let config = config();
        let started = Instant::now();
        let state = DirectSessionState {
            last_message: started,
            next_heartbeat: started + Duration::from_secs(10),
            heartbeat_deadline: Some(started + Duration::from_secs(5)),
            source_ordinal: 0,
            catalog: CatalogCommandState::default(),
        };
        assert_eq!(
            silence_invalidation(&state, &config, started + Duration::from_secs(5)),
            Some((
                ProviderInvalidationReason::HeartbeatSilence,
                RetryDisposition::Transient
            ))
        );
        assert_eq!(
            silence_invalidation(&state, &config, started + Duration::from_secs(30)),
            Some((
                ProviderInvalidationReason::MessageSilence,
                RetryDisposition::Transient
            ))
        );
    }

    #[test]
    fn silence_evidence_faults_are_generation_scoped_and_reconnect_stops_cleanly() {
        let task: Arc<SessionTask> = Arc::new(|config, generation, _, stop, _, _, emitter| {
            if !emitter.send(ProviderSessionEvent::DiscoveryStarted)
                || !emitter.send(ProviderSessionEvent::SystemsDiscovered {
                    environments: vec![RithmicProviderConfig::environment()],
                })
                || !emitter.send(ProviderSessionEvent::AuthenticationChanged {
                    generation,
                    state: AuthenticationState::Accepted,
                })
                || !emitter.send(ProviderSessionEvent::InstrumentsDiscovered {
                    generation,
                    instruments: Vec::new(),
                })
            {
                return;
            }
            if let Some(fault) = config.silence_evidence_fault(generation) {
                let reason = match fault {
                    RithmicAuthorizedSilenceEvidenceFault::Heartbeat => {
                        ProviderInvalidationReason::HeartbeatSilence
                    }
                    RithmicAuthorizedSilenceEvidenceFault::Message => {
                        ProviderInvalidationReason::MessageSilence
                    }
                };
                emitter.invalid(reason, RetryDisposition::Transient);
                return;
            }
            while !stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
        });
        let config = config()
            .with_authorized_silence_evidence_fault(
                generation(1),
                RithmicAuthorizedSilenceEvidenceFault::Message,
            )
            .with_authorized_silence_evidence_fault(
                generation(2),
                RithmicAuthorizedSilenceEvidenceFault::Heartbeat,
            );
        let (mut driver, events) =
            RithmicProviderDriver::with_task(config, callback_limits(16, 64 * 1_024), task);
        let credentials = credentials();

        for (current, expected) in [
            (generation(1), ProviderInvalidationReason::MessageSilence),
            (generation(2), ProviderInvalidationReason::HeartbeatSilence),
        ] {
            driver
                .start_session(current, credentials.as_bytes())
                .expect("fault generation starts");
            let terminal = loop {
                let callback = wait_event(&events);
                if matches!(callback.event, ProviderSessionEvent::Invalidated { .. }) {
                    break callback;
                }
            };
            assert_eq!(
                terminal.event,
                ProviderSessionEvent::Invalidated {
                    generation: Some(current),
                    reason: expected,
                }
            );
            assert_eq!(terminal.retry, Some(RetryDisposition::Transient));
            driver
                .stop_session(current)
                .expect("finished fault generation joins cleanly");
        }

        let recovered = generation(3);
        driver
            .start_session(recovered, credentials.as_bytes())
            .expect("fresh recovery generation starts");
        let mut established = false;
        while !established {
            established = matches!(
                wait_event(&events).event,
                ProviderSessionEvent::InstrumentsDiscovered {
                    generation: callback_generation,
                    ..
                } if callback_generation == recovered
            );
        }
        driver
            .stop_session(recovered)
            .expect("recovered generation stops cleanly");
    }

    fn callback_limits(events: usize, bytes: usize) -> RithmicCallbackLimits {
        RithmicCallbackLimits::try_new(nonzero(events), nonzero(bytes), nonzero(32))
            .expect("fixture callback limits validate")
    }

    fn credentials() -> RithmicCredentialBytes {
        RithmicCredentialBytes::try_encode("fixture-user", "fixture-password")
            .expect("fixture credentials encode")
    }

    fn wait_event(events: &RithmicProviderEvents) -> RithmicProviderCallback {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(event) = events.try_recv() {
                return event;
            }
            assert!(Instant::now() < deadline, "Rithmic callback timed out");
            thread::yield_now();
        }
    }

    fn runtime_task() -> Arc<SessionTask> {
        Arc::new(|config, generation, _, _, _, _, emitter| {
            if !emitter.send(ProviderSessionEvent::DiscoveryStarted)
                || !emitter.send(ProviderSessionEvent::SystemsDiscovered {
                    environments: vec![RithmicProviderConfig::environment()],
                })
                || !emitter.send(ProviderSessionEvent::AuthenticationChanged {
                    generation,
                    state: AuthenticationState::Accepted,
                })
                || !emitter.send(ProviderSessionEvent::InstrumentsDiscovered {
                    generation,
                    instruments: config
                        .instruments
                        .iter()
                        .map(|instrument| instrument.descriptor.clone())
                        .collect(),
                })
            {
                return;
            }
            emitter.invalid(
                ProviderInvalidationReason::Transport,
                RetryDisposition::Transient,
            );
        })
    }

    fn stable_runtime_task() -> Arc<SessionTask> {
        Arc::new(|config, generation, _, stop, _, _, emitter| {
            if !emitter.send(ProviderSessionEvent::DiscoveryStarted)
                || !emitter.send(ProviderSessionEvent::SystemsDiscovered {
                    environments: vec![RithmicProviderConfig::environment()],
                })
                || !emitter.send(ProviderSessionEvent::AuthenticationChanged {
                    generation,
                    state: AuthenticationState::Accepted,
                })
                || !emitter.send(ProviderSessionEvent::InstrumentsDiscovered {
                    generation,
                    instruments: config
                        .instruments
                        .iter()
                        .map(|instrument| instrument.descriptor.clone())
                        .collect(),
                })
                || !emitter.send(ProviderSessionEvent::Heartbeat {
                    generation,
                    received_unix_nanos: 1,
                })
            {
                return;
            }
            while !stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
        })
    }

    fn open_worker(
        driver: RithmicProviderDriver,
    ) -> (
        DesktopMarketWorker<u64, MemoryVault, RithmicProviderDriver>,
        PathBuf,
    ) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "axiusflow-rithmic-driver-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("history root creates");
        let ui_thread = thread::spawn(|| thread::current().id())
            .join()
            .expect("UI thread identity is captured");
        let encoded = credentials();
        let worker = DesktopMarketWorker::try_open(
            MemoryVault(encoded.as_bytes().to_vec()),
            driver,
            RITHMIC_TEST_VAULT_KEY,
            &root,
            CatalogKey::try_new("catalog-key-v1".to_string(), [0x52; 32])
                .expect("catalog key validates"),
            ui_thread,
            DesktopMarketWorkerConfig {
                provider: DesktopProviderConfig::new(
                    nonzero(16),
                    nonzero(MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES),
                )
                .with_diagnostics(RithmicProviderConfig::environment(), None)
                .expect("diagnostics identity validates"),
                history: HistoryWorkerConfig {
                    maximum_cache_entries: nonzero(2),
                    maximum_decoded_bytes: nonzero(1_024),
                    maximum_charts: nonzero(2),
                    maximum_segment_read_bytes: nonzero(1_024),
                    maximum_buffered_live: nonzero(2),
                    maximum_handoffs: nonzero(2),
                },
                maximum_catalog_entries: 2,
            },
        )
        .expect("market worker opens");
        (worker, root)
    }

    fn wait_applied(
        worker: &mut DesktopMarketWorker<u64, MemoryVault, RithmicProviderDriver>,
        events: &RithmicProviderEvents,
        retries: &mut RithmicRetryScheduler,
        now: Instant,
    ) -> AppliedRithmicEvent {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(event) =
                try_recv_rithmic_event(worker, events, retries, now).expect("callback applies")
            {
                return event;
            }
            assert!(Instant::now() < deadline, "Rithmic callback timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn configuration_requires_unique_explicit_registry_entries() {
        let duplicate = instrument();
        assert_eq!(
            RithmicProviderConfig::try_new(
                "AxiusFlow",
                "0.1.0",
                RithmicSessionLimits::default(),
                Duration::from_secs(30),
                vec![instrument(), duplicate],
            ),
            Err(RithmicProviderConfigError)
        );

        let mut unselected = instrument();
        unselected.trades = false;
        unselected.quotes = false;
        unselected.order_book = false;
        assert_eq!(
            RithmicProviderConfig::try_new(
                "AxiusFlow",
                "0.1.0",
                RithmicSessionLimits::default(),
                Duration::from_secs(30),
                vec![unselected],
            ),
            Err(RithmicProviderConfigError)
        );
    }

    #[test]
    fn trade_conversion_uses_registry_identity_scales_and_local_ordering() {
        let config = config();
        let mut canonical =
            CanonicalSessionState::try_new(&config, generation(7)).expect("state initializes");
        let converted = canonical
            .convert(
                DecodedMarketMessage::Trade(TradeUpdate {
                    identity: identity(),
                    price: 5_100.25,
                    size: 3,
                    aggressor: Some(TradeAggressor::Buy),
                    is_snapshot: false,
                    timestamp: timestamp(),
                }),
                11,
                1_700_000_000_999_000_000,
            )
            .expect("trade converts")
            .expect("trade produces an event");

        let MarketEvent::Trade(trade) = converted else {
            panic!("trade input must produce a trade event");
        };
        assert_eq!(trade.metadata.provider_id, PROVIDER_ID);
        assert_eq!(trade.metadata.instrument_id, "future-cme-es-2027-06");
        assert_eq!(trade.metadata.entitlement_id, "rithmic-test-cme");
        assert_eq!(trade.metadata.source_sequence, 11);
        assert_eq!(trade.metadata.session_generation, 7);
        assert_eq!(
            trade.metadata.timestamps.exchange_unix_nanos,
            Some(1_700_000_000_123_456_000)
        );
        assert_eq!(trade.trade_id, "rithmic-local-7-11");
        assert_eq!(trade.price, 510_025);
        assert_eq!(trade.quantity, 300);
        assert_eq!(trade.aggressor, AggressorSide::Buy);

        assert_eq!(
            canonical.convert(
                DecodedMarketMessage::Trade(TradeUpdate {
                    identity: identity(),
                    price: 5_100.251,
                    size: 1,
                    aggressor: None,
                    is_snapshot: false,
                    timestamp: timestamp(),
                }),
                12,
                1_700_000_001_000_000_000,
            ),
            Err(malformed())
        );
    }

    #[test]
    fn pending_subscription_accepts_market_data_before_provider_acknowledgement() {
        let empty = RithmicProviderConfig::try_new(
            "AxiusFlow",
            "0.1.0",
            RithmicSessionLimits::default(),
            Duration::from_secs(30),
            Vec::new(),
        )
        .expect("empty dynamic catalog config validates");
        let mut canonical =
            CanonicalSessionState::try_new(&empty, generation(9)).expect("state initializes");
        canonical
            .replace_instrument(instrument())
            .expect("pending instrument installs before subscribe");

        let event = canonical
            .convert(
                DecodedMarketMessage::Trade(TradeUpdate {
                    identity: identity(),
                    price: 5_100.25,
                    size: 1,
                    aggressor: None,
                    is_snapshot: false,
                    timestamp: timestamp(),
                }),
                1,
                1_700_000_000_999_000_000,
            )
            .expect("market-before-ack converts")
            .expect("trade produces an event");
        assert!(matches!(event, MarketEvent::Trade(_)));
    }

    #[test]
    fn quote_conversion_preserves_unchanged_and_cleared_sides() {
        let config = config();
        let mut canonical =
            CanonicalSessionState::try_new(&config, generation(3)).expect("state initializes");
        let bid = QuoteLevel {
            price: 5_100.25,
            size: 2,
            orders: Some(1),
        };
        let ask = QuoteLevel {
            price: 5_100.50,
            size: 4,
            orders: Some(2),
        };
        assert_eq!(
            canonical.convert(
                DecodedMarketMessage::Quote(QuoteUpdate {
                    identity: identity(),
                    bid: QuoteSideUpdate::Value(bid),
                    ask: QuoteSideUpdate::Unchanged,
                    is_snapshot: false,
                    timestamp: timestamp(),
                }),
                1,
                1,
            ),
            Ok(None)
        );
        let quote = canonical
            .convert(
                DecodedMarketMessage::Quote(QuoteUpdate {
                    identity: identity(),
                    bid: QuoteSideUpdate::Unchanged,
                    ask: QuoteSideUpdate::Value(ask),
                    is_snapshot: false,
                    timestamp: timestamp(),
                }),
                2,
                2,
            )
            .expect("quote converts")
            .expect("both sides produce a quote");
        assert!(matches!(
            quote,
            MarketEvent::Quote(TopOfBookQuote {
                bid_price: 510_025,
                bid_quantity: 200,
                ask_price: 510_050,
                ask_quantity: 400,
                ..
            })
        ));
        assert_eq!(
            canonical.convert(
                DecodedMarketMessage::Quote(QuoteUpdate {
                    identity: identity(),
                    bid: QuoteSideUpdate::Unchanged,
                    ask: QuoteSideUpdate::Cleared,
                    is_snapshot: false,
                    timestamp: timestamp(),
                }),
                3,
                3,
            ),
            Ok(None)
        );
    }

    #[test]
    fn depth_conversion_publishes_only_complete_images_and_recovers_on_clear() {
        let config = config();
        let mut canonical =
            CanonicalSessionState::try_new(&config, generation(5)).expect("state initializes");
        let bid = OrderBookLevel {
            price: 5_100.25,
            size: 2,
            orders: Some(1),
            implied_size: None,
        };
        let ask = OrderBookLevel {
            price: 5_100.50,
            size: 3,
            orders: Some(2),
            implied_size: None,
        };
        assert_eq!(
            canonical.convert(
                DecodedMarketMessage::OrderBook(OrderBookUpdate {
                    identity: identity(),
                    kind: OrderBookUpdateKind::Begin,
                    present_sides: OrderBookSides {
                        bids: true,
                        asks: false,
                    },
                    bids: vec![bid],
                    asks: Vec::new(),
                    timestamp: None,
                }),
                1,
                1,
            ),
            Ok(None)
        );
        let depth = canonical
            .convert(
                DecodedMarketMessage::OrderBook(OrderBookUpdate {
                    identity: identity(),
                    kind: OrderBookUpdateKind::End,
                    present_sides: OrderBookSides {
                        bids: false,
                        asks: true,
                    },
                    bids: Vec::new(),
                    asks: vec![ask],
                    timestamp: None,
                }),
                2,
                2,
            )
            .expect("depth image converts")
            .expect("complete image produces a snapshot");
        let MarketEvent::DepthSnapshot(depth) = depth else {
            panic!("complete aggregate image must produce a depth snapshot");
        };
        assert_eq!(depth.metadata.source_sequence, 2);
        assert_eq!(depth.metadata.timestamps.exchange_unix_nanos, None);
        assert_eq!(depth.bids[0].price, 510_025);
        assert_eq!(depth.bids[0].quantity, 200);
        assert_eq!(depth.asks[0].price, 510_050);
        assert_eq!(depth.asks[0].quantity, 300);

        assert_eq!(
            canonical.convert(
                DecodedMarketMessage::OrderBook(OrderBookUpdate {
                    identity: identity(),
                    kind: OrderBookUpdateKind::Clear,
                    present_sides: OrderBookSides {
                        bids: false,
                        asks: false,
                    },
                    bids: Vec::new(),
                    asks: Vec::new(),
                    timestamp: Some(timestamp()),
                }),
                3,
                3,
            ),
            Err((
                ProviderInvalidationReason::SequenceGap,
                RetryDisposition::Transient,
            ))
        );
    }

    #[test]
    fn driver_fences_stop_to_the_exact_active_generation() {
        let task: Arc<SessionTask> = Arc::new(|_, _, _, stop, _, _, _| {
            while !stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
        });
        let (mut driver, _events) =
            RithmicProviderDriver::with_task(config(), callback_limits(4, 4_096), task);
        assert_eq!(
            driver.start_session(generation(1), &[]),
            Err(RithmicProviderDriverError::InvalidCredentials)
        );
        let credentials = credentials();
        driver
            .start_session(generation(1), credentials.as_bytes())
            .expect("session starts");
        assert_eq!(
            driver.stop_session(generation(2)),
            Err(RithmicProviderDriverError::StaleGeneration)
        );
        driver
            .stop_session(generation(1))
            .expect("owning generation stops");
    }

    #[test]
    fn driver_stop_aborts_the_active_socket_before_joining() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local abort fixture");
        let client = TcpStream::connect(listener.local_addr().expect("read fixture address"))
            .expect("connect local abort fixture");
        let (server, _) = listener.accept().expect("accept local abort fixture");
        let server = Arc::new(Mutex::new(Some(server)));
        let (ready_tx, ready_rx) = sync_channel(1);
        let task_server = Arc::clone(&server);
        let task: Arc<SessionTask> = Arc::new(move |_, _, _, _, abort, _, _| {
            let stream = Arc::new(
                task_server
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .expect("fixture stream is used once"),
            );
            abort
                .register(Arc::clone(&stream))
                .expect("register active fixture socket");
            ready_tx.send(()).expect("publish fixture readiness");
            let mut byte = [0_u8; 1];
            let _ = (&*stream).read(&mut byte);
        });
        let (mut driver, _events) =
            RithmicProviderDriver::with_task(config(), callback_limits(4, 4_096), task);
        let credentials = credentials();
        driver
            .start_session(generation(8), credentials.as_bytes())
            .expect("session starts");
        ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fixture reaches blocking read");

        let (stopped_tx, stopped_rx) = sync_channel(1);
        let stopper = thread::spawn(move || {
            let result = driver.stop_session(generation(8));
            let _ = stopped_tx.send(result);
        });
        let stop_result = stopped_rx.recv_timeout(Duration::from_secs(1));
        drop(client);
        stopper.join().expect("stopper thread joins");
        assert_eq!(stop_result, Ok(Ok(())));
    }

    fn search(search_generation: usize) -> RithmicSymbolSearch {
        RithmicSymbolSearch::try_new(
            NonZeroUsize::new(search_generation).expect("search generation is nonzero"),
            "ES",
            Some("CME".to_string()),
            None,
            Some(InstrumentType::Future),
            SearchPattern::Contains,
            nonzero(16),
        )
        .expect("search validates")
    }

    #[test]
    fn catalog_commands_validate_bounds_and_redact_provider_text() {
        let search = search(1);
        let search_debug = format!("{search:?}");
        assert!(search_debug.contains("[REDACTED]"));
        assert!(!search_debug.contains("ES"));
        assert!(!search_debug.contains("CME"));
        assert_eq!(
            RithmicSymbolSearch::try_new(
                NonZeroUsize::MIN,
                "x".repeat(MAXIMUM_IDENTITY_BYTES + 1),
                None,
                None,
                None,
                SearchPattern::Equals,
                nonzero(1),
            ),
            Err(RithmicProviderCommandError::InvalidRequest)
        );
        assert_eq!(
            RithmicSymbolSearch::try_new(
                NonZeroUsize::MIN,
                "ES",
                None,
                None,
                None,
                SearchPattern::Equals,
                nonzero(MAXIMUM_INSTRUMENTS + 1),
            ),
            Err(RithmicProviderCommandError::InvalidRequest)
        );

        let selection = RithmicInstrumentSelection::try_new(
            NonZeroUsize::MIN,
            NonZeroUsize::MIN,
            "ESM7",
            "CME",
            "private-entitlement",
            RithmicReadOnlySubscription::try_new(true, true, true)
                .expect("read-only selection validates"),
        )
        .expect("selection validates");
        let selection_debug = format!("{selection:?}");
        assert!(selection_debug.contains("[REDACTED]"));
        assert!(!selection_debug.contains("ESM7"));
        assert!(!selection_debug.contains("private-entitlement"));
        assert_eq!(market_data_exchange("CME-Delayed"), "CME");
        assert_eq!(market_data_exchange("CME"), "CME");
        assert_eq!(
            RithmicInstrumentSelection::try_new(
                NonZeroUsize::MIN,
                NonZeroUsize::MIN,
                "ESM7",
                "CME",
                "entitlement",
                RithmicReadOnlySubscription {
                    trades: false,
                    quotes: false,
                    order_book: false,
                },
            ),
            Err(RithmicProviderCommandError::InvalidRequest)
        );
    }

    #[test]
    fn active_session_command_queue_is_bounded_generation_fenced_and_closed_on_stop() {
        let task_started = Arc::new(Barrier::new(2));
        let session_started = Arc::clone(&task_started);
        let task: Arc<SessionTask> = Arc::new(move |_, _, _, stop, _, _, _| {
            session_started.wait();
            while !stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
        });
        let (mut driver, events) =
            RithmicProviderDriver::with_task(config(), callback_limits(16, 64 * 1_024), task);
        let credentials = credentials();
        driver
            .start_session(generation(11), credentials.as_bytes())
            .expect("session starts");
        task_started.wait();

        assert_eq!(
            events.search_symbols(generation(10), search(1)),
            Err(RithmicProviderCommandError::StaleGeneration)
        );
        for command_generation in 1..=SESSION_COMMAND_CAPACITY {
            events
                .search_symbols(generation(11), search(command_generation))
                .expect("bounded command queues");
        }
        assert_eq!(
            events.search_symbols(generation(11), search(SESSION_COMMAND_CAPACITY + 1)),
            Err(RithmicProviderCommandError::QueueFull)
        );

        driver
            .stop_session(generation(11))
            .expect("active command path stops");
        assert_eq!(
            events.search_symbols(generation(11), search(12)),
            Err(RithmicProviderCommandError::SessionUnavailable)
        );
    }

    #[test]
    fn overlapping_provider_search_is_rejected_without_invalidating_the_session() {
        assert_eq!(
            search_command_failure(RithmicSessionError::RequestInFlight),
            Ok(RithmicCatalogRejection::SearchRejected)
        );
        assert_eq!(
            search_command_failure(RithmicSessionError::Transport),
            Err((
                ProviderInvalidationReason::Transport,
                RetryDisposition::Transient,
            ))
        );
    }

    #[test]
    fn catalog_callbacks_are_bounded_generation_fenced_and_redacted() {
        let producer_done = Arc::new(Barrier::new(2));
        let task_done = Arc::clone(&producer_done);
        let send_results = Arc::new(Mutex::new(None));
        let task_results = Arc::clone(&send_results);
        let task: Arc<SessionTask> = Arc::new(move |_, generation, _, _, _, _, emitter| {
            let symbols = CollectedSymbols {
                results: vec![SymbolSearchResult {
                    symbol: "ESM7".to_string(),
                    exchange: "CME".to_string(),
                    name: Some("E-mini S&P 500".to_string()),
                    product_code: Some("ES".to_string()),
                    instrument_type: Some("FUTURE".to_string()),
                    expiration_date: Some("202706".to_string()),
                }],
                duplicate_count: 0,
            };
            let first = emitter.send_catalog(RithmicCatalogEvent::SearchCompleted {
                session_generation: generation,
                search_generation: NonZeroUsize::MIN,
                symbols: symbols.clone(),
            });
            let second = emitter.send_catalog(RithmicCatalogEvent::SearchCompleted {
                session_generation: generation,
                search_generation: nonzero(2),
                symbols,
            });
            *task_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((first, second));
            task_done.wait();
        });
        let (mut driver, events) =
            RithmicProviderDriver::with_task(config(), callback_limits(1, 64 * 1_024), task);
        let credentials = credentials();
        driver
            .start_session(generation(12), credentials.as_bytes())
            .expect("session starts");
        producer_done.wait();
        assert_eq!(
            *send_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some((true, false))
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        let catalog = loop {
            if let Some(catalog) = events.try_recv_catalog() {
                break catalog;
            }
            assert!(Instant::now() < deadline, "catalog callback timed out");
            thread::yield_now();
        };
        assert_eq!(catalog.generation, generation(12));
        let debug = format!("{catalog:?}");
        assert!(debug.contains("result_count"));
        assert!(!debug.contains("ESM7"));
        assert!(!debug.contains("E-mini"));
        assert_eq!(
            wait_event(&events).event,
            ProviderSessionEvent::Invalidated {
                generation: Some(generation(12)),
                reason: ProviderInvalidationReason::QueueOverflow,
            }
        );
        driver
            .stop_session(generation(12))
            .expect("finished catalog producer joins");
    }

    #[test]
    fn panicked_session_task_latches_terminal_invalidation() {
        let task: Arc<SessionTask> = Arc::new(|_, _, _, _, _, _, _| {
            panic!("controlled session task panic");
        });
        let (mut driver, events) =
            RithmicProviderDriver::with_task(config(), callback_limits(2, 4_096), task);
        let credentials = credentials();
        driver
            .start_session(generation(3), credentials.as_bytes())
            .expect("session starts");

        let terminal = wait_event(&events);
        assert_eq!(terminal.generation, generation(3));
        assert_eq!(
            terminal.event,
            ProviderSessionEvent::Invalidated {
                generation: Some(generation(3)),
                reason: ProviderInvalidationReason::MalformedMessage,
            }
        );
        assert_eq!(terminal.retry, Some(RetryDisposition::Terminal));
        driver
            .stop_session(generation(3))
            .expect("panicked task is joined after invalidation");
    }

    #[test]
    fn callback_queue_latches_item_overflow_after_accepted_work() {
        let producer_done = Arc::new(Barrier::new(2));
        let task_done = Arc::clone(&producer_done);
        let send_results = Arc::new(Mutex::new(None));
        let task_results = Arc::clone(&send_results);
        let task: Arc<SessionTask> = Arc::new(move |_, _, _, _, _, _, emitter| {
            let first = emitter.send(ProviderSessionEvent::DiscoveryStarted);
            let second = emitter.send(ProviderSessionEvent::DiscoveryStarted);
            *task_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((first, second));
            task_done.wait();
        });
        let (mut driver, events) =
            RithmicProviderDriver::with_task(config(), callback_limits(1, 4_096), task);
        let credentials = credentials();
        driver
            .start_session(generation(4), credentials.as_bytes())
            .expect("session starts");
        producer_done.wait();
        assert_eq!(
            *send_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some((true, false))
        );

        let accepted = wait_event(&events);
        assert_eq!(accepted.generation, generation(4));
        assert_eq!(accepted.event, ProviderSessionEvent::DiscoveryStarted);
        assert_eq!(accepted.retry, None);
        let terminal = wait_event(&events);
        assert_eq!(terminal.generation, generation(4));
        assert_eq!(
            terminal.event,
            ProviderSessionEvent::Invalidated {
                generation: Some(generation(4)),
                reason: ProviderInvalidationReason::QueueOverflow,
            }
        );
        assert_eq!(terminal.retry, Some(RetryDisposition::Transient));
        assert_eq!(events.try_recv(), None);
        driver
            .stop_session(generation(4))
            .expect("finished session is joined");
    }

    #[test]
    fn callback_queue_fails_closed_on_byte_or_generation_bounds() {
        let byte_task: Arc<SessionTask> = Arc::new(|_, _, _, _, _, _, emitter| {
            assert!(!emitter.send(ProviderSessionEvent::DiscoveryStarted));
        });
        let (mut byte_driver, byte_events) =
            RithmicProviderDriver::with_task(config(), callback_limits(2, 1), byte_task);
        let credentials = credentials();
        byte_driver
            .start_session(generation(6), credentials.as_bytes())
            .expect("session starts");
        assert_eq!(
            wait_event(&byte_events).event,
            ProviderSessionEvent::Invalidated {
                generation: Some(generation(6)),
                reason: ProviderInvalidationReason::QueueOverflow,
            }
        );
        byte_driver
            .stop_session(generation(6))
            .expect("finished session is joined");

        let generation_task: Arc<SessionTask> = Arc::new(|_, _, _, _, _, _, emitter| {
            assert!(!emitter.send(ProviderSessionEvent::AuthenticationChanged {
                generation: generation(8),
                state: AuthenticationState::Accepted,
            }));
        });
        let (mut generation_driver, generation_events) =
            RithmicProviderDriver::with_task(config(), callback_limits(2, 4_096), generation_task);
        generation_driver
            .start_session(generation(7), credentials.as_bytes())
            .expect("session starts");
        let terminal = wait_event(&generation_events);
        assert_eq!(terminal.generation, generation(7));
        assert_eq!(
            terminal.event,
            ProviderSessionEvent::Invalidated {
                generation: Some(generation(7)),
                reason: ProviderInvalidationReason::MalformedMessage,
            }
        );
        assert_eq!(terminal.retry, Some(RetryDisposition::Terminal));
        generation_driver
            .stop_session(generation(7))
            .expect("finished session is joined");
    }

    #[test]
    fn retry_schedule_is_capped_resettable_and_exact_generation_fenced() {
        let mut retries = RithmicRetryScheduler::default();
        let now = Instant::now();
        for expected in RETRY_DELAYS {
            let ticket = retries
                .record_invalid(generation(9), RetryDisposition::Transient, now)
                .expect("transient failure schedules retry");
            assert_eq!(ticket.due_at.duration_since(now), expected);
        }
        let capped = retries
            .record_invalid(generation(9), RetryDisposition::Transient, now)
            .expect("capped retry remains scheduled");
        assert_eq!(capped.due_at.duration_since(now), Duration::from_secs(8));
        assert_eq!(
            retries.record_invalid(generation(9), RetryDisposition::Terminal, now),
            None
        );
        assert_eq!(retries.ticket(), None);

        retries.established();
        let reset = retries
            .record_invalid(generation(9), RetryDisposition::Transient, now)
            .expect("established session resets the delay");
        assert_eq!(reset.due_at.duration_since(now), Duration::from_millis(250));
        assert!(!retries.take_due(
            DesktopProviderState::RecoveryRequired {
                generation: Some(generation(10)),
                reason: RecoveryReason::TransportInvalid,
            },
            reset.due_at,
        ));
        assert_eq!(retries.ticket(), None);

        retries.established();
        let exact = retries
            .record_invalid(generation(9), RetryDisposition::Transient, now)
            .expect("retry schedules");
        assert!(retries.take_due(
            DesktopProviderState::RecoveryRequired {
                generation: Some(generation(9)),
                reason: RecoveryReason::SemanticQueueOverflow,
            },
            exact.due_at,
        ));
        assert_eq!(retries.ticket(), None);
    }

    #[test]
    fn vault_callbacks_drive_shared_runtime_recovery_and_exact_retry() {
        let (driver, events) = RithmicProviderDriver::with_task(
            config(),
            callback_limits(8, 64 * 1_024),
            runtime_task(),
        );
        let (mut worker, root) = open_worker(driver);
        let mut retries = RithmicRetryScheduler::default();
        let first = worker
            .connect(ConnectTrigger::Initial)
            .expect("vault-backed session starts");
        assert_eq!(first, generation(1));
        let now = Instant::now();

        assert!(matches!(
            wait_applied(&mut worker, &events, &mut retries, now),
            AppliedRithmicEvent::Semantic(ProviderSessionEvent::DiscoveryStarted)
        ));
        assert!(matches!(
            wait_applied(&mut worker, &events, &mut retries, now),
            AppliedRithmicEvent::Semantic(ProviderSessionEvent::SystemsDiscovered { .. })
        ));
        assert!(matches!(
            wait_applied(&mut worker, &events, &mut retries, now),
            AppliedRithmicEvent::Semantic(ProviderSessionEvent::AuthenticationChanged {
                generation,
                state: AuthenticationState::Accepted,
            }) if generation == first
        ));
        assert!(matches!(
            wait_applied(&mut worker, &events, &mut retries, now),
            AppliedRithmicEvent::Semantic(ProviderSessionEvent::InstrumentsDiscovered {
                generation,
                ..
            }) if generation == first
        ));
        assert_eq!(
            worker.provider_state().expect("state reads"),
            DesktopProviderState::Streaming { generation: first }
        );

        let AppliedRithmicEvent::RetryScheduled(ticket) =
            wait_applied(&mut worker, &events, &mut retries, now)
        else {
            panic!("transient invalidation must schedule a retry");
        };
        assert_eq!(ticket.failed_generation, first);
        assert_eq!(
            ticket.due_at.duration_since(now),
            Duration::from_millis(250)
        );
        assert_eq!(
            worker.provider_state().expect("state reads"),
            DesktopProviderState::RecoveryRequired {
                generation: Some(first),
                reason: RecoveryReason::TransportInvalid,
            }
        );
        assert_eq!(
            retries
                .retry_due(&mut worker, now)
                .expect("early retry is ignored"),
            None
        );
        let second = retries
            .retry_due(&mut worker, ticket.due_at)
            .expect("due retry starts")
            .expect("exact recovery ticket is consumed");
        assert_eq!(second, generation(2));

        let (foreign_driver, foreign_events) =
            RithmicProviderDriver::with_task(config(), callback_limits(2, 4_096), runtime_task());
        assert!(matches!(
            try_recv_rithmic_event(&mut worker, &foreign_events, &mut retries, ticket.due_at,),
            Err(RithmicDesktopEventError::Runtime(
                DesktopMarketWorkerError::CallbackSourceMismatch
            ))
        ));
        drop(foreign_driver);

        for _ in 0..5 {
            let _ = wait_applied(&mut worker, &events, &mut retries, ticket.due_at);
        }
        assert_eq!(
            worker.provider_state().expect("state reads"),
            DesktopProviderState::RecoveryRequired {
                generation: Some(second),
                reason: RecoveryReason::TransportInvalid,
            }
        );
        drop(worker);
        fs::remove_dir_all(root).expect("history fixture removes");
    }

    #[test]
    fn native_environment_fences_callbacks_retries_and_reconnects_fresh() {
        let (driver, events) = RithmicProviderDriver::with_task(
            config(),
            callback_limits(16, 64 * 1_024),
            stable_runtime_task(),
        );
        let (mut worker, root) = open_worker(driver);
        let mut retries = RithmicRetryScheduler::default();
        let first = worker
            .request_connection()
            .expect("connection intent starts")
            .expect("initial generation starts");
        let now = Instant::now();
        for _ in 0..4 {
            let _ = wait_applied(&mut worker, &events, &mut retries, now);
        }
        assert_eq!(
            worker.provider_state().expect("state reads"),
            DesktopProviderState::Streaming { generation: first }
        );
        retries.record_invalid(first, RetryDisposition::Transient, now);

        assert_eq!(
            apply_rithmic_environment_event(
                &mut worker,
                &events,
                &mut retries,
                RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable),
            )
            .expect("network loss fences the session"),
            None
        );
        assert_eq!(retries.ticket(), None);
        assert_eq!(events.try_recv(), None);
        assert_eq!(events.try_recv_catalog(), None);
        assert_eq!(
            worker.provider_state().expect("state reads"),
            DesktopProviderState::NetworkUnavailable
        );

        let second = apply_rithmic_environment_event(
            &mut worker,
            &events,
            &mut retries,
            RithmicEnvironmentEvent::Network(NetworkEvent::Available),
        )
        .expect("network restoration is applied")
        .expect("network restoration starts a fresh generation");
        assert!(second > first);
        for _ in 0..4 {
            let _ = wait_applied(&mut worker, &events, &mut retries, now);
        }

        assert_eq!(
            apply_rithmic_environment_event(
                &mut worker,
                &events,
                &mut retries,
                RithmicEnvironmentEvent::Power(PowerEvent::Suspending),
            )
            .expect("suspend fences the replacement session"),
            None
        );
        let third = apply_rithmic_environment_event(
            &mut worker,
            &events,
            &mut retries,
            RithmicEnvironmentEvent::Power(PowerEvent::Resumed),
        )
        .expect("resume is applied")
        .expect("resume starts a fresh generation");
        assert!(third > second);

        drop(worker);
        fs::remove_dir_all(root).expect("history fixture removes");
    }

    #[test]
    fn offline_startup_waits_for_native_restoration_before_loading_vault() {
        let (driver, events) = RithmicProviderDriver::with_task(
            config(),
            callback_limits(16, 64 * 1_024),
            stable_runtime_task(),
        );
        let (mut worker, root) = open_worker(driver);
        let mut retries = RithmicRetryScheduler::default();
        apply_rithmic_environment_event(
            &mut worker,
            &events,
            &mut retries,
            RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable),
        )
        .expect("initial offline state is applied");
        assert_eq!(
            worker
                .request_connection()
                .expect("offline connection intent is retained"),
            None
        );
        assert_eq!(
            worker.provider_state().expect("state reads"),
            DesktopProviderState::NetworkUnavailable
        );
        let restored_generation = apply_rithmic_environment_event(
            &mut worker,
            &events,
            &mut retries,
            RithmicEnvironmentEvent::Network(NetworkEvent::Available),
        )
        .expect("network restoration is applied")
        .expect("restoration starts the requested session");
        assert_eq!(restored_generation, generation(1));

        drop(worker);
        fs::remove_dir_all(root).expect("history fixture removes");
    }

    #[test]
    fn session_failures_distinguish_terminal_protocol_from_transient_transport() {
        assert_eq!(
            session_failure(RithmicSessionError::LoginRejected),
            (
                ProviderInvalidationReason::Authentication,
                RetryDisposition::Terminal,
            )
        );
        assert_eq!(
            session_failure(RithmicSessionError::SchemaMismatch),
            (
                ProviderInvalidationReason::SchemaMismatch,
                RetryDisposition::Terminal,
            )
        );
        assert_eq!(
            session_failure(RithmicSessionError::Deadline),
            (
                ProviderInvalidationReason::Transport,
                RetryDisposition::Transient,
            )
        );
    }
}
