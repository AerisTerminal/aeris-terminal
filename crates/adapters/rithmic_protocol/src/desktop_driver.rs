use crate::{
    AggregateBookAssembler, AggregateBookLimits, AggregateBookOutcome, DecodedControlMessage,
    DecodedMarketMessage, MarketDataSubscription, MarketIdentity, OrderBookLevel,
    ProviderTimestamp, QuoteLevel, QuoteSideUpdate, RetryDisposition, RithmicApplication,
    RithmicCredentialBytes, RithmicSessionError, RithmicSessionLimits, RithmicSessionMessage,
    RithmicTestSession, SubscriptionAction, TradeAggressor,
};
use axiusflow_desktop_provider_runtime::{
    AuthenticationState, ConnectTrigger, DesktopMarketWorker, DesktopMarketWorkerError,
    DesktopProviderState, InstrumentDescriptor, ProviderEnvironment, ProviderInvalidationReason,
    ProviderSessionDriver, ProviderSessionEvent, RecoveryReason, SessionGeneration,
};
use axiusflow_market_data::{
    AggressorSide, DepthLevel, DepthSnapshot, EventMetadata, MarketEvent, MarketTrade,
    QualifiedTimestamp, TopOfBookQuote,
};
use axiusflow_platform_runtime::CredentialVault;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    mem::size_of,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
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
        })
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

impl RithmicProviderEvents {
    #[must_use]
    pub fn has_ready(&self) -> bool {
        let state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.queue.is_empty() || state.terminal.is_some()
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
        SessionEmitter,
    ) + Send
    + Sync
    + 'static;

struct ActiveSession {
    generation: SessionGeneration,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
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

struct SharedCallbacks {
    limits: RithmicCallbackLimits,
    maximum_instruments: NonZeroUsize,
    state: Mutex<CallbackState>,
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
            let _ = active.handle.join();
        }
        Ok(())
    }

    fn owns_events(&self, events: &RithmicProviderEvents) -> bool {
        Arc::ptr_eq(&self.callbacks, &events.callbacks)
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
        let stop = Arc::new(AtomicBool::new(false));
        let task = Arc::clone(&self.task);
        let config = self.config.clone();
        let emitter = SessionEmitter {
            generation,
            callbacks: Arc::clone(&self.callbacks),
        };
        let panic_emitter = emitter.clone();
        let task_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name(format!("rithmic-session-{}", generation.get()))
            .spawn(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    task(config, generation, credentials, task_stop, emitter);
                }))
                .is_err()
                {
                    panic_emitter.invalid(
                        ProviderInvalidationReason::MalformedMessage,
                        RetryDisposition::Terminal,
                    );
                }
            })
            .map_err(|_| RithmicProviderDriverError::ThreadUnavailable)?;
        self.active = Some(ActiveSession {
            generation,
            stop,
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
        let _ = active.handle.join();
        Ok(())
    }
}

impl Drop for RithmicProviderDriver {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.stop.store(true, Ordering::Release);
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
    Arc::new(|config, generation, credential_bytes, stop, emitter| {
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
            RithmicTestSession::discover_and_login(
                credentials,
                RithmicApplication {
                    name: &config.application_name,
                    version: &config.application_version,
                },
                config.session_limits,
                Some(Arc::clone(&stop)),
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
            collect_market(&mut connection, &config, generation, &stop, &emitter)
        };
        if stop.load(Ordering::Acquire) {
            let _ = connection.close();
            return;
        }
        if let Err((reason, retry)) = result {
            emitter.invalid(reason, retry);
        }
    })
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

fn collect_market(
    connection: &mut crate::RithmicTickerConnection,
    config: &RithmicProviderConfig,
    generation: SessionGeneration,
    stop: &AtomicBool,
    emitter: &SessionEmitter,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let mut canonical = CanonicalSessionState::try_new(config, generation)?;
    let heartbeat_interval = connection.heartbeat_interval();
    let started = Instant::now();
    let mut last_message = started;
    let mut next_heartbeat = started + heartbeat_interval;
    let mut heartbeat_deadline = None;
    let mut source_ordinal = 0_u64;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let now = Instant::now();
        if now.duration_since(last_message) >= config.message_silence_timeout {
            return Err((
                ProviderInvalidationReason::MessageSilence,
                RetryDisposition::Transient,
            ));
        }
        if heartbeat_deadline.is_some_and(|deadline| now >= deadline) {
            return Err((
                ProviderInvalidationReason::HeartbeatSilence,
                RetryDisposition::Transient,
            ));
        }
        if heartbeat_deadline.is_none() && now >= next_heartbeat {
            connection.send_heartbeat().map_err(session_failure)?;
            heartbeat_deadline = Some(now + config.session_limits.response_timeout);
            next_heartbeat = now + heartbeat_interval;
        }
        let mut deadline = last_message + config.message_silence_timeout;
        if let Some(pending) = heartbeat_deadline {
            deadline = deadline.min(pending);
        } else {
            deadline = deadline.min(next_heartbeat);
        }
        let message = match connection.read_next_until(deadline) {
            Ok(message) => message,
            Err(RithmicSessionError::Deadline) => continue,
            Err(RithmicSessionError::Cancelled) if stop.load(Ordering::Acquire) => return Ok(()),
            Err(error) => return Err(session_failure(error)),
        };
        last_message = Instant::now();
        source_ordinal = source_ordinal.checked_add(1).ok_or((
            ProviderInvalidationReason::MalformedMessage,
            RetryDisposition::Terminal,
        ))?;
        match message {
            RithmicSessionMessage::Market(message) => {
                let received_unix_nanos = unix_nanos_now()?;
                if let Some(event) =
                    canonical.convert(message, source_ordinal, received_unix_nanos)?
                    && !emitter.send(ProviderSessionEvent::Market { generation, event })
                {
                    stop.store(true, Ordering::Release);
                    return Ok(());
                }
            }
            RithmicSessionMessage::Control(DecodedControlMessage::Heartbeat {
                accepted: true,
                ..
            }) => {
                heartbeat_deadline = None;
                if !emitter.send(ProviderSessionEvent::Heartbeat {
                    generation,
                    received_unix_nanos: unix_nanos_now()?,
                }) {
                    stop.store(true, Ordering::Release);
                    return Ok(());
                }
            }
            RithmicSessionMessage::Control(DecodedControlMessage::ForcedLogout) => {
                return Err((
                    ProviderInvalidationReason::Authentication,
                    RetryDisposition::Terminal,
                ));
            }
            _ => {
                return Err((
                    ProviderInvalidationReason::MalformedMessage,
                    RetryDisposition::Terminal,
                ));
            }
        }
    }
}

struct CanonicalSessionState<'a> {
    config: &'a RithmicProviderConfig,
    generation: SessionGeneration,
    quotes: BTreeMap<String, QuoteState>,
    books: BTreeMap<String, AggregateBookAssembler>,
}

#[derive(Clone, Copy, Default)]
struct QuoteState {
    bid: Option<QuoteLevel>,
    ask: Option<QuoteLevel>,
}

impl<'a> CanonicalSessionState<'a> {
    fn try_new(
        config: &'a RithmicProviderConfig,
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
            config,
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
                        trade.timestamp,
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
                        quote.timestamp,
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
        self.config
            .instruments
            .iter()
            .find(|instrument| instrument.identity_matches(identity))
            .ok_or_else(malformed)
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
    timestamp: ProviderTimestamp,
    received_unix_nanos: i64,
) -> Result<EventMetadata, (ProviderInvalidationReason, RetryDisposition)> {
    Ok(EventMetadata {
        provider_id: PROVIDER_ID.to_string(),
        instrument_id: instrument.descriptor.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        source_sequence,
        session_generation: generation.get(),
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: Some(provider_timestamp_nanos(timestamp)?),
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
    use std::{fs, num::NonZeroU64, path::PathBuf, sync::Barrier, thread};

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
        Arc::new(|config, generation, _, _, emitter| {
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
                    timestamp: timestamp(),
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
                    timestamp: timestamp(),
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
                    timestamp: timestamp(),
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
        let task: Arc<SessionTask> = Arc::new(|_, _, _, stop, _| {
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
    fn panicked_session_task_latches_terminal_invalidation() {
        let task: Arc<SessionTask> = Arc::new(|_, _, _, _, _| {
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
        let task: Arc<SessionTask> = Arc::new(move |_, _, _, _, emitter| {
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
        let byte_task: Arc<SessionTask> = Arc::new(|_, _, _, _, emitter| {
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

        let generation_task: Arc<SessionTask> = Arc::new(|_, _, _, _, emitter| {
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
