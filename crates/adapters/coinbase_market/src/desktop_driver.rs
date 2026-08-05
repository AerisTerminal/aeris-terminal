use crate::{CanonicalTrade, CoinbaseConfig, CoinbaseSession, SessionOutcome};
use axiusflow_desktop_provider_runtime::{
    DesktopMarketWorker, DesktopMarketWorkerError, ProviderCredentialRequirement,
    ProviderSessionDriver, SessionGeneration,
};
use axiusflow_platform_runtime::CredentialVault;
use core::fmt;
use std::{
    collections::VecDeque,
    error::Error,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

/// Coarse reason that a direct Coinbase generation became unusable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoinbaseProviderInvalidReason {
    Transport,
    SequenceGap,
    ClosedByPeer,
    InactivityTimeout,
    EventQueueOverflow,
}

/// Bounded direct-provider callback delivered to the owning desktop worker.
#[derive(Clone, Eq, PartialEq)]
pub enum CoinbaseProviderEvent {
    Established {
        generation: SessionGeneration,
    },
    Trade {
        generation: SessionGeneration,
        trade: CanonicalTrade,
    },
    Heartbeat {
        generation: SessionGeneration,
    },
    Invalid {
        generation: SessionGeneration,
        reason: CoinbaseProviderInvalidReason,
    },
}

impl fmt::Debug for CoinbaseProviderEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Established { generation } => formatter
                .debug_struct("Established")
                .field("generation", generation)
                .finish(),
            Self::Trade { generation, .. } => formatter
                .debug_struct("Trade")
                .field("generation", generation)
                .field("trade", &"[REDACTED]")
                .finish(),
            Self::Heartbeat { generation } => formatter
                .debug_struct("Heartbeat")
                .field("generation", generation)
                .finish(),
            Self::Invalid { generation, reason } => formatter
                .debug_struct("Invalid")
                .field("generation", generation)
                .field("reason", reason)
                .finish(),
        }
    }
}

/// Receiving half of the bounded direct Coinbase callback channel.
pub struct CoinbaseProviderEvents {
    callbacks: Arc<SharedCallbacks>,
}

impl CoinbaseProviderEvents {
    /// Reports whether at least one callback is ready without consuming it.
    #[must_use]
    pub fn has_ready(&self) -> bool {
        let state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.queue.is_empty() || state.terminal.is_some()
    }

    /// Receives at most one ready callback without blocking.
    #[must_use]
    pub fn try_recv(&self) -> Option<CoinbaseProviderEvent> {
        let mut state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.queue.pop_front().or_else(|| state.terminal.take())
    }
}

/// Redacted lifecycle failures from the direct Coinbase driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoinbaseProviderDriverError {
    SessionAlreadyActive,
    UnexpectedCredentials,
    StaleGeneration,
    ThreadUnavailable,
    StopUnconfirmed,
}

impl fmt::Display for CoinbaseProviderDriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Coinbase desktop driver failed: {self:?}")
    }
}

impl Error for CoinbaseProviderDriverError {}

type SessionTask = dyn Fn(CoinbaseConfig, SessionGeneration, Arc<AtomicBool>, SessionEmitter)
    + Send
    + Sync
    + 'static;

struct ActiveSession {
    generation: SessionGeneration,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct CallbackState {
    queue: VecDeque<CoinbaseProviderEvent>,
    terminal: Option<CoinbaseProviderEvent>,
    failed: bool,
}

struct SharedCallbacks {
    capacity: NonZeroUsize,
    state: Mutex<CallbackState>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone)]
struct SessionEmitter {
    generation: SessionGeneration,
    callbacks: Arc<SharedCallbacks>,
}

impl SessionEmitter {
    fn established(&self) -> bool {
        self.send(CoinbaseProviderEvent::Established {
            generation: self.generation,
        })
    }

    fn trade(&self, trade: CanonicalTrade) -> bool {
        self.send(CoinbaseProviderEvent::Trade {
            generation: self.generation,
            trade,
        })
    }

    fn heartbeat(&self) -> bool {
        self.send(CoinbaseProviderEvent::Heartbeat {
            generation: self.generation,
        })
    }

    fn send(&self, event: CoinbaseProviderEvent) -> bool {
        let mut state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (accepted, notify) = if state.failed {
            (false, false)
        } else if state.queue.len() >= self.callbacks.capacity.get() {
            state.failed = true;
            state.terminal = Some(CoinbaseProviderEvent::Invalid {
                generation: self.generation,
                reason: CoinbaseProviderInvalidReason::EventQueueOverflow,
            });
            (false, true)
        } else {
            state.queue.push_back(event);
            (true, true)
        };
        drop(state);
        if notify {
            self.wake();
        }
        accepted
    }

    fn invalid(&self, reason: CoinbaseProviderInvalidReason) {
        let mut state = self
            .callbacks
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let notify = if state.failed {
            false
        } else {
            state.failed = true;
            state.terminal = Some(CoinbaseProviderEvent::Invalid {
                generation: self.generation,
                reason,
            });
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

/// Cancellable one-generation-at-a-time Coinbase session driver.
pub struct CoinbaseProviderDriver {
    config: CoinbaseConfig,
    callbacks: Arc<SharedCallbacks>,
    task: Arc<SessionTask>,
    active: Option<ActiveSession>,
}

impl CoinbaseProviderDriver {
    /// Creates a direct, keyless Coinbase driver and bounded callback receiver.
    #[must_use]
    pub fn new(
        config: CoinbaseConfig,
        event_capacity: NonZeroUsize,
    ) -> (Self, CoinbaseProviderEvents) {
        Self::with_task_and_wake(config, event_capacity, direct_session_task(), None)
    }

    /// Creates a direct driver that invokes a nonblocking worker wake callback
    /// whenever a provider callback becomes ready.
    #[must_use]
    pub fn new_with_wake(
        config: CoinbaseConfig,
        event_capacity: NonZeroUsize,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> (Self, CoinbaseProviderEvents) {
        Self::with_task_and_wake(config, event_capacity, direct_session_task(), Some(wake))
    }

    #[cfg(test)]
    fn with_task(
        config: CoinbaseConfig,
        event_capacity: NonZeroUsize,
        task: Arc<SessionTask>,
    ) -> (Self, CoinbaseProviderEvents) {
        Self::with_task_and_wake(config, event_capacity, task, None)
    }

    fn with_task_and_wake(
        config: CoinbaseConfig,
        event_capacity: NonZeroUsize,
        task: Arc<SessionTask>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> (Self, CoinbaseProviderEvents) {
        let callbacks = Arc::new(SharedCallbacks {
            capacity: event_capacity,
            state: Mutex::new(CallbackState {
                queue: VecDeque::with_capacity(event_capacity.get()),
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
            CoinbaseProviderEvents { callbacks },
        )
    }

    fn reap_finished(&mut self) -> Result<(), CoinbaseProviderDriverError> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.handle.is_finished())
        {
            let active = self
                .active
                .take()
                .ok_or(CoinbaseProviderDriverError::StopUnconfirmed)?;
            let _ = active.handle.join();
        }
        Ok(())
    }

    fn owns_events(&self, events: &CoinbaseProviderEvents) -> bool {
        Arc::ptr_eq(&self.callbacks, &events.callbacks)
    }
}

impl ProviderSessionDriver for CoinbaseProviderDriver {
    type Error = CoinbaseProviderDriverError;

    fn credential_requirement(&self) -> ProviderCredentialRequirement {
        ProviderCredentialRequirement::Public
    }

    fn start_session(
        &mut self,
        generation: SessionGeneration,
        credentials: &[u8],
    ) -> Result<(), Self::Error> {
        self.reap_finished()?;
        if self.active.is_some() {
            return Err(CoinbaseProviderDriverError::SessionAlreadyActive);
        }
        if !credentials.is_empty() {
            return Err(CoinbaseProviderDriverError::UnexpectedCredentials);
        }
        {
            let mut callbacks = self
                .callbacks
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            callbacks.queue.clear();
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
        let task_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name(format!("coinbase-session-{}", generation.get()))
            .spawn(move || task(config, generation, task_stop, emitter))
            .map_err(|_| CoinbaseProviderDriverError::ThreadUnavailable)?;
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
            .ok_or(CoinbaseProviderDriverError::StaleGeneration)?;
        if active.generation != generation {
            self.active = Some(active);
            return Err(CoinbaseProviderDriverError::StaleGeneration);
        }
        active.stop.store(true, Ordering::Release);
        let _ = active.handle.join();
        Ok(())
    }
}

impl Drop for CoinbaseProviderDriver {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.stop.store(true, Ordering::Release);
            let _ = active.handle.join();
        }
    }
}

/// Redacted failures while applying Coinbase callbacks to the shared desktop runtime.
#[derive(Debug)]
pub enum CoinbaseDesktopEventError {
    Runtime(DesktopMarketWorkerError),
    Aggregation,
    AggregatorLimit { maximum: usize },
    DuplicateAggregator,
    ProductNotRegistered,
    HistoryIdentityMismatch,
    HistoryUnavailable,
}

impl fmt::Display for CoinbaseDesktopEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "Coinbase callback failed: {error}"),
            Self::Aggregation => formatter.write_str("Coinbase trade aggregation failed"),
            Self::AggregatorLimit { maximum } => {
                write!(
                    formatter,
                    "Coinbase aggregation exceeds its {maximum}-product bound"
                )
            }
            Self::DuplicateAggregator => {
                formatter.write_str("Coinbase bar product is registered more than once")
            }
            Self::ProductNotRegistered => {
                formatter.write_str("Coinbase bar product is not registered")
            }
            Self::HistoryIdentityMismatch => {
                formatter.write_str("Coinbase bar history identity mismatched")
            }
            Self::HistoryUnavailable => formatter.write_str("Coinbase bar history is unavailable"),
        }
    }
}

impl Error for CoinbaseDesktopEventError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::Aggregation
            | Self::AggregatorLimit { .. }
            | Self::DuplicateAggregator
            | Self::ProductNotRegistered
            | Self::HistoryIdentityMismatch
            | Self::HistoryUnavailable => None,
        }
    }
}

impl From<DesktopMarketWorkerError> for CoinbaseDesktopEventError {
    fn from(error: DesktopMarketWorkerError) -> Self {
        Self::Runtime(error)
    }
}

/// Applies at most one adapter callback and returns a generation-fenced trade.
///
/// # Errors
///
/// Returns a redacted source or lifecycle failure.
pub fn try_recv_coinbase_trade<T: Clone, V: CredentialVault>(
    worker: &mut DesktopMarketWorker<T, V, CoinbaseProviderDriver>,
    events: &CoinbaseProviderEvents,
) -> Result<Option<(SessionGeneration, CanonicalTrade)>, CoinbaseDesktopEventError> {
    if !worker.callback_source_matches(|driver| driver.owns_events(events))? {
        return Err(DesktopMarketWorkerError::CallbackSourceMismatch.into());
    }
    let Some(event) = events.try_recv() else {
        return Ok(None);
    };
    match event {
        CoinbaseProviderEvent::Established { generation } => {
            worker.session_established(generation)?;
            Ok(None)
        }
        CoinbaseProviderEvent::Trade { generation, trade } => {
            worker.ensure_streaming_generation(generation)?;
            worker
                .record_trade_diagnostics(generation, Some(trade.provider_timestamp_unix_nanos))?;
            Ok(Some((generation, trade)))
        }
        CoinbaseProviderEvent::Heartbeat { generation } => {
            worker.ensure_streaming_generation(generation)?;
            worker.record_heartbeat_diagnostics(generation)?;
            Ok(None)
        }
        CoinbaseProviderEvent::Invalid { generation, reason } => {
            if reason == CoinbaseProviderInvalidReason::EventQueueOverflow {
                worker.session_callback_queue_overflow(generation)?;
            } else {
                worker.session_invalid(generation)?;
            }
            Ok(None)
        }
    }
}

/// Applies at most one callback and aggregates a completed provider bar.
///
/// # Errors
///
/// Returns a redacted source, lifecycle, or aggregation failure.
pub fn try_recv_coinbase_aggregated_bar<T: Clone, V: CredentialVault>(
    worker: &mut DesktopMarketWorker<T, V, CoinbaseProviderDriver>,
    events: &CoinbaseProviderEvents,
    aggregators: &mut [crate::CoinbaseBarAggregator],
) -> Result<Option<(SessionGeneration, crate::CoinbaseAggregatedBar)>, CoinbaseDesktopEventError> {
    if !worker.callback_source_matches(|driver| driver.owns_events(events))? {
        return Err(DesktopMarketWorkerError::CallbackSourceMismatch.into());
    }
    validate_aggregators(aggregators)?;
    let Some(event) = events.try_recv() else {
        return Ok(None);
    };
    match event {
        CoinbaseProviderEvent::Established { generation } => {
            worker.session_established(generation)?;
            reset_aggregators(aggregators);
            Ok(None)
        }
        CoinbaseProviderEvent::Trade { generation, trade } => {
            worker.ensure_streaming_generation(generation)?;
            worker
                .record_trade_diagnostics(generation, Some(trade.provider_timestamp_unix_nanos))?;
            let Some(aggregator) = aggregators
                .iter_mut()
                .find(|aggregator| aggregator.product_id() == trade.product_id)
            else {
                reset_aggregators(aggregators);
                worker.session_invalid(generation)?;
                return Err(CoinbaseDesktopEventError::ProductNotRegistered);
            };
            let Ok(completed) = aggregator.apply_trade_with_evidence(&trade) else {
                reset_aggregators(aggregators);
                worker.session_invalid(generation)?;
                return Err(CoinbaseDesktopEventError::Aggregation);
            };
            Ok(completed.map(|bar| (generation, bar)))
        }
        CoinbaseProviderEvent::Heartbeat { generation } => {
            worker.ensure_streaming_generation(generation)?;
            worker.record_heartbeat_diagnostics(generation)?;
            Ok(None)
        }
        CoinbaseProviderEvent::Invalid { generation, reason } => {
            reset_aggregators(aggregators);
            if reason == CoinbaseProviderInvalidReason::EventQueueOverflow {
                worker.session_callback_queue_overflow(generation)?;
            } else {
                worker.session_invalid(generation)?;
            }
            Ok(None)
        }
    }
}

/// Applies at most one callback and returns only the completed canonical bar.
///
/// # Errors
///
/// Returns a redacted source, lifecycle, or aggregation failure.
pub fn try_recv_coinbase_bar<T: Clone, V: CredentialVault>(
    worker: &mut DesktopMarketWorker<T, V, CoinbaseProviderDriver>,
    events: &CoinbaseProviderEvents,
    aggregators: &mut [crate::CoinbaseBarAggregator],
) -> Result<Option<(SessionGeneration, axiusflow_market_data::MarketBar)>, CoinbaseDesktopEventError>
{
    try_recv_coinbase_aggregated_bar(worker, events, aggregators)
        .map(|completed| completed.map(|(generation, completed)| (generation, completed.bar)))
}

fn reset_aggregators(aggregators: &mut [crate::CoinbaseBarAggregator]) {
    for aggregator in aggregators {
        aggregator.reset();
    }
}

fn validate_aggregators(
    aggregators: &[crate::CoinbaseBarAggregator],
) -> Result<(), CoinbaseDesktopEventError> {
    if aggregators.len() > crate::MAXIMUM_PRODUCTS {
        return Err(CoinbaseDesktopEventError::AggregatorLimit {
            maximum: crate::MAXIMUM_PRODUCTS,
        });
    }
    let mut products = std::collections::BTreeSet::new();
    if aggregators
        .iter()
        .any(|aggregator| !products.insert(aggregator.product_id()))
    {
        return Err(CoinbaseDesktopEventError::DuplicateAggregator);
    }
    Ok(())
}

/// Seeds adapter-owned aggregation from an authorized provider history publication.
///
/// # Errors
///
/// Returns a redacted lifecycle, identity, history, or aggregation failure.
pub fn seed_coinbase_bar_history<V: CredentialVault>(
    worker: &mut DesktopMarketWorker<axiusflow_market_data::MarketBar, V, CoinbaseProviderDriver>,
    aggregator: &mut crate::CoinbaseBarAggregator,
    generation: SessionGeneration,
    product_id: &str,
    identity: &axiusflow_desktop_storage::SegmentIdentity,
    encryption_key: &axiusflow_desktop_storage::SegmentEncryptionKey,
    now_unix_seconds: i64,
) -> Result<usize, CoinbaseDesktopEventError> {
    worker.ensure_history_callback(generation, identity)?;
    if product_id != aggregator.product_id()
        || identity.scope.provider_id != "coinbase"
        || identity.scope.account_id != crate::COINBASE_PUBLIC_ACCOUNT_ID
        || identity.scope.entitlement_revision != crate::ENTITLEMENT_CLASS
        || identity.instrument_id != aggregator.instrument_id()
        || identity.data_kind != axiusflow_desktop_storage::DataKind::Bars
        || identity.resolution != "1m"
        || aggregator.price_scale() != 2
        || aggregator.quantity_scale() != 8
    {
        return Err(CoinbaseDesktopEventError::HistoryIdentityMismatch);
    }
    let publication = worker
        .current_history_publication(identity, encryption_key, now_unix_seconds)?
        .ok_or(CoinbaseDesktopEventError::HistoryUnavailable)?;
    let latest_completed_minute = now_unix_seconds
        .div_euclid(60)
        .checked_mul(60)
        .and_then(|minute| minute.checked_sub(60))
        .ok_or(CoinbaseDesktopEventError::HistoryUnavailable)?;
    if publication
        .values
        .last()
        .is_none_or(|item| item.value.exchange_timestamp_seconds != latest_completed_minute)
    {
        return Err(CoinbaseDesktopEventError::HistoryUnavailable);
    }
    let bars = publication
        .values
        .iter()
        .map(|item| item.value)
        .collect::<Vec<_>>();
    aggregator
        .seed_completed_history(&bars)
        .map_err(|_| CoinbaseDesktopEventError::Aggregation)
}

fn direct_session_task() -> Arc<SessionTask> {
    Arc::new(|config, _generation, stop, emitter| {
        let session = CoinbaseSession::new(config);
        let Ok(connection) = session.connect_cancellable(Arc::clone(&stop)) else {
            if !stop.load(Ordering::Acquire) {
                emitter.invalid(CoinbaseProviderInvalidReason::Transport);
            }
            return;
        };
        if !emitter.established() {
            stop.store(true, Ordering::Release);
            return;
        }
        let result = connection.collect_until_stopped_with_heartbeat(
            &mut || stop.load(Ordering::Acquire),
            &mut |trade| {
                if !emitter.trade(trade.clone()) {
                    stop.store(true, Ordering::Release);
                }
            },
            &mut || {
                if !emitter.heartbeat() {
                    stop.store(true, Ordering::Release);
                }
            },
        );
        if stop.load(Ordering::Acquire) {
            return;
        }
        match result {
            Ok(health) => match health.outcome {
                SessionOutcome::SequenceGap => {
                    emitter.invalid(CoinbaseProviderInvalidReason::SequenceGap);
                }
                SessionOutcome::ClosedByPeer => {
                    emitter.invalid(CoinbaseProviderInvalidReason::ClosedByPeer);
                }
                SessionOutcome::InactivityTimeout => {
                    emitter.invalid(CoinbaseProviderInvalidReason::InactivityTimeout);
                }
                SessionOutcome::Completed => {
                    emitter.invalid(CoinbaseProviderInvalidReason::Transport);
                }
            },
            Err(_) => emitter.invalid(CoinbaseProviderInvalidReason::Transport),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CoinbaseDesktopEventError, CoinbaseProviderDriver, CoinbaseProviderDriverError,
        CoinbaseProviderEvent, CoinbaseProviderEvents, CoinbaseProviderInvalidReason,
        SessionEmitter, SessionTask, seed_coinbase_bar_history, try_recv_coinbase_aggregated_bar,
        try_recv_coinbase_bar, try_recv_coinbase_trade,
    };
    use crate::{
        CanonicalTrade, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig, CoinbaseConfig,
        FixedPointValue,
    };
    use axiusflow_desktop_history::{HistoryWorkerConfig, StartupCacheState};
    use axiusflow_desktop_provider_runtime::{
        ConnectTrigger, DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopMarketWorkerError,
        DesktopProviderConfig, DesktopProviderRuntime, DesktopProviderState, ProviderEnvironment,
        ProviderSessionDriver, RecoveryReason, SessionGeneration,
    };
    use axiusflow_desktop_storage::{
        CatalogKey, DataKind, HistoryScope, SegmentEncryptionKey, SegmentIdentity,
    };
    use axiusflow_market_data::MarketBar;
    use axiusflow_platform_runtime::CredentialVault;
    use axiusflow_provider_history::{SequencedHistory, VerifiedHistorySnapshot};
    use std::{
        fs,
        num::{NonZeroU64, NonZeroUsize},
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    struct UnavailableVault;

    impl CredentialVault for UnavailableVault {
        type Error = ();

        fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
            Err(())
        }

        fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            Err(())
        }

        fn delete(&self, _key: &str) -> Result<(), Self::Error> {
            Err(())
        }
    }

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
    }

    fn generation(value: u64) -> SessionGeneration {
        SessionGeneration::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN))
    }

    fn config() -> CoinbaseConfig {
        CoinbaseConfig::try_new(vec!["BTC-USD".to_string()]).expect("config validates")
    }

    fn trade() -> CanonicalTrade {
        trade_at(1_700_000_040, "37000.00", 7)
    }

    fn trade_at(second: i64, price: &str, sequence_num: u64) -> CanonicalTrade {
        trade_for_at("BTC-USD", second, price, sequence_num)
    }

    fn trade_for_at(
        product_id: &str,
        second: i64,
        price: &str,
        sequence_num: u64,
    ) -> CanonicalTrade {
        CanonicalTrade {
            product_id: product_id.to_string(),
            trade_id: format!("redacted-trade-{sequence_num}"),
            price: FixedPointValue::parse(price).expect("price parses"),
            size: FixedPointValue::parse("0.5").expect("size parses"),
            maker_side_buy: true,
            trade_time_unix_nanos: second * 1_000_000_000 + 1_000_000,
            provider_timestamp_unix_nanos: second * 1_000_000_000 + 2_000_000,
            sequence_num,
            canonical_sequence: sequence_num.saturating_add(1),
        }
    }

    fn wait_event(events: &CoinbaseProviderEvents) -> CoinbaseProviderEvent {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(event) = events.try_recv() {
                return event;
            }
            assert!(Instant::now() < deadline, "provider event timed out");
            thread::yield_now();
        }
    }

    fn controlled_task(include_trade: bool) -> Arc<SessionTask> {
        Arc::new(
            move |_config, _generation, stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                if !emitter.established() {
                    return;
                }
                if include_trade && !emitter.trade(trade()) {
                    return;
                }
                while !stop.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            },
        )
    }

    fn open_worker<T: Clone>(
        driver: CoinbaseProviderDriver,
    ) -> (
        DesktopMarketWorker<T, UnavailableVault, CoinbaseProviderDriver>,
        std::path::PathBuf,
    ) {
        open_worker_with_provider_config(
            driver,
            DesktopProviderConfig::new(nonzero(8), nonzero(1))
                .with_diagnostics(
                    ProviderEnvironment {
                        provider_id: "coinbase".to_string(),
                        system_id: "advanced_trade_public".to_string(),
                        environment: "production".to_string(),
                    },
                    None,
                )
                .expect("diagnostics identity validates"),
        )
    }

    fn open_worker_with_provider_config<T: Clone>(
        driver: CoinbaseProviderDriver,
        provider: DesktopProviderConfig,
    ) -> (
        DesktopMarketWorker<T, UnavailableVault, CoinbaseProviderDriver>,
        std::path::PathBuf,
    ) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "axiusflow-coinbase-driver-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("history root creates");
        let ui_thread = thread::spawn(|| thread::current().id())
            .join()
            .expect("UI thread identity is captured");
        let worker = DesktopMarketWorker::try_open(
            UnavailableVault,
            driver,
            "coinbase_public",
            &root,
            CatalogKey::try_new("catalog-key-v1".to_string(), [0x41; 32])
                .expect("catalog key validates"),
            ui_thread,
            DesktopMarketWorkerConfig {
                provider,
                history: HistoryWorkerConfig {
                    maximum_cache_entries: nonzero(2),
                    maximum_decoded_bytes: nonzero(1024),
                    maximum_charts: nonzero(2),
                    maximum_segment_read_bytes: nonzero(1024),
                    maximum_buffered_live: nonzero(2),
                    maximum_handoffs: nonzero(2),
                },
                maximum_catalog_entries: 2,
            },
        )
        .expect("market worker opens");
        (worker, root)
    }

    fn worker(
        driver: CoinbaseProviderDriver,
    ) -> (
        DesktopMarketWorker<u64, UnavailableVault, CoinbaseProviderDriver>,
        std::path::PathBuf,
    ) {
        open_worker(driver)
    }

    fn history_identity() -> SegmentIdentity {
        SegmentIdentity {
            scope: HistoryScope {
                provider_id: "coinbase".to_string(),
                account_id: crate::COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
                entitlement_revision: crate::ENTITLEMENT_CLASS.to_string(),
            },
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            data_kind: DataKind::Bars,
            resolution: "1m".to_string(),
            range_start_unix_nanos: 1_700_000_040_000_000_000,
            range_end_unix_nanos: 1_700_000_160_000_000_000,
            source_revision: 1,
            schema_revision: 1,
            calendar_revision: 1,
            adjustment_revision: 1,
            correction_revision: 1,
        }
    }

    fn history_bar(sequence: u64, second: i64, price: i64) -> SequencedHistory<MarketBar> {
        SequencedHistory {
            sequence: NonZeroU64::new(sequence).unwrap_or(NonZeroU64::MIN),
            value: MarketBar {
                source_sequence: sequence,
                exchange_timestamp_seconds: second,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: 100_000_000,
            },
        }
    }

    #[test]
    fn public_session_skips_vault_and_stops_the_exact_generation() {
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(4), controlled_task(true));
        let mut runtime = DesktopProviderRuntime::try_new(
            UnavailableVault,
            driver,
            "coinbase_public",
            DesktopProviderConfig::new(nonzero(8), nonzero(1)),
        )
        .expect("runtime configures");
        let active = runtime
            .connect(ConnectTrigger::Initial)
            .expect("public provider does not load vault credentials");
        assert_eq!(
            wait_event(&events),
            CoinbaseProviderEvent::Established { generation: active }
        );
        runtime
            .session_established(active)
            .expect("established callback is accepted");
        let event = wait_event(&events);
        assert!(matches!(
            event,
            CoinbaseProviderEvent::Trade {
                generation,
                trade: _
            } if generation == active
        ));
        let debug = format!("{event:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("redacted-trade-id"));
        runtime.stop().expect("active session stop is confirmed");
    }

    #[test]
    fn provider_callbacks_invoke_the_worker_wake_hook() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let wake = Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::AcqRel);
        });
        let (driver, events) = CoinbaseProviderDriver::with_task_and_wake(
            config(),
            nonzero(2),
            controlled_task(false),
            Some(wake),
        );
        let mut runtime = DesktopProviderRuntime::try_new(
            UnavailableVault,
            driver,
            "coinbase_public",
            DesktopProviderConfig::new(nonzero(8), nonzero(1)),
        )
        .expect("runtime configures");
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("public provider connects");
        assert_eq!(
            wait_event(&events),
            CoinbaseProviderEvent::Established { generation }
        );
        assert!(wake_count.load(Ordering::Acquire) >= 1);
        runtime.stop().expect("provider stops cleanly");
    }

    #[test]
    fn callback_overflow_is_latched_after_the_bounded_queue_drains() {
        let overflowed = Arc::new(Barrier::new(2));
        let drained = Arc::new(Barrier::new(2));
        let task_overflowed = Arc::clone(&overflowed);
        let task_drained = Arc::clone(&drained);
        let overflow_task: Arc<SessionTask> = Arc::new(
            move |_config, _generation, _stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                assert!(emitter.established());
                assert!(emitter.trade(trade()));
                assert!(!emitter.trade(trade()));
                task_overflowed.wait();
                task_drained.wait();
                assert!(!emitter.trade(trade()));
            },
        );
        let (mut driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(2), overflow_task);
        driver
            .start_session(generation(1), &[])
            .expect("session starts");
        overflowed.wait();
        assert_eq!(
            wait_event(&events),
            CoinbaseProviderEvent::Established {
                generation: generation(1)
            }
        );
        assert!(matches!(
            wait_event(&events),
            CoinbaseProviderEvent::Trade {
                generation: event_generation,
                ..
            } if event_generation == generation(1)
        ));
        assert_eq!(
            wait_event(&events),
            CoinbaseProviderEvent::Invalid {
                generation: generation(1),
                reason: CoinbaseProviderInvalidReason::EventQueueOverflow,
            }
        );
        drained.wait();
        driver
            .stop_session(generation(1))
            .expect("overflow task completes");
        assert_eq!(events.try_recv(), None);
    }

    #[test]
    fn composed_callback_overflow_reports_semantic_recovery() {
        let established = Arc::new(Barrier::new(2));
        let overflowed = Arc::new(Barrier::new(2));
        let task_established = Arc::clone(&established);
        let task_overflowed = Arc::clone(&overflowed);
        let overflow_task: Arc<SessionTask> = Arc::new(
            move |_config, _generation, stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                assert!(emitter.established());
                task_established.wait();
                assert!(emitter.trade(trade()));
                assert!(!emitter.trade(trade()));
                task_overflowed.wait();
                while !stop.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            },
        );
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(1), overflow_task);
        let (mut worker, root) = worker(driver);
        let active = worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            try_recv_coinbase_trade(&mut worker, &events)
                .expect("established callback is generation fenced");
            if matches!(
                worker.provider_state().expect("provider state is readable"),
                DesktopProviderState::Streaming { generation } if generation == active
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "established callback timed out");
            thread::yield_now();
        }
        established.wait();
        overflowed.wait();
        loop {
            try_recv_coinbase_trade(&mut worker, &events)
                .expect("overflow callback is generation fenced");
            if matches!(
                worker.provider_state().expect("provider state is readable"),
                DesktopProviderState::RecoveryRequired {
                    generation: Some(generation),
                    reason: RecoveryReason::SemanticQueueOverflow,
                } if generation == active
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "overflow callback timed out");
            thread::yield_now();
        }
        assert_eq!(
            worker.provider_state().expect("provider state is readable"),
            DesktopProviderState::RecoveryRequired {
                generation: Some(active),
                reason: RecoveryReason::SemanticQueueOverflow,
            }
        );
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn composed_worker_forwards_live_heartbeat_diagnostics() {
        let emitted = Arc::new(Barrier::new(2));
        let task_emitted = Arc::clone(&emitted);
        let heartbeat_task: Arc<SessionTask> = Arc::new(
            move |_config, _generation, stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                assert!(emitter.established());
                assert!(emitter.heartbeat());
                task_emitted.wait();
                while !stop.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            },
        );
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(4), heartbeat_task);
        let (mut worker, root) = worker(driver);
        worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        emitted.wait();
        while events.has_ready() {
            try_recv_coinbase_trade(&mut worker, &events)
                .expect("heartbeat callback is generation fenced");
        }

        let snapshot = worker
            .try_diagnostics_snapshot()
            .expect("diagnostics snapshot succeeds")
            .expect("first snapshot publishes");
        assert!(snapshot.heartbeat_age_nanos.is_some());
        assert!(snapshot.last_message_age_nanos.is_some());
        assert_eq!(snapshot.counters.trades, 0);
        worker.stop().expect("heartbeat session stops");
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn heartbeat_callbacks_are_generation_fenced_without_diagnostics() {
        let heartbeat_task: Arc<SessionTask> = Arc::new(
            |_config, _generation, stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                assert!(emitter.heartbeat());
                while !stop.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            },
        );

        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(2), Arc::clone(&heartbeat_task));
        let (mut worker, root) = open_worker_with_provider_config::<u64>(
            driver,
            DesktopProviderConfig::new(nonzero(8), nonzero(1)),
        );
        worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match try_recv_coinbase_trade(&mut worker, &events) {
                Err(CoinbaseDesktopEventError::Runtime(
                    DesktopMarketWorkerError::ProviderNotStreaming,
                )) => break,
                Ok(None) if Instant::now() < deadline => thread::yield_now(),
                result => panic!("unexpected heartbeat callback result: {result:?}"),
            }
        }
        worker.stop().expect("heartbeat session stops");
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");

        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(2), heartbeat_task);
        let (mut worker, root) = open_worker_with_provider_config::<u64>(
            driver,
            DesktopProviderConfig::new(nonzero(8), nonzero(1)),
        );
        worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let mut aggregators = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match try_recv_coinbase_aggregated_bar(&mut worker, &events, &mut aggregators) {
                Err(CoinbaseDesktopEventError::Runtime(
                    DesktopMarketWorkerError::ProviderNotStreaming,
                )) => break,
                Ok(None) if Instant::now() < deadline => thread::yield_now(),
                result => panic!("unexpected heartbeat callback result: {result:?}"),
            }
        }
        worker.stop().expect("heartbeat session stops");
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn composed_worker_rejects_another_drivers_callbacks() {
        let (driver, _events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(2), controlled_task(false));
        let (_other_driver, other_events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(2), controlled_task(false));
        let (mut worker, root) = worker(driver);
        assert!(matches!(
            try_recv_coinbase_trade(&mut worker, &other_events),
            Err(CoinbaseDesktopEventError::Runtime(
                axiusflow_desktop_provider_runtime::DesktopMarketWorkerError::CallbackSourceMismatch
            ))
        ));
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn stale_stop_cannot_cancel_the_active_generation() {
        let (mut driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(4), controlled_task(true));
        assert_eq!(
            driver.start_session(generation(1), b"unexpected-secret"),
            Err(CoinbaseProviderDriverError::UnexpectedCredentials)
        );
        driver
            .start_session(generation(1), &[])
            .expect("session starts");
        assert_eq!(
            driver.stop_session(generation(2)),
            Err(CoinbaseProviderDriverError::StaleGeneration)
        );
        driver
            .stop_session(generation(1))
            .expect("active generation stops");
        driver
            .start_session(generation(2), &[])
            .expect("fresh generation starts after confirmed stop");
        assert_eq!(
            wait_event(&events),
            CoinbaseProviderEvent::Established {
                generation: generation(2)
            }
        );
        assert!(matches!(
            wait_event(&events),
            CoinbaseProviderEvent::Trade {
                generation: event_generation,
                ..
            } if event_generation == generation(2)
        ));
        driver
            .stop_session(generation(2))
            .expect("fresh generation stops");
    }

    #[test]
    fn panicked_session_is_still_confirmed_stopped() {
        let panic_task: Arc<SessionTask> = Arc::new(
            |_config, _generation, _stop: Arc<AtomicBool>, _emitter: SessionEmitter| {
                panic!("injected session failure");
            },
        );
        let (mut driver, _events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(2), panic_task);
        driver
            .start_session(generation(1), &[])
            .expect("session starts");
        driver
            .stop_session(generation(1))
            .expect("joined panic confirms thread termination");
        driver
            .start_session(generation(2), &[])
            .expect("replacement session is not blocked");
        driver
            .stop_session(generation(2))
            .expect("replacement panic is also confirmed stopped");
    }

    #[test]
    fn composed_market_worker_applies_lifecycle_before_returning_trades() {
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(4), controlled_task(true));
        let (mut worker, root) = worker(driver);
        let active = worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let deadline = Instant::now() + Duration::from_secs(1);
        let received = loop {
            if let Some(received) = try_recv_coinbase_trade(&mut worker, &events)
                .expect("callback is generation fenced")
            {
                break received;
            }
            assert!(Instant::now() < deadline, "composed trade timed out");
            thread::yield_now();
        };
        assert_eq!(received.0, active);
        assert_eq!(received.1.trade_id, "redacted-trade-7");
        worker.stop().expect("composed worker stops");
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn malformed_live_trade_invalidates_the_streaming_generation() {
        let malformed_task: Arc<SessionTask> = Arc::new(
            |_config, _generation, stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                assert!(emitter.established());
                assert!(emitter.trade(trade_at(1_700_000_101, "38000.001", 8)));
                while !stop.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            },
        );
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(4), malformed_task);
        let (mut worker, root) = open_worker::<MarketBar>(driver);
        let mut aggregator = CoinbaseBarAggregator::new(
            CoinbaseBarAggregatorConfig::try_new("BTC-USD", 2, 8, nonzero(8))
                .expect("bar config validates"),
        );
        let active = worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match try_recv_coinbase_bar(&mut worker, &events, std::slice::from_mut(&mut aggregator))
            {
                Ok(None) => {}
                Err(CoinbaseDesktopEventError::Aggregation) => break,
                result => panic!("unexpected composed result: {result:?}"),
            }
            assert!(Instant::now() < deadline, "malformed trade timed out");
            thread::yield_now();
        }
        assert_eq!(
            worker.provider_state().expect("provider state is readable"),
            DesktopProviderState::RecoveryRequired {
                generation: Some(active),
                reason: RecoveryReason::TransportInvalid,
            }
        );
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn composed_history_seeds_live_coinbase_bar_aggregation() {
        let seeded = Arc::new(Barrier::new(2));
        let task_seeded = Arc::clone(&seeded);
        let aggregation_task: Arc<SessionTask> = Arc::new(
            move |_config, _generation, stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                assert!(emitter.established());
                task_seeded.wait();
                assert!(emitter.trade(trade_at(1_700_000_161, "38000.00", 8)));
                assert!(emitter.trade(trade_at(1_700_000_221, "39000.00", 9)));
                while !stop.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            },
        );
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(8), aggregation_task);
        let (mut worker, root) = open_worker::<MarketBar>(driver);
        let mut aggregator = CoinbaseBarAggregator::new(
            CoinbaseBarAggregatorConfig::try_new("BTC-USD", 2, 8, nonzero(8))
                .expect("bar config validates"),
        );
        let active = worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            try_recv_coinbase_bar(&mut worker, &events, std::slice::from_mut(&mut aggregator))
                .expect("establishment is accepted");
            if matches!(
                worker.provider_state().expect("provider state is readable"),
                DesktopProviderState::Streaming { generation } if generation == active
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "establishment timed out");
            thread::yield_now();
        }
        let identity = history_identity();
        let encryption_key =
            SegmentEncryptionKey::try_new("segment-key-v1".to_string(), [0x52; 32])
                .expect("segment key validates");
        worker
            .begin_history_handoff(active, identity.clone(), &encryption_key, 1_700_000_200)
            .expect("history handoff starts");
        worker
            .install_history_snapshot(
                active,
                &identity,
                VerifiedHistorySnapshot::try_new(
                    NonZeroU64::MIN,
                    vec![
                        history_bar(1, 1_700_000_040, 3_600_000),
                        history_bar(2, 1_700_000_100, 3_700_000),
                    ],
                )
                .expect("history snapshot validates"),
                2 * size_of::<SequencedHistory<MarketBar>>(),
                StartupCacheState::Cold,
            )
            .expect("history snapshot installs");
        assert_eq!(
            seed_coinbase_bar_history(
                &mut worker,
                &mut aggregator,
                active,
                "BTC-USD",
                &identity,
                &encryption_key,
                1_700_000_161,
            )
            .expect("authorized history seeds aggregation"),
            2
        );
        seeded.wait();
        let completed = loop {
            if let Some(completed) =
                try_recv_coinbase_bar(&mut worker, &events, std::slice::from_mut(&mut aggregator))
                    .expect("live trade aggregates")
            {
                break completed;
            }
            assert!(Instant::now() < deadline, "completed bar timed out");
            thread::yield_now();
        };
        assert_eq!(completed.0, active);
        assert_eq!(completed.1.source_sequence, 3);
        assert_eq!(completed.1.exchange_timestamp_seconds, 1_700_000_160);
        assert_eq!(completed.1.open, 3_800_000);
        assert_eq!(completed.1.high, 3_800_000);
        assert_eq!(completed.1.close, 3_800_000);
        assert_eq!(completed.1.volume, 50_000_000);
        worker.stop().expect("composed worker stops");
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn composed_aggregation_routes_each_trade_to_its_product() {
        let aggregation_task: Arc<SessionTask> = Arc::new(
            |_config, _generation, stop: Arc<AtomicBool>, emitter: SessionEmitter| {
                assert!(emitter.established());
                assert!(emitter.trade(trade_for_at("ETH-USD", 1_700_000_101, "1800.00", 8,)));
                assert!(emitter.trade(trade_for_at("ETH-USD", 1_700_000_161, "1900.00", 9,)));
                while !stop.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            },
        );
        let multi_product_config =
            CoinbaseConfig::try_new(vec!["BTC-USD".to_string(), "ETH-USD".to_string()])
                .expect("multi-product config validates");
        let (driver, events) =
            CoinbaseProviderDriver::with_task(multi_product_config, nonzero(8), aggregation_task);
        let (mut worker, root) = open_worker::<MarketBar>(driver);
        let mut aggregators = [
            CoinbaseBarAggregator::new(
                CoinbaseBarAggregatorConfig::try_new("BTC-USD", 2, 8, nonzero(8))
                    .expect("BTC bar config validates"),
            ),
            CoinbaseBarAggregator::new(
                CoinbaseBarAggregatorConfig::try_new("ETH-USD", 2, 8, nonzero(8))
                    .expect("ETH bar config validates"),
            ),
        ];
        let active = worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let deadline = Instant::now() + Duration::from_secs(1);
        let completed = loop {
            if let Some(completed) = try_recv_coinbase_bar(&mut worker, &events, &mut aggregators)
                .expect("multi-product trade routes")
            {
                break completed;
            }
            assert!(Instant::now() < deadline, "ETH bar timed out");
            thread::yield_now();
        };

        assert_eq!(completed.0, active);
        assert_eq!(completed.1.exchange_timestamp_seconds, 1_700_000_100);
        assert!(aggregators[0].history().is_empty());
        assert_eq!(aggregators[1].history(), vec![completed.1]);
        worker.stop().expect("composed worker stops");
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn composed_aggregation_rejects_duplicate_and_oversized_product_sets() {
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(4), controlled_task(false));
        let (mut worker, root) = open_worker::<MarketBar>(driver);
        let mut duplicates = [
            CoinbaseBarAggregator::new(
                CoinbaseBarAggregatorConfig::try_new("BTC-USD", 2, 8, nonzero(8))
                    .expect("bar config validates"),
            ),
            CoinbaseBarAggregator::new(
                CoinbaseBarAggregatorConfig::try_new("BTC-USD", 2, 8, nonzero(8))
                    .expect("bar config validates"),
            ),
        ];
        assert!(matches!(
            try_recv_coinbase_bar(&mut worker, &events, &mut duplicates),
            Err(CoinbaseDesktopEventError::DuplicateAggregator)
        ));

        let mut oversized = (0..=crate::MAXIMUM_PRODUCTS)
            .map(|index| {
                CoinbaseBarAggregator::new(
                    CoinbaseBarAggregatorConfig::try_new(format!("P{index}-USD"), 2, 8, nonzero(8))
                        .expect("bar config validates"),
                )
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            try_recv_coinbase_bar(&mut worker, &events, &mut oversized),
            Err(CoinbaseDesktopEventError::AggregatorLimit {
                maximum: crate::MAXIMUM_PRODUCTS
            })
        ));
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }

    #[test]
    fn fresh_generation_establishment_resets_adapter_aggregation() {
        let (driver, events) =
            CoinbaseProviderDriver::with_task(config(), nonzero(4), controlled_task(false));
        let (mut worker, root) = open_worker::<MarketBar>(driver);
        let mut aggregator = CoinbaseBarAggregator::new(
            CoinbaseBarAggregatorConfig::try_new("BTC-USD", 2, 8, nonzero(8))
                .expect("bar config validates"),
        );
        aggregator
            .seed_completed_history(&[MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 1_700_000_040,
                open: 3_700_000,
                high: 3_700_000,
                low: 3_700_000,
                close: 3_700_000,
                volume: 50_000_000,
            }])
            .expect("prior generation history seeds");
        let active = worker
            .connect(ConnectTrigger::Initial)
            .expect("public session starts");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            try_recv_coinbase_bar(&mut worker, &events, std::slice::from_mut(&mut aggregator))
                .expect("establishment is accepted");
            if matches!(
                worker.provider_state().expect("provider state is readable"),
                DesktopProviderState::Streaming { generation } if generation == active
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "establishment timed out");
            thread::yield_now();
        }

        assert!(aggregator.history().is_empty());
        worker.stop().expect("composed worker stops");
        drop(worker);
        fs::remove_dir_all(root).expect("history root removes");
    }
}
