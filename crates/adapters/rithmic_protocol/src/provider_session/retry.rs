use super::{RETRY_DELAYS, RithmicProviderDriver, RithmicProviderEvents};
use crate::{
    ConnectTrigger, NetworkEvent, ProviderInvalidationReason, ProviderSessionDriver,
    ProviderSessionEvent, RecoveryReason, RetryDisposition, RithmicProviderRuntime,
    RithmicProviderRuntimeError, RithmicProviderRuntimeState, SessionGeneration,
};
use aeris_platform_runtime::{CredentialVault, PowerEvent};
use std::{error::Error, fmt, time::Instant};

/// One scheduled transient reconnect ticket for a retired session generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicRetryTicket {
    pub failed_generation: SessionGeneration,
    pub reason: ProviderInvalidationReason,
    pub due_at: Instant,
}

/// Nonblocking capped retry scheduler owned by the engine provider-worker loop.
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
        reason: ProviderInvalidationReason,
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
            reason,
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
    pub fn retry_due<V: CredentialVault, D: ProviderSessionDriver>(
        &mut self,
        worker: &mut RithmicProviderRuntime<V, D>,
        now: Instant,
    ) -> Result<Option<SessionGeneration>, RithmicProviderRuntimeError> {
        let state = worker.state()?;
        if !self.take_due(state, now) {
            return Ok(None);
        }
        match worker.connect(ConnectTrigger::Retry) {
            Ok(generation) => Ok(Some(generation)),
            Err(error) => {
                self.record_start_failure(worker.state()?, now);
                Err(error)
            }
        }
    }

    pub(super) fn record_start_failure(
        &mut self,
        state: RithmicProviderRuntimeState,
        now: Instant,
    ) {
        if let RithmicProviderRuntimeState::RecoveryRequired {
            generation: Some(generation),
            reason: RecoveryReason::ProviderFailure,
        } = state
        {
            self.record_invalid(
                generation,
                ProviderInvalidationReason::Transport,
                RetryDisposition::Transient,
                now,
            );
        }
    }

    pub(super) fn take_due(&mut self, state: RithmicProviderRuntimeState, now: Instant) -> bool {
        let Some(ticket) = self.ticket else {
            return false;
        };
        if now < ticket.due_at {
            return false;
        }
        let exact_recovery = matches!(
            state,
            RithmicProviderRuntimeState::RecoveryRequired {
                generation: Some(generation),
                reason: RecoveryReason::TransportInvalid | RecoveryReason::SemanticQueueOverflow | RecoveryReason::ProviderFailure,
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
pub fn apply_rithmic_environment_event<V: CredentialVault>(
    worker: &mut RithmicProviderRuntime<V, RithmicProviderDriver>,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    event: RithmicEnvironmentEvent,
) -> Result<Option<SessionGeneration>, RithmicProviderEventError> {
    if !worker.driver_matches(|driver| driver.owns_events(events))? {
        return Err(RithmicProviderEventError::CallbackSourceMismatch);
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
pub enum RithmicProviderEventError {
    Runtime(RithmicProviderRuntimeError),
    CallbackSourceMismatch,
    GenerationMismatch,
    ProviderNotStreaming,
    MissingRetryDisposition,
}

impl fmt::Display for RithmicProviderEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "Rithmic callback failed: {error}"),
            Self::CallbackSourceMismatch => {
                formatter.write_str("Rithmic callback source mismatched")
            }
            Self::GenerationMismatch => {
                formatter.write_str("Rithmic callback belongs to a stale provider generation")
            }
            Self::ProviderNotStreaming => {
                formatter.write_str("Rithmic provider session is not streaming")
            }
            Self::MissingRetryDisposition => {
                formatter.write_str("Rithmic invalidation omitted retry classification")
            }
        }
    }
}

impl Error for RithmicProviderEventError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::CallbackSourceMismatch
            | Self::GenerationMismatch
            | Self::ProviderNotStreaming
            | Self::MissingRetryDisposition => None,
        }
    }
}

impl From<RithmicProviderRuntimeError> for RithmicProviderEventError {
    fn from(error: RithmicProviderRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// Applies at most one generation-fenced semantic callback.
///
/// # Errors
///
/// Returns a redacted source, event-shape, or lifecycle failure.
pub fn try_recv_rithmic_event<V: CredentialVault>(
    worker: &mut RithmicProviderRuntime<V, RithmicProviderDriver>,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    now: Instant,
) -> Result<Option<AppliedRithmicEvent>, RithmicProviderEventError> {
    if !worker.driver_matches(|driver| driver.owns_events(events))? {
        return Err(RithmicProviderEventError::CallbackSourceMismatch);
    }
    let Some(callback) = events.try_recv() else {
        return Ok(None);
    };
    match worker.state()? {
        RithmicProviderRuntimeState::Connecting { generation, .. }
        | RithmicProviderRuntimeState::Streaming { generation }
            if generation == callback.generation => {}
        RithmicProviderRuntimeState::Connecting { .. }
        | RithmicProviderRuntimeState::Streaming { .. } => {
            return Err(RithmicProviderEventError::GenerationMismatch);
        }
        _ => return Err(RithmicProviderEventError::ProviderNotStreaming),
    }
    match &callback.event {
        ProviderSessionEvent::InstrumentsDiscovered { generation, .. } => {
            worker.session_established(*generation)?;
            retries.established();
        }
        ProviderSessionEvent::Market { generation, event } => {
            ensure_streaming_generation(worker, *generation)?;
            // Every Rithmic frame carries the gateway clock; only some carry
            // an exchange clock, so feed latency is measured from the gateway.
            let message_timestamp = event.metadata().timestamps.provider_unix_nanos;
            worker.record_market_event_diagnostics(*generation, event, message_timestamp)?;
        }
        ProviderSessionEvent::Heartbeat { generation, .. } => {
            ensure_streaming_generation(worker, *generation)?;
            worker.record_heartbeat_diagnostics(*generation)?;
        }
        ProviderSessionEvent::Invalidated {
            generation: Some(generation),
            reason,
        } => {
            if *reason == ProviderInvalidationReason::QueueOverflow {
                worker.session_callback_queue_overflow(*generation)?;
            } else {
                worker.session_invalid_for_provider(*generation, *reason)?;
            }
            let retry = callback
                .retry
                .ok_or(RithmicProviderEventError::MissingRetryDisposition)?;
            return if let Some(ticket) = retries.record_invalid(*generation, *reason, retry, now) {
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

fn ensure_streaming_generation<V: CredentialVault>(
    worker: &RithmicProviderRuntime<V, RithmicProviderDriver>,
    generation: SessionGeneration,
) -> Result<(), RithmicProviderEventError> {
    match worker.state()? {
        RithmicProviderRuntimeState::Streaming { generation: active } if active == generation => {
            Ok(())
        }
        RithmicProviderRuntimeState::Streaming { .. } => {
            Err(RithmicProviderEventError::GenerationMismatch)
        }
        _ => Err(RithmicProviderEventError::ProviderNotStreaming),
    }
}
