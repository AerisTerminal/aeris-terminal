use super::{
    ENVIRONMENT, MAXIMUM_CALLBACK_BYTES, MAXIMUM_CALLBACK_EVENTS, MAXIMUM_IDENTITY_BYTES,
    MAXIMUM_INSTRUMENTS, MAXIMUM_SILENCE_TIMEOUT, PROVIDER_ID, RithmicCatalogEvent,
    RithmicInstrumentSelection, RithmicProductListing, RithmicProviderCommandError,
    RithmicProviderInstrument, RithmicSessionCommand, RithmicSymbolSearch, SYSTEM_ID,
    SharedCallbacks, validate_provider_instruments,
};
use crate::{
    ProviderEnvironment, ProviderSessionEvent, RetryDisposition, RithmicSessionLimits,
    SessionGeneration,
};
use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    num::NonZeroUsize,
    sync::{Arc, mpsc::TrySendError},
    time::Duration,
};

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
    pub(super) application_name: String,
    pub(super) application_version: String,
    pub(super) session_limits: RithmicSessionLimits,
    pub(super) message_silence_timeout: Duration,
    pub(super) instruments: Vec<RithmicProviderInstrument>,
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

    pub(super) fn silence_evidence_fault(
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

pub(super) fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAXIMUM_IDENTITY_BYTES
        && !value.chars().any(char::is_control)
}

/// Item, retained-byte, and canonical depth bounds for adapter callbacks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicCallbackLimits {
    pub(super) event_capacity: NonZeroUsize,
    pub(super) byte_capacity: NonZeroUsize,
    pub(super) maximum_depth: NonZeroUsize,
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
    pub(super) callbacks: Arc<SharedCallbacks>,
}

/// Non-secret silence-detector timing negotiated for one exact session generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicSessionTiming {
    pub(super) generation: SessionGeneration,
    pub(super) heartbeat_interval: Duration,
    pub(super) response_timeout: Duration,
    pub(super) message_silence_timeout: Duration,
}

impl RithmicSessionTiming {
    pub(super) fn try_new(
        generation: SessionGeneration,
        heartbeat_interval: Duration,
        response_timeout: Duration,
        message_silence_timeout: Duration,
    ) -> Option<Self> {
        if heartbeat_interval.is_zero()
            || heartbeat_interval > MAXIMUM_SILENCE_TIMEOUT
            || response_timeout.is_zero()
            || response_timeout > MAXIMUM_SILENCE_TIMEOUT
            || message_silence_timeout.is_zero()
            || message_silence_timeout > MAXIMUM_SILENCE_TIMEOUT
        {
            return None;
        }
        Some(Self {
            generation,
            heartbeat_interval,
            response_timeout,
            message_silence_timeout,
        })
    }

    #[must_use]
    pub const fn generation(self) -> SessionGeneration {
        self.generation
    }

    #[must_use]
    pub const fn heartbeat_interval(self) -> Duration {
        self.heartbeat_interval
    }

    #[must_use]
    pub const fn response_timeout(self) -> Duration {
        self.response_timeout
    }

    #[must_use]
    pub const fn message_silence_timeout(self) -> Duration {
        self.message_silence_timeout
    }
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

    /// Returns the immutable timing snapshot for the exact active or most recently stopped
    /// session generation.
    #[must_use]
    pub fn session_timing(&self, generation: SessionGeneration) -> Option<RithmicSessionTiming> {
        self.callbacks
            .timing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .copied()
            .filter(|timing| timing.generation == generation)
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

    /// Enqueues one bounded product-code listing for the exact active session generation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a missing/stale session or a full command queue.
    pub fn list_products(
        &self,
        generation: SessionGeneration,
        request: RithmicProductListing,
    ) -> Result<(), RithmicProviderCommandError> {
        self.send_command(generation, RithmicSessionCommand::ListProducts(request))
    }

    /// Enqueues one read-only selection from a retained completed symbol search.
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

    /// Replaces the complete bounded read-only subscription set without
    /// replacing the authenticated provider session.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for invalid demand, a missing/stale session,
    /// or a full command queue.
    pub fn replace_subscriptions(
        &self,
        generation: SessionGeneration,
        instruments: Vec<RithmicProviderInstrument>,
    ) -> Result<(), RithmicProviderCommandError> {
        validate_provider_instruments(&instruments)?;
        self.send_command(
            generation,
            RithmicSessionCommand::ReplaceSubscriptions(instruments),
        )
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
        write!(formatter, "Rithmic provider driver failed: {self:?}")
    }
}

impl Error for RithmicProviderDriverError {}
