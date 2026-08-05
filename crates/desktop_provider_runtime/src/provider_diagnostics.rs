use crate::{
    AuthenticationState, ProviderInvalidationReason, ProviderSessionEvent, RecoveryReason,
    SessionGeneration,
};
use axiusflow_market_data::MarketEvent;
use axiusflow_observability::{
    DiagnosticsError, DiagnosticsQueue, FeedConnectionState, FeedCounter, FeedDiagnostics,
    FeedDiagnosticsSnapshot, FeedIdentity, FeedRecoveryReason, HistoryDiagnosticsState,
    LatencyError, LatencyTimestampChain, LocalLatencyMetric, OrderBookDiagnosticsState,
};
use core::fmt;
use std::{error::Error, num::NonZeroU64};

/// Redacted failures at the provider-session diagnostics boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFeedDiagnosticsError {
    Diagnostics(DiagnosticsError),
    MissingActiveGeneration,
    NotStreaming,
    StaleGeneration,
}

impl fmt::Display for ProviderFeedDiagnosticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "provider feed diagnostics failed: {self:?}")
    }
}

impl Error for ProviderFeedDiagnosticsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Diagnostics(error) => Some(error),
            Self::MissingActiveGeneration | Self::NotStreaming | Self::StaleGeneration => None,
        }
    }
}

impl From<DiagnosticsError> for ProviderFeedDiagnosticsError {
    fn from(error: DiagnosticsError) -> Self {
        Self::Diagnostics(error)
    }
}

/// Provider-neutral wiring between validated headless events and feed-health evidence.
pub struct ProviderFeedDiagnostics {
    diagnostics: FeedDiagnostics,
    active_generation: Option<SessionGeneration>,
    terminal_generation: Option<SessionGeneration>,
    streaming: bool,
    last_observation_monotonic_nanos: Option<u64>,
}

impl fmt::Debug for ProviderFeedDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderFeedDiagnostics")
            .field("diagnostics", &self.diagnostics)
            .field("active_generation", &self.active_generation)
            .field("terminal_generation", &self.terminal_generation)
            .field("streaming", &self.streaming)
            .field(
                "last_observation_monotonic_nanos",
                &self.last_observation_monotonic_nanos,
            )
            .finish()
    }
}

impl ProviderFeedDiagnostics {
    /// Creates one bounded diagnostics owner for a non-secret provider environment.
    ///
    /// # Errors
    ///
    /// Returns a redacted identity validation failure.
    pub fn try_new(
        provider: impl Into<String>,
        system: impl Into<String>,
        environment: impl Into<String>,
        detailed_latency_maximum_nanos: Option<NonZeroU64>,
    ) -> Result<Self, ProviderFeedDiagnosticsError> {
        let identity = FeedIdentity::try_new(provider, system, environment)?;
        Ok(Self {
            diagnostics: FeedDiagnostics::new(identity, detailed_latency_maximum_nanos),
            active_generation: None,
            terminal_generation: None,
            streaming: false,
            last_observation_monotonic_nanos: None,
        })
    }

    /// Starts one production runtime generation at a local monotonic timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error for stale generation or monotonic timestamp evidence.
    pub fn begin_runtime_session(
        &mut self,
        generation: SessionGeneration,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.accept_authentication_generation(generation, monotonic_nanos)?;
        self.streaming = false;
        self.diagnostics
            .set_connection_state(FeedConnectionState::Discovering);
        Ok(())
    }

    /// Marks the active production generation as streaming.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, stale, or regressed runtime evidence.
    pub fn mark_runtime_streaming(
        &mut self,
        generation: SessionGeneration,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.require_active_generation(generation)?;
        self.streaming = true;
        self.diagnostics
            .set_connection_state(FeedConnectionState::Streaming);
        Ok(())
    }

    /// Applies one validated provider-session event at a local monotonic timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error when generation evidence is missing, stale, or regresses.
    pub fn observe_event(
        &mut self,
        event: &ProviderSessionEvent,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        match event {
            ProviderSessionEvent::DiscoveryStarted
            | ProviderSessionEvent::SystemsDiscovered { .. } => {
                self.streaming = false;
                self.diagnostics
                    .set_connection_state(FeedConnectionState::Discovering);
            }
            ProviderSessionEvent::AuthenticationChanged { generation, state } => {
                self.accept_authentication_generation(*generation, monotonic_nanos)?;
                self.streaming = false;
                match state {
                    AuthenticationState::Required | AuthenticationState::Accepted => self
                        .diagnostics
                        .set_connection_state(FeedConnectionState::Authenticating),
                    AuthenticationState::Rejected => self
                        .diagnostics
                        .require_recovery(FeedRecoveryReason::Authentication),
                    AuthenticationState::AgreementRequired => self
                        .diagnostics
                        .require_recovery(FeedRecoveryReason::AgreementRequired),
                }
            }
            ProviderSessionEvent::InstrumentsDiscovered { generation, .. } => {
                self.require_active_generation(*generation)?;
                self.streaming = true;
                self.diagnostics
                    .set_connection_state(FeedConnectionState::Streaming);
            }
            ProviderSessionEvent::Market { generation, event } => {
                self.require_streaming_generation(*generation)?;
                let metadata = event.metadata();
                self.diagnostics
                    .record_message(monotonic_nanos, metadata.timestamps.provider_unix_nanos);
                match event {
                    MarketEvent::Trade(_) => self.diagnostics.increment(FeedCounter::Trades),
                    MarketEvent::Quote(_) => self.diagnostics.increment(FeedCounter::Quotes),
                    MarketEvent::DepthSnapshot(_) => {
                        self.diagnostics.increment(FeedCounter::DepthSnapshots);
                        self.diagnostics
                            .set_order_book_state(OrderBookDiagnosticsState::Ready);
                    }
                    MarketEvent::DepthDelta(_) => {
                        self.diagnostics.increment(FeedCounter::DepthDeltas);
                    }
                }
            }
            ProviderSessionEvent::Heartbeat { generation, .. } => {
                self.require_streaming_generation(*generation)?;
                self.diagnostics.record_heartbeat(monotonic_nanos);
            }
            ProviderSessionEvent::Invalidated { generation, reason } => {
                match (*generation, self.active_generation) {
                    (Some(generation), _) => {
                        self.require_active_generation(generation)?;
                        self.terminal_generation = Some(generation);
                        self.active_generation = None;
                    }
                    (None, Some(_)) => return self.reject_stale(),
                    (None, None) => {}
                }
                self.streaming = false;
                self.observe_invalidation(*reason);
            }
            ProviderSessionEvent::Stopped => {
                if let Some(generation) = self.active_generation.take() {
                    self.terminal_generation = Some(generation);
                }
                self.streaming = false;
                self.diagnostics
                    .set_connection_state(FeedConnectionState::Stopped);
            }
        }
        Ok(())
    }

    /// Records one immutable model publication for the active generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing or stale generation.
    pub fn record_publication(
        &mut self,
        generation: SessionGeneration,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.require_streaming_generation(generation)?;
        self.diagnostics.increment(FeedCounter::Publications);
        Ok(())
    }

    /// Records one live production trade and its provider-clock timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, stale, non-streaming, or regressed evidence.
    pub fn record_runtime_trade(
        &mut self,
        generation: SessionGeneration,
        provider_timestamp_unix_nanos: Option<i64>,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.require_streaming_generation(generation)?;
        self.diagnostics
            .record_message(monotonic_nanos, provider_timestamp_unix_nanos);
        self.diagnostics.increment(FeedCounter::Trades);
        Ok(())
    }

    /// Records one live production heartbeat.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, stale, non-streaming, or regressed evidence.
    pub fn record_runtime_heartbeat(
        &mut self,
        generation: SessionGeneration,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.require_streaming_generation(generation)?;
        self.diagnostics.record_heartbeat(monotonic_nanos);
        Ok(())
    }

    pub fn record_duplicate(&mut self) {
        self.diagnostics.increment(FeedCounter::Duplicates);
    }

    pub fn record_ui_coalescing(&mut self, count: u64) {
        self.diagnostics
            .increment_by(FeedCounter::CoalescedUiUpdates, count);
    }

    pub const fn set_history_state(&mut self, state: HistoryDiagnosticsState) {
        self.diagnostics.set_history_state(state);
    }

    pub const fn set_order_book_state(&mut self, state: OrderBookDiagnosticsState) {
        self.diagnostics.set_order_book_state(state);
    }

    /// Records a coarse provider-neutral recovery request.
    ///
    /// # Errors
    ///
    /// Returns an error for monotonic timestamp regression.
    pub fn require_runtime_recovery(
        &mut self,
        reason: RecoveryReason,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.require_runtime_recovery_inner(reason, monotonic_nanos, true)
    }

    /// Requires semantic-queue recovery after the rejected enqueue was counted.
    ///
    /// # Errors
    ///
    /// Returns an error for monotonic timestamp regression.
    pub fn require_runtime_recovery_after_recorded_overflow(
        &mut self,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.require_runtime_recovery_inner(
            RecoveryReason::SemanticQueueOverflow,
            monotonic_nanos,
            false,
        )
    }

    fn require_runtime_recovery_inner(
        &mut self,
        reason: RecoveryReason,
        monotonic_nanos: u64,
        record_overflow: bool,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        let reason = match reason {
            RecoveryReason::CredentialUnavailable => FeedRecoveryReason::Authentication,
            RecoveryReason::ProviderFailure | RecoveryReason::TransportInvalid => {
                FeedRecoveryReason::Transport
            }
            RecoveryReason::SemanticQueueOverflow => FeedRecoveryReason::QueueOverflow,
        };
        if record_overflow && matches!(reason, FeedRecoveryReason::QueueOverflow) {
            self.diagnostics.increment(FeedCounter::Overflows);
        }
        self.streaming = false;
        self.diagnostics.require_recovery(reason);
        Ok(())
    }

    /// Records one stale production callback without mutating active feed state.
    ///
    /// # Errors
    ///
    /// Returns an error for monotonic timestamp regression.
    pub fn record_stale_callback(
        &mut self,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.diagnostics.increment(FeedCounter::StaleCallbacks);
        Ok(())
    }

    /// Records one rejected semantic event enqueue.
    ///
    /// # Errors
    ///
    /// Returns an error for monotonic timestamp regression.
    pub fn record_semantic_queue_overflow(
        &mut self,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.diagnostics.increment(FeedCounter::Overflows);
        Ok(())
    }

    /// Marks a production runtime as disconnected or environmentally unavailable.
    ///
    /// # Errors
    ///
    /// Returns an error for monotonic timestamp regression.
    pub fn mark_runtime_disconnected(
        &mut self,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.streaming = false;
        self.diagnostics
            .set_connection_state(FeedConnectionState::Disconnected);
        Ok(())
    }

    /// Marks a production runtime as permanently stopped.
    ///
    /// # Errors
    ///
    /// Returns an error for monotonic timestamp regression.
    pub fn mark_runtime_stopped(
        &mut self,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.streaming = false;
        self.diagnostics
            .set_connection_state(FeedConnectionState::Stopped);
        Ok(())
    }

    /// Observes one fixed-capacity runtime queue.
    ///
    /// # Errors
    ///
    /// Returns an error for impossible occupancy or a changing registered bound.
    pub fn observe_queue(
        &mut self,
        queue: DiagnosticsQueue,
        current_items: usize,
        item_capacity: usize,
        current_bytes: usize,
        byte_capacity: usize,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.diagnostics.observe_queue(
            queue,
            current_items,
            item_capacity,
            current_bytes,
            byte_capacity,
        )?;
        Ok(())
    }

    /// Observes approximate retained runtime memory against an immutable bound.
    ///
    /// # Errors
    ///
    /// Returns an error when usage exceeds or changes its registered bound.
    pub fn observe_runtime_memory(
        &mut self,
        current_bytes: usize,
        bound_bytes: usize,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.diagnostics
            .observe_runtime_memory(current_bytes, bound_bytes)?;
        Ok(())
    }

    /// Records one correctly labelled local latency interval.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, reversed, or regressed timestamps.
    pub fn record_latency_chain(
        &mut self,
        metric: LocalLatencyMetric,
        chain: &LatencyTimestampChain,
    ) -> Result<(), LatencyError> {
        self.diagnostics.record_latency_chain(metric, chain)
    }

    /// Publishes at most one immutable feed-health snapshot per 250 milliseconds.
    ///
    /// # Errors
    ///
    /// Returns an error for local monotonic-clock regression.
    pub fn try_snapshot(
        &mut self,
        monotonic_nanos: u64,
        wall_clock_unix_nanos: i64,
    ) -> Result<Option<FeedDiagnosticsSnapshot>, ProviderFeedDiagnosticsError> {
        self.accept_monotonic_observation(monotonic_nanos)?;
        self.diagnostics
            .try_snapshot(monotonic_nanos, wall_clock_unix_nanos)
            .map_err(Into::into)
    }

    fn accept_authentication_generation(
        &mut self,
        generation: SessionGeneration,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        if self
            .terminal_generation
            .is_some_and(|terminal| generation <= terminal)
        {
            return self.reject_stale();
        }
        match self.active_generation {
            Some(active) if generation < active => self.reject_stale(),
            Some(active) if generation == active => Ok(()),
            Some(_) | None => {
                self.diagnostics.begin_session(
                    NonZeroU64::new(generation.get())
                        .ok_or(ProviderFeedDiagnosticsError::MissingActiveGeneration)?,
                    monotonic_nanos,
                )?;
                self.active_generation = Some(generation);
                Ok(())
            }
        }
    }

    fn require_active_generation(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        match self.active_generation {
            Some(active) if active == generation => Ok(()),
            Some(_) => self.reject_stale(),
            None if self
                .terminal_generation
                .is_some_and(|terminal| generation <= terminal) =>
            {
                self.reject_stale()
            }
            None => Err(ProviderFeedDiagnosticsError::MissingActiveGeneration),
        }
    }

    fn require_streaming_generation(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        self.require_active_generation(generation)?;
        if !self.streaming {
            return Err(ProviderFeedDiagnosticsError::NotStreaming);
        }
        Ok(())
    }

    fn reject_stale<T>(&mut self) -> Result<T, ProviderFeedDiagnosticsError> {
        self.diagnostics.increment(FeedCounter::StaleCallbacks);
        Err(ProviderFeedDiagnosticsError::StaleGeneration)
    }

    fn accept_monotonic_observation(
        &mut self,
        monotonic_nanos: u64,
    ) -> Result<(), ProviderFeedDiagnosticsError> {
        if self
            .last_observation_monotonic_nanos
            .is_some_and(|last| monotonic_nanos < last)
        {
            return Err(ProviderFeedDiagnosticsError::Diagnostics(
                DiagnosticsError::MonotonicClockRegressed,
            ));
        }
        self.last_observation_monotonic_nanos = Some(monotonic_nanos);
        Ok(())
    }

    fn observe_invalidation(&mut self, reason: ProviderInvalidationReason) {
        let reason = match reason {
            ProviderInvalidationReason::Transport => FeedRecoveryReason::Transport,
            ProviderInvalidationReason::Authentication => FeedRecoveryReason::Authentication,
            ProviderInvalidationReason::AgreementRequired => FeedRecoveryReason::AgreementRequired,
            ProviderInvalidationReason::UnsupportedSystem => FeedRecoveryReason::UnsupportedSystem,
            ProviderInvalidationReason::SchemaMismatch => FeedRecoveryReason::SchemaMismatch,
            ProviderInvalidationReason::HeartbeatSilence => FeedRecoveryReason::HeartbeatSilence,
            ProviderInvalidationReason::MessageSilence => FeedRecoveryReason::MessageSilence,
            ProviderInvalidationReason::SequenceGap => {
                self.diagnostics.increment(FeedCounter::Gaps);
                self.diagnostics
                    .set_order_book_state(OrderBookDiagnosticsState::Recovering);
                FeedRecoveryReason::SequenceGap
            }
            ProviderInvalidationReason::QueueOverflow => {
                self.diagnostics.increment(FeedCounter::Overflows);
                FeedRecoveryReason::QueueOverflow
            }
            ProviderInvalidationReason::MalformedMessage => {
                self.diagnostics.increment(FeedCounter::MalformedMessages);
                FeedRecoveryReason::MalformedMessage
            }
        };
        self.diagnostics.require_recovery(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstrumentDescriptor, ProviderEnvironment};
    use axiusflow_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};

    fn generation(value: u64) -> SessionGeneration {
        SessionGeneration::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN))
    }

    fn diagnostics() -> ProviderFeedDiagnostics {
        ProviderFeedDiagnostics::try_new("rithmic", "RITHMIC_TEST", "Test", None)
            .expect("diagnostics identity validates")
    }

    fn authentication(generation: SessionGeneration) -> ProviderSessionEvent {
        ProviderSessionEvent::AuthenticationChanged {
            generation,
            state: AuthenticationState::Accepted,
        }
    }

    fn instrument_event(generation: SessionGeneration) -> ProviderSessionEvent {
        ProviderSessionEvent::InstrumentsDiscovered {
            generation,
            instruments: vec![InstrumentDescriptor {
                instrument_id: "instrument:rithmic:cme:es".to_string(),
                provider_symbol: "ESM7".to_string(),
                display_symbol: "ES".to_string(),
                venue_id: "CME".to_string(),
                price_scale: 2,
                quantity_scale: 0,
            }],
        }
    }

    fn trade_event(generation: SessionGeneration) -> ProviderSessionEvent {
        ProviderSessionEvent::Market {
            generation,
            event: MarketEvent::Trade(MarketTrade {
                metadata: EventMetadata {
                    provider_id: "rithmic".to_string(),
                    instrument_id: "instrument:rithmic:cme:es".to_string(),
                    entitlement_id: "fixture_test_realtime".to_string(),
                    source_sequence: 1,
                    session_generation: generation.get(),
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(1_800_000_000_000_000_000),
                        provider_unix_nanos: Some(1_800_000_001_000_000_000),
                        received_unix_nanos: 1_800_000_002_000_000_000,
                    },
                },
                trade_id: "fixture-trade".to_string(),
                price: 525_025,
                quantity: 3,
                aggressor: AggressorSide::Buy,
            }),
        }
    }

    #[test]
    fn session_events_publish_fenced_redacted_feed_health() {
        let generation = generation(1);
        let mut diagnostics = diagnostics();
        diagnostics
            .observe_event(&ProviderSessionEvent::DiscoveryStarted, 10)
            .expect("discovery records");
        diagnostics
            .observe_event(
                &ProviderSessionEvent::SystemsDiscovered {
                    environments: vec![ProviderEnvironment {
                        provider_id: "rithmic".to_string(),
                        system_id: "RITHMIC_TEST".to_string(),
                        environment: "Test".to_string(),
                    }],
                },
                20,
            )
            .expect("systems record");
        diagnostics
            .observe_event(&authentication(generation), 30)
            .expect("authentication records");
        diagnostics
            .observe_event(&instrument_event(generation), 40)
            .expect("instruments record");
        diagnostics
            .observe_event(&trade_event(generation), 50)
            .expect("trade records");
        diagnostics
            .observe_event(
                &ProviderSessionEvent::Heartbeat {
                    generation,
                    received_unix_nanos: 1_800_000_003_000_000_000,
                },
                60,
            )
            .expect("heartbeat records");
        diagnostics
            .record_publication(generation, 70)
            .expect("publication records");

        let snapshot = diagnostics
            .try_snapshot(250_000_060, 1_800_000_004_000_000_000)
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Streaming);
        assert_eq!(snapshot.session_generation, NonZeroU64::new(1));
        assert_eq!(snapshot.counters.trades, 1);
        assert_eq!(snapshot.counters.publications, 1);
        assert_eq!(snapshot.heartbeat_age_nanos, Some(250_000_000));
        assert_eq!(snapshot.last_message_age_nanos, Some(250_000_000));
        assert_eq!(
            snapshot
                .provider_timestamp_age
                .expect("provider age exists")
                .nanos,
            3_000_000_000
        );
    }

    #[test]
    fn stale_generations_increment_only_the_stale_callback_counter() {
        let first = generation(1);
        let second = generation(2);
        let mut diagnostics = diagnostics();
        diagnostics
            .observe_event(&authentication(first), 10)
            .expect("first generation records");
        diagnostics
            .observe_event(&authentication(second), 20)
            .expect("second generation records");
        assert_eq!(
            diagnostics.observe_event(&trade_event(first), 30),
            Err(ProviderFeedDiagnosticsError::StaleGeneration)
        );
        let snapshot = diagnostics
            .try_snapshot(250_000_020, 1_800_000_004_000_000_000)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        assert_eq!(snapshot.session_generation, NonZeroU64::new(2));
        assert_eq!(snapshot.reconnect_count, 1);
        assert_eq!(snapshot.counters.stale_callbacks, 1);
        assert_eq!(snapshot.counters.trades, 0);
    }

    #[test]
    fn invalidations_map_to_coarse_recovery_and_counter_evidence() {
        let generation = generation(1);
        let mut diagnostics = diagnostics();
        diagnostics
            .observe_event(&authentication(generation), 10)
            .expect("generation records");
        diagnostics
            .observe_event(
                &ProviderSessionEvent::Invalidated {
                    generation: Some(generation),
                    reason: ProviderInvalidationReason::SequenceGap,
                },
                20,
            )
            .expect("invalidation records");
        let snapshot = diagnostics
            .try_snapshot(250_000_010, 1_800_000_004_000_000_000)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Recovering);
        assert_eq!(
            snapshot.recovery_reason,
            Some(FeedRecoveryReason::SequenceGap)
        );
        assert_eq!(snapshot.counters.gaps, 1);
        assert_eq!(
            snapshot.order_book_state,
            OrderBookDiagnosticsState::Recovering
        );
    }

    #[test]
    fn recovery_and_stop_fence_later_publications() {
        let generation = generation(1);
        let mut diagnostics = diagnostics();
        diagnostics
            .observe_event(&authentication(generation), 10)
            .expect("generation records");
        diagnostics
            .observe_event(&instrument_event(generation), 20)
            .expect("streaming records");
        diagnostics
            .require_runtime_recovery(RecoveryReason::SemanticQueueOverflow, 30)
            .expect("recovery records");
        assert_eq!(
            diagnostics.record_publication(generation, 40),
            Err(ProviderFeedDiagnosticsError::NotStreaming)
        );
        diagnostics
            .observe_event(&ProviderSessionEvent::Stopped, 50)
            .expect("stop records");
        assert_eq!(
            diagnostics.observe_event(&trade_event(generation), 60),
            Err(ProviderFeedDiagnosticsError::StaleGeneration)
        );
        assert_eq!(
            diagnostics.observe_event(&authentication(generation), 70),
            Err(ProviderFeedDiagnosticsError::StaleGeneration)
        );
        let snapshot = diagnostics
            .try_snapshot(250_000_070, 1_800_000_004_000_000_000)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Stopped);
        assert_eq!(snapshot.counters.stale_callbacks, 2);
    }

    #[test]
    fn unfenced_invalidation_and_monotonic_regression_fail_closed() {
        let generation = generation(1);
        let mut runtime_diagnostics = diagnostics();
        runtime_diagnostics
            .observe_event(&authentication(generation), 20)
            .expect("generation records");
        assert_eq!(
            runtime_diagnostics.observe_event(
                &ProviderSessionEvent::Invalidated {
                    generation: None,
                    reason: ProviderInvalidationReason::Transport,
                },
                30,
            ),
            Err(ProviderFeedDiagnosticsError::StaleGeneration)
        );
        assert_eq!(
            runtime_diagnostics.observe_event(&instrument_event(generation), 25),
            Err(ProviderFeedDiagnosticsError::Diagnostics(
                DiagnosticsError::MonotonicClockRegressed
            ))
        );

        let mut snapshot_fenced = diagnostics();
        snapshot_fenced
            .observe_event(&authentication(generation), 10)
            .expect("generation records");
        snapshot_fenced
            .try_snapshot(1_000, 1_800_000_004_000_000_000)
            .expect("snapshot timestamp records");
        assert_eq!(
            snapshot_fenced.observe_event(&instrument_event(generation), 750),
            Err(ProviderFeedDiagnosticsError::Diagnostics(
                DiagnosticsError::MonotonicClockRegressed
            ))
        );
    }
}
