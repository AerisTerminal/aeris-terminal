//! Worker-owned desktop provider session lifecycle and publication fencing.
//!
//! The runtime loads opaque credentials from the operating-system vault for each
//! connection attempt, bounds all semantic output, fences callbacks by a local
//! generation, and exposes only shared immutable market publications. It has no
//! cloud client or payload-upload boundary.

use axiusflow_application::MarketStreamPublication;
pub use axiusflow_platform_runtime::NetworkEvent;
use axiusflow_platform_runtime::{CredentialVault, PowerEvent};
use core::fmt;
use std::{
    collections::VecDeque,
    error::Error,
    marker::PhantomData,
    num::{NonZeroU64, NonZeroUsize},
    rc::Rc,
    sync::Arc,
    thread::{self, ThreadId},
};
use zeroize::Zeroize;

mod coinbase_driver;
mod market_worker;

pub use coinbase_driver::{
    CoinbaseProviderDriver, CoinbaseProviderDriverError, CoinbaseProviderEvent,
    CoinbaseProviderEvents, CoinbaseProviderInvalidReason,
};
pub use market_worker::{
    DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopMarketWorkerError,
    HistoryCompletionBinding, HistoryCompletionInstall,
};

struct CredentialBytes(Vec<u8>);

impl CredentialBytes {
    fn as_slice(&self) -> &[u8] {
        &self.0
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

impl Drop for CredentialBytes {
    fn drop(&mut self) {
        self.0.as_mut_slice().zeroize();
    }
}

/// Monotonic identity for one provider session attempt.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SessionGeneration(NonZeroU64);

impl SessionGeneration {
    /// Returns the nonzero generation value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Trigger for a new provider session attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectTrigger {
    Initial,
    Retry,
    Resume,
    NetworkRestored,
}

/// Coarse, non-secret reason that a fresh session is required.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryReason {
    CredentialUnavailable,
    ProviderFailure,
    TransportInvalid,
    SemanticQueueOverflow,
}

/// Observable provider lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopProviderState {
    Disconnected,
    Connecting {
        generation: SessionGeneration,
        trigger: ConnectTrigger,
    },
    Streaming {
        generation: SessionGeneration,
    },
    RecoveryRequired {
        generation: Option<SessionGeneration>,
        reason: RecoveryReason,
    },
    StopUnconfirmed {
        generation: SessionGeneration,
        recovery: Option<RecoveryReason>,
    },
    Suspended,
    NetworkUnavailable,
    Stopped,
}

/// Bounded semantic output for the desktop application worker.
#[derive(Clone, Eq, PartialEq)]
pub enum DesktopProviderEvent {
    SessionStarting {
        generation: SessionGeneration,
        trigger: ConnectTrigger,
    },
    SessionEstablished {
        generation: SessionGeneration,
    },
    Publication {
        generation: SessionGeneration,
        publication: Arc<MarketStreamPublication>,
    },
    RecoveryRequired {
        generation: Option<SessionGeneration>,
        reason: RecoveryReason,
    },
    StopUnconfirmed {
        generation: SessionGeneration,
    },
    Suspended,
    NetworkUnavailable,
    Stopped,
}

impl fmt::Debug for DesktopProviderEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionStarting {
                generation,
                trigger,
            } => formatter
                .debug_struct("SessionStarting")
                .field("generation", generation)
                .field("trigger", trigger)
                .finish(),
            Self::SessionEstablished { generation } => formatter
                .debug_struct("SessionEstablished")
                .field("generation", generation)
                .finish(),
            Self::Publication { generation, .. } => formatter
                .debug_struct("Publication")
                .field("generation", generation)
                .field("publication", &"[REDACTED]")
                .finish(),
            Self::RecoveryRequired { generation, reason } => formatter
                .debug_struct("RecoveryRequired")
                .field("generation", generation)
                .field("reason", reason)
                .finish(),
            Self::StopUnconfirmed { generation } => formatter
                .debug_struct("StopUnconfirmed")
                .field("generation", generation)
                .finish(),
            Self::Suspended => formatter.write_str("Suspended"),
            Self::NetworkUnavailable => formatter.write_str("NetworkUnavailable"),
            Self::Stopped => formatter.write_str("Stopped"),
        }
    }
}

/// Redacted, bounded runtime counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DesktopProviderMetrics {
    pub connection_attempts: u64,
    pub established_sessions: u64,
    pub publications: u64,
    pub stale_callbacks: u64,
    pub dropped_events: u64,
    pub provider_stop_failures: u64,
}

/// Explicit memory limits for one provider lifecycle owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopProviderConfig {
    event_capacity: NonZeroUsize,
    maximum_credential_bytes: NonZeroUsize,
}

impl DesktopProviderConfig {
    /// Creates bounded semantic-event and credential limits.
    #[must_use]
    pub const fn new(event_capacity: NonZeroUsize, maximum_credential_bytes: NonZeroUsize) -> Self {
        Self {
            event_capacity,
            maximum_credential_bytes,
        }
    }
}

/// Direct provider session boundary driven from one desktop worker thread.
pub trait ProviderSessionDriver {
    type Error;

    /// Declares whether this provider session consumes secret credentials.
    #[must_use]
    fn credential_requirement(&self) -> ProviderCredentialRequirement {
        ProviderCredentialRequirement::Required
    }

    /// Starts one fresh provider session using borrowed, opaque vault bytes.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific error when the session cannot start.
    fn start_session(
        &mut self,
        generation: SessionGeneration,
        credentials: &[u8],
    ) -> Result<(), Self::Error>;

    /// Stops one generation. Delayed callbacks may still arrive and are fenced.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific error when shutdown cannot be confirmed.
    fn stop_session(&mut self, generation: SessionGeneration) -> Result<(), Self::Error>;
}

/// Provider credential policy used before starting a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCredentialRequirement {
    Required,
    Public,
}

/// Redacted failure classes returned by the desktop runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopProviderError {
    InvalidCredentialKey,
    WorkerThreadMismatch,
    InvalidTransition,
    GenerationExhausted,
    CredentialVaultUnavailable,
    CredentialMissing,
    CredentialTooLarge { requested: usize, maximum: usize },
    ProviderUnavailable,
    StaleGeneration,
    EventQueueFull { maximum: usize },
}

impl fmt::Display for DesktopProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "desktop provider runtime failed: {self:?}")
    }
}

impl Error for DesktopProviderError {}

/// Single-writer desktop provider lifecycle owner.
pub struct DesktopProviderRuntime<V, D: ProviderSessionDriver> {
    owner_thread: ThreadId,
    vault: V,
    driver: D,
    credential_key: String,
    config: DesktopProviderConfig,
    state: DesktopProviderState,
    last_generation: u64,
    events: VecDeque<DesktopProviderEvent>,
    current_publication: Option<Arc<MarketStreamPublication>>,
    metrics: DesktopProviderMetrics,
    connection_desired: bool,
    suspended: bool,
    network_available: bool,
    _not_send: PhantomData<Rc<()>>,
}

impl<V, D: ProviderSessionDriver> fmt::Debug for DesktopProviderRuntime<V, D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopProviderRuntime")
            .field("state", &self.state)
            .field("event_capacity", &self.config.event_capacity)
            .field("queued_events", &self.events.len())
            .field(
                "has_current_publication",
                &self.current_publication.is_some(),
            )
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

impl<V, D> DesktopProviderRuntime<V, D>
where
    V: CredentialVault,
    D: ProviderSessionDriver,
{
    /// Creates a worker-owned provider runtime without reading credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the non-secret vault key is empty or contains a
    /// control character.
    pub fn try_new(
        vault: V,
        driver: D,
        credential_key: impl Into<String>,
        config: DesktopProviderConfig,
    ) -> Result<Self, DesktopProviderError> {
        let credential_key = credential_key.into();
        if credential_key.is_empty() || credential_key.chars().any(char::is_control) {
            return Err(DesktopProviderError::InvalidCredentialKey);
        }
        Ok(Self {
            owner_thread: thread::current().id(),
            vault,
            driver,
            credential_key,
            config,
            state: DesktopProviderState::Disconnected,
            last_generation: 0,
            events: VecDeque::with_capacity(config.event_capacity.get()),
            current_publication: None,
            metrics: DesktopProviderMetrics::default(),
            connection_desired: false,
            suspended: false,
            network_available: true,
            _not_send: PhantomData,
        })
    }

    /// Returns the redacted lifecycle state.
    ///
    /// # Errors
    ///
    /// Returns an error when called outside the owning worker thread.
    pub fn state(&self) -> Result<DesktopProviderState, DesktopProviderError> {
        self.ensure_owner()?;
        Ok(self.state)
    }

    /// Returns redacted bounded counters.
    ///
    /// # Errors
    ///
    /// Returns an error when called outside the owning worker thread.
    pub fn metrics(&self) -> Result<DesktopProviderMetrics, DesktopProviderError> {
        self.ensure_owner()?;
        Ok(self.metrics)
    }

    /// Returns the last accepted immutable publication.
    ///
    /// # Errors
    ///
    /// Returns an error when called outside the owning worker thread.
    pub fn current_publication(
        &self,
    ) -> Result<Option<Arc<MarketStreamPublication>>, DesktopProviderError> {
        self.ensure_owner()?;
        Ok(self.current_publication.clone())
    }

    /// Starts a fresh direct-provider session with vault-loaded credentials.
    ///
    /// Credential bytes are bounded, borrowed only for the provider call, and
    /// zeroized before this function returns.
    ///
    /// # Errors
    ///
    /// Returns a redacted vault, credential, provider, transition, generation,
    /// ownership, or queue-bound failure.
    pub fn connect(
        &mut self,
        trigger: ConnectTrigger,
    ) -> Result<SessionGeneration, DesktopProviderError> {
        self.ensure_owner()?;
        if self.suspended || !self.network_available {
            return Err(DesktopProviderError::InvalidTransition);
        }
        let valid_transition = matches!(
            (self.state, trigger),
            (
                DesktopProviderState::Disconnected,
                ConnectTrigger::Initial | ConnectTrigger::Retry
            ) | (
                DesktopProviderState::RecoveryRequired { .. },
                ConnectTrigger::Retry
            ) | (DesktopProviderState::Suspended, ConnectTrigger::Resume)
                | (
                    DesktopProviderState::NetworkUnavailable,
                    ConnectTrigger::NetworkRestored
                )
        );
        if !valid_transition {
            return Err(DesktopProviderError::InvalidTransition);
        }
        self.connection_desired = true;
        let generation_value = self
            .last_generation
            .checked_add(1)
            .ok_or(DesktopProviderError::GenerationExhausted)?;
        let generation = SessionGeneration(
            NonZeroU64::new(generation_value).ok_or(DesktopProviderError::GenerationExhausted)?,
        );
        let credentials = match self.driver.credential_requirement() {
            ProviderCredentialRequirement::Required => match self.load_credentials() {
                Ok(credentials) => credentials,
                Err(error) => {
                    self.state = DesktopProviderState::RecoveryRequired {
                        generation: None,
                        reason: RecoveryReason::CredentialUnavailable,
                    };
                    self.record_event(DesktopProviderEvent::RecoveryRequired {
                        generation: None,
                        reason: RecoveryReason::CredentialUnavailable,
                    })?;
                    return Err(error);
                }
            },
            ProviderCredentialRequirement::Public => CredentialBytes(Vec::new()),
        };
        self.last_generation = generation_value;
        self.metrics.connection_attempts = self.metrics.connection_attempts.saturating_add(1);
        self.state = DesktopProviderState::Connecting {
            generation,
            trigger,
        };
        if self
            .driver
            .start_session(generation, credentials.as_slice())
            .is_err()
        {
            self.state = DesktopProviderState::RecoveryRequired {
                generation: Some(generation),
                reason: RecoveryReason::ProviderFailure,
            };
            self.record_event(DesktopProviderEvent::RecoveryRequired {
                generation: Some(generation),
                reason: RecoveryReason::ProviderFailure,
            })?;
            return Err(DesktopProviderError::ProviderUnavailable);
        }
        if let Err(error) = self.record_event(DesktopProviderEvent::SessionStarting {
            generation,
            trigger,
        }) {
            self.latch_queue_overflow(generation);
            return Err(error);
        }
        Ok(generation)
    }

    /// Accepts provider confirmation for the active generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale callback, invalid transition, wrong thread,
    /// or full semantic queue.
    pub fn session_established(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        match self.state {
            DesktopProviderState::Connecting {
                generation: active, ..
            } if active == generation => {}
            _ if self.active_generation() != Some(generation) => return self.reject_stale(),
            _ => return Err(DesktopProviderError::InvalidTransition),
        }
        if let Err(error) =
            self.record_event(DesktopProviderEvent::SessionEstablished { generation })
        {
            self.latch_queue_overflow(generation);
            return Err(error);
        }
        self.state = DesktopProviderState::Streaming { generation };
        self.metrics.established_sessions = self.metrics.established_sessions.saturating_add(1);
        Ok(())
    }

    /// Publishes one immutable model generation only for the active session.
    ///
    /// # Errors
    ///
    /// Returns an error for stale callbacks, invalid lifecycle state, wrong
    /// thread, or a full semantic queue. Queue overflow preserves the prior
    /// immutable publication and latches recovery.
    pub fn publish(
        &mut self,
        generation: SessionGeneration,
        publication: MarketStreamPublication,
    ) -> Result<Arc<MarketStreamPublication>, DesktopProviderError> {
        self.ensure_owner()?;
        match self.state {
            DesktopProviderState::Streaming { generation: active } if active == generation => {}
            _ if self.active_generation() != Some(generation) => return self.reject_stale(),
            _ => return Err(DesktopProviderError::InvalidTransition),
        }
        if self.events.len() >= self.config.event_capacity.get() {
            self.metrics.dropped_events = self.metrics.dropped_events.saturating_add(1);
            self.latch_queue_overflow(generation);
            return Err(DesktopProviderError::EventQueueFull {
                maximum: self.config.event_capacity.get(),
            });
        }
        let publication = Arc::new(publication);
        self.events.push_back(DesktopProviderEvent::Publication {
            generation,
            publication: publication.clone(),
        });
        self.current_publication = Some(publication.clone());
        self.metrics.publications = self.metrics.publications.saturating_add(1);
        Ok(publication)
    }

    /// Invalidates the active provider stream and requires a fresh generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale callback, invalid lifecycle state, wrong
    /// thread, provider stop failure, or a full semantic queue.
    pub fn session_invalid(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopProviderError> {
        self.session_invalid_with_reason(generation, RecoveryReason::TransportInvalid)
    }

    /// Invalidates a generation whose bounded provider callback queue overflowed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale callback, invalid lifecycle state, wrong
    /// thread, provider stop failure, or a full semantic queue.
    pub fn session_callback_queue_overflow(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopProviderError> {
        self.session_invalid_with_reason(generation, RecoveryReason::SemanticQueueOverflow)
    }

    fn session_invalid_with_reason(
        &mut self,
        generation: SessionGeneration,
        reason: RecoveryReason,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        if self.active_generation() != Some(generation) {
            return self.reject_stale();
        }
        self.state = DesktopProviderState::RecoveryRequired {
            generation: Some(generation),
            reason,
        };
        let stop_failed = self.driver.stop_session(generation).is_err();
        if stop_failed {
            self.metrics.provider_stop_failures =
                self.metrics.provider_stop_failures.saturating_add(1);
            self.state = DesktopProviderState::StopUnconfirmed {
                generation,
                recovery: Some(reason),
            };
        }
        if stop_failed {
            self.record_event(DesktopProviderEvent::StopUnconfirmed { generation })?;
            return Err(DesktopProviderError::ProviderUnavailable);
        }
        self.record_event(DesktopProviderEvent::RecoveryRequired {
            generation: Some(generation),
            reason,
        })?;
        Ok(())
    }

    /// Applies one native suspend or resume transition.
    ///
    /// Resume starts a fresh generation and reloads credentials from the vault.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle, provider, vault, ownership, or queue error.
    pub fn handle_power_event(
        &mut self,
        event: PowerEvent,
    ) -> Result<Option<SessionGeneration>, DesktopProviderError> {
        self.ensure_owner()?;
        match event {
            PowerEvent::Suspending => {
                if self.suspended || self.state == DesktopProviderState::Stopped {
                    return Err(DesktopProviderError::InvalidTransition);
                }
                self.suspended = true;
                if matches!(self.state, DesktopProviderState::StopUnconfirmed { .. }) {
                    return Ok(None);
                }
                self.fence_into(DesktopProviderState::Suspended, None)?;
                self.record_event(DesktopProviderEvent::Suspended)?;
                Ok(None)
            }
            PowerEvent::Resumed => {
                if !self.suspended || self.state == DesktopProviderState::Stopped {
                    return Err(DesktopProviderError::InvalidTransition);
                }
                self.suspended = false;
                if matches!(self.state, DesktopProviderState::StopUnconfirmed { .. }) {
                    return Ok(None);
                }
                if !self.connection_desired {
                    self.state = if self.network_available {
                        DesktopProviderState::Disconnected
                    } else {
                        DesktopProviderState::NetworkUnavailable
                    };
                    return Ok(None);
                }
                if !self.network_available {
                    self.state = DesktopProviderState::NetworkUnavailable;
                    return Ok(None);
                }
                self.state = DesktopProviderState::Suspended;
                self.connect(ConnectTrigger::Resume).map(Some)
            }
        }
    }

    /// Applies one native network availability transition.
    ///
    /// Restoration starts a fresh generation and reloads credentials from the vault.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle, provider, vault, ownership, or queue error.
    pub fn handle_network_event(
        &mut self,
        event: NetworkEvent,
    ) -> Result<Option<SessionGeneration>, DesktopProviderError> {
        self.ensure_owner()?;
        match event {
            NetworkEvent::Unavailable => {
                if !self.network_available || self.state == DesktopProviderState::Stopped {
                    return Err(DesktopProviderError::InvalidTransition);
                }
                self.network_available = false;
                if matches!(self.state, DesktopProviderState::StopUnconfirmed { .. }) {
                    return Ok(None);
                }
                self.fence_into(DesktopProviderState::NetworkUnavailable, None)?;
                self.record_event(DesktopProviderEvent::NetworkUnavailable)?;
                Ok(None)
            }
            NetworkEvent::Available => {
                if self.network_available || self.state == DesktopProviderState::Stopped {
                    return Err(DesktopProviderError::InvalidTransition);
                }
                self.network_available = true;
                if matches!(self.state, DesktopProviderState::StopUnconfirmed { .. }) {
                    return Ok(None);
                }
                if !self.connection_desired {
                    self.state = if self.suspended {
                        DesktopProviderState::Suspended
                    } else {
                        DesktopProviderState::Disconnected
                    };
                    return Ok(None);
                }
                if self.suspended {
                    self.state = DesktopProviderState::Suspended;
                    return Ok(None);
                }
                self.connect(ConnectTrigger::NetworkRestored).map(Some)
            }
        }
    }

    /// Retries a provider stop that previously could not be confirmed.
    ///
    /// A new session remains forbidden until this succeeds. The runtime then
    /// returns to the recovery or environmental state that originally required
    /// the stop.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread use, an invalid state, another provider
    /// stop failure, or a full semantic queue.
    pub fn retry_unconfirmed_stop(
        &mut self,
    ) -> Result<Option<SessionGeneration>, DesktopProviderError> {
        self.ensure_owner()?;
        let DesktopProviderState::StopUnconfirmed {
            generation,
            recovery,
        } = self.state
        else {
            return Err(DesktopProviderError::InvalidTransition);
        };
        if self.driver.stop_session(generation).is_err() {
            self.metrics.provider_stop_failures =
                self.metrics.provider_stop_failures.saturating_add(1);
            return Err(DesktopProviderError::ProviderUnavailable);
        }
        if !self.connection_desired {
            self.state = DesktopProviderState::Stopped;
            self.record_event(DesktopProviderEvent::Stopped)?;
            return Ok(None);
        }
        if self.suspended {
            self.state = DesktopProviderState::Suspended;
            self.record_event(DesktopProviderEvent::Suspended)?;
            return Ok(None);
        }
        if !self.network_available {
            self.state = DesktopProviderState::NetworkUnavailable;
            self.record_event(DesktopProviderEvent::NetworkUnavailable)?;
            return Ok(None);
        }
        let reason = recovery.unwrap_or(RecoveryReason::ProviderFailure);
        self.state = DesktopProviderState::RecoveryRequired {
            generation: Some(generation),
            reason,
        };
        self.connect(ConnectTrigger::Retry).map(Some)
    }

    /// Receives at most one ready semantic event without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error when called outside the owning worker thread.
    pub fn try_recv_event(&mut self) -> Result<Option<DesktopProviderEvent>, DesktopProviderError> {
        self.ensure_owner()?;
        Ok(self.events.pop_front())
    }

    /// Fences the active generation and permanently stops this runtime.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread use, repeated shutdown, provider stop
    /// failure, or a full semantic queue.
    pub fn stop(&mut self) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        if self.state == DesktopProviderState::Stopped {
            return Err(DesktopProviderError::InvalidTransition);
        }
        self.connection_desired = false;
        if matches!(self.state, DesktopProviderState::StopUnconfirmed { .. }) {
            return self.retry_unconfirmed_stop().map(|_| ());
        }
        self.fence_into(DesktopProviderState::Stopped, None)?;
        self.record_event(DesktopProviderEvent::Stopped)
    }

    fn load_credentials(&self) -> Result<CredentialBytes, DesktopProviderError> {
        let credentials = self
            .vault
            .load(&self.credential_key)
            .map_err(|_| DesktopProviderError::CredentialVaultUnavailable)?
            .ok_or(DesktopProviderError::CredentialMissing)?;
        let credentials = CredentialBytes(credentials);
        if credentials.len() > self.config.maximum_credential_bytes.get() {
            return Err(DesktopProviderError::CredentialTooLarge {
                requested: credentials.len(),
                maximum: self.config.maximum_credential_bytes.get(),
            });
        }
        Ok(credentials)
    }

    fn active_generation(&self) -> Option<SessionGeneration> {
        match self.state {
            DesktopProviderState::Connecting { generation, .. }
            | DesktopProviderState::Streaming { generation } => Some(generation),
            DesktopProviderState::Disconnected
            | DesktopProviderState::RecoveryRequired { .. }
            | DesktopProviderState::StopUnconfirmed { .. }
            | DesktopProviderState::Suspended
            | DesktopProviderState::NetworkUnavailable
            | DesktopProviderState::Stopped => None,
        }
    }

    fn fence_into(
        &mut self,
        state: DesktopProviderState,
        recovery: Option<RecoveryReason>,
    ) -> Result<(), DesktopProviderError> {
        let generation = self.active_generation();
        self.state = state;
        if let Some(generation) = generation
            && self.driver.stop_session(generation).is_err()
        {
            self.metrics.provider_stop_failures =
                self.metrics.provider_stop_failures.saturating_add(1);
            self.state = DesktopProviderState::StopUnconfirmed {
                generation,
                recovery,
            };
            return Err(DesktopProviderError::ProviderUnavailable);
        }
        Ok(())
    }

    fn record_event(&mut self, event: DesktopProviderEvent) -> Result<(), DesktopProviderError> {
        if self.events.len() >= self.config.event_capacity.get() {
            self.metrics.dropped_events = self.metrics.dropped_events.saturating_add(1);
            return Err(DesktopProviderError::EventQueueFull {
                maximum: self.config.event_capacity.get(),
            });
        }
        self.events.push_back(event);
        Ok(())
    }

    fn latch_queue_overflow(&mut self, generation: SessionGeneration) {
        self.state = DesktopProviderState::RecoveryRequired {
            generation: Some(generation),
            reason: RecoveryReason::SemanticQueueOverflow,
        };
        if self.driver.stop_session(generation).is_err() {
            self.metrics.provider_stop_failures =
                self.metrics.provider_stop_failures.saturating_add(1);
            self.state = DesktopProviderState::StopUnconfirmed {
                generation,
                recovery: Some(RecoveryReason::SemanticQueueOverflow),
            };
        }
    }

    fn reject_stale<T>(&mut self) -> Result<T, DesktopProviderError> {
        self.metrics.stale_callbacks = self.metrics.stale_callbacks.saturating_add(1);
        Err(DesktopProviderError::StaleGeneration)
    }

    fn ensure_owner(&self) -> Result<(), DesktopProviderError> {
        if thread::current().id() != self.owner_thread {
            return Err(DesktopProviderError::WorkerThreadMismatch);
        }
        Ok(())
    }
}

impl<V, D: ProviderSessionDriver> Drop for DesktopProviderRuntime<V, D> {
    fn drop(&mut self) {
        let generation = match self.state {
            DesktopProviderState::Connecting { generation, .. }
            | DesktopProviderState::Streaming { generation }
            | DesktopProviderState::StopUnconfirmed { generation, .. } => Some(generation),
            DesktopProviderState::Disconnected
            | DesktopProviderState::RecoveryRequired { .. }
            | DesktopProviderState::Suspended
            | DesktopProviderState::NetworkUnavailable
            | DesktopProviderState::Stopped => None,
        };
        if let Some(generation) = generation {
            let _ = self.driver.stop_session(generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectTrigger, DesktopProviderConfig, DesktopProviderError, DesktopProviderEvent,
        DesktopProviderRuntime, DesktopProviderState, NetworkEvent, ProviderSessionDriver,
        RecoveryReason, SessionGeneration,
    };
    use axiusflow_application::{
        EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
        MarketBarReplayPort, MarketStreamPublication, ReplayStreamUpdate,
    };
    use axiusflow_platform_runtime::{CredentialVault, PowerEvent};
    use std::{
        num::NonZeroUsize,
        sync::{Arc, Mutex},
    };

    #[derive(Clone)]
    struct MemoryVault {
        secret: Option<Vec<u8>>,
        fail_load: bool,
    }

    impl CredentialVault for MemoryVault {
        type Error = ();

        fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            if self.fail_load {
                Err(())
            } else {
                Ok(self.secret.clone())
            }
        }

        fn delete(&self, _key: &str) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct DriverState {
        starts: Vec<(SessionGeneration, Vec<u8>)>,
        stops: Vec<SessionGeneration>,
        fail_start: bool,
        fail_stop: bool,
    }

    #[derive(Clone, Debug, Default)]
    struct RecordingDriver(Arc<Mutex<DriverState>>);

    impl ProviderSessionDriver for RecordingDriver {
        type Error = ();

        fn start_session(
            &mut self,
            generation: SessionGeneration,
            credentials: &[u8],
        ) -> Result<(), Self::Error> {
            let mut state = self.0.lock().expect("driver state lock is available");
            state.starts.push((generation, credentials.to_vec()));
            if state.fail_start { Err(()) } else { Ok(()) }
        }

        fn stop_session(&mut self, generation: SessionGeneration) -> Result<(), Self::Error> {
            let mut state = self.0.lock().expect("driver state lock is available");
            state.stops.push(generation);
            if state.fail_stop { Err(()) } else { Ok(()) }
        }
    }

    fn config(event_capacity: usize, maximum_credential_bytes: usize) -> DesktopProviderConfig {
        DesktopProviderConfig::new(
            NonZeroUsize::new(event_capacity).expect("test event capacity is nonzero"),
            NonZeroUsize::new(maximum_credential_bytes)
                .expect("test credential capacity is nonzero"),
        )
    }

    fn runtime(
        event_capacity: usize,
    ) -> (
        DesktopProviderRuntime<MemoryVault, RecordingDriver>,
        Arc<Mutex<DriverState>>,
    ) {
        let driver = RecordingDriver::default();
        let state = driver.0.clone();
        let runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(event_capacity, 64),
        )
        .expect("valid runtime starts disconnected");
        (runtime, state)
    }

    fn publication() -> MarketStreamPublication {
        let source = EmbeddedReplaySource;
        let snapshot = source
            .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
            .expect("embedded snapshot is valid");
        let update = ReplayStreamUpdate::Snapshot(snapshot);
        let mut model = MarketBarClientModel::new(
            NonZeroUsize::new(8).expect("test model capacity is nonzero"),
        );
        let MarketBarModelOutcome::Published(generation) = model
            .apply_update(update.clone())
            .expect("snapshot publishes")
        else {
            panic!("snapshot must publish");
        };
        MarketStreamPublication::try_new("direct-provider".to_string(), update, generation)
            .expect("publication evidence agrees")
    }

    fn drain(runtime: &mut DesktopProviderRuntime<MemoryVault, RecordingDriver>) {
        while runtime
            .try_recv_event()
            .expect("event receive remains available")
            .is_some()
        {}
    }

    #[test]
    fn vault_credentials_are_bounded_and_errors_are_redacted() {
        let driver = RecordingDriver::default();
        let mut missing = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: None,
                fail_load: false,
            },
            driver.clone(),
            "provider-session",
            config(4, 8),
        )
        .expect("runtime configuration is valid");
        assert_eq!(
            missing.connect(ConnectTrigger::Initial),
            Err(DesktopProviderError::CredentialMissing)
        );

        let mut oversized = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"secret-value".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(4, 8),
        )
        .expect("runtime configuration is valid");
        assert_eq!(
            oversized.connect(ConnectTrigger::Initial),
            Err(DesktopProviderError::CredentialTooLarge {
                requested: 12,
                maximum: 8,
            })
        );
        let debug = format!("{oversized:?}");
        assert!(!debug.contains("provider-session"));
        assert!(!debug.contains("secret-value"));
    }

    #[test]
    fn network_and_power_recovery_create_fresh_fenced_generations() {
        let (mut runtime, driver) = runtime(16);
        let first = runtime
            .connect(ConnectTrigger::Initial)
            .expect("initial provider session starts");
        runtime
            .session_established(first)
            .expect("first session establishes");
        runtime
            .handle_network_event(NetworkEvent::Unavailable)
            .expect("network loss fences first session");
        let second = runtime
            .handle_network_event(NetworkEvent::Available)
            .expect("network restoration starts a session")
            .expect("restoration returns a generation");
        assert!(second > first);
        assert_eq!(
            runtime.publish(first, publication()),
            Err(DesktopProviderError::StaleGeneration)
        );
        runtime
            .session_established(second)
            .expect("second session establishes");
        runtime
            .handle_power_event(PowerEvent::Suspending)
            .expect("suspend fences second session");
        let third = runtime
            .handle_power_event(PowerEvent::Resumed)
            .expect("resume starts a session")
            .expect("resume returns a generation");
        assert!(third > second);
        assert_eq!(
            runtime.session_established(second),
            Err(DesktopProviderError::StaleGeneration)
        );
        assert_eq!(
            driver.lock().expect("driver state lock is available").stops,
            vec![first, second]
        );
        assert_eq!(
            runtime
                .metrics()
                .expect("metrics are available")
                .stale_callbacks,
            2
        );
    }

    #[test]
    fn environmental_cycles_do_not_connect_an_idle_runtime() {
        let (mut runtime, driver) = runtime(8);
        assert_eq!(
            runtime
                .handle_power_event(PowerEvent::Suspending)
                .expect("idle suspend is recorded"),
            None
        );
        assert_eq!(
            runtime
                .handle_power_event(PowerEvent::Resumed)
                .expect("idle resume is recorded"),
            None
        );
        assert_eq!(
            runtime
                .handle_network_event(NetworkEvent::Unavailable)
                .expect("idle network loss is recorded"),
            None
        );
        assert_eq!(
            runtime
                .handle_network_event(NetworkEvent::Available)
                .expect("idle network restoration is recorded"),
            None
        );
        assert!(
            driver
                .lock()
                .expect("driver state lock is available")
                .starts
                .is_empty()
        );
        assert_eq!(
            runtime.state().expect("state is available"),
            DesktopProviderState::Disconnected
        );
    }

    #[test]
    fn public_connect_cannot_bypass_environmental_fences() {
        let (mut runtime, driver) = runtime(8);
        runtime
            .handle_power_event(PowerEvent::Suspending)
            .expect("idle suspend is recorded");
        assert_eq!(
            runtime.connect(ConnectTrigger::Resume),
            Err(DesktopProviderError::InvalidTransition)
        );
        runtime
            .handle_power_event(PowerEvent::Resumed)
            .expect("idle resume is recorded");
        runtime
            .handle_network_event(NetworkEvent::Unavailable)
            .expect("idle network loss is recorded");
        assert_eq!(
            runtime.connect(ConnectTrigger::NetworkRestored),
            Err(DesktopProviderError::InvalidTransition)
        );
        assert!(
            driver
                .lock()
                .expect("driver state lock is available")
                .starts
                .is_empty()
        );
    }

    #[test]
    fn unconfirmed_stop_blocks_replacement_until_cleanup_succeeds() {
        let (mut runtime, driver) = runtime(1);
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        drain(&mut runtime);
        runtime
            .session_established(generation)
            .expect("session establishes");
        drain(&mut runtime);
        runtime
            .publish(generation, publication())
            .expect("first publication fills the queue");
        driver
            .lock()
            .expect("driver state lock is available")
            .fail_stop = true;
        assert_eq!(
            runtime.publish(generation, publication()),
            Err(DesktopProviderError::EventQueueFull { maximum: 1 })
        );
        assert_eq!(
            runtime.state().expect("state is available"),
            DesktopProviderState::StopUnconfirmed {
                generation,
                recovery: Some(RecoveryReason::SemanticQueueOverflow),
            }
        );
        assert_eq!(
            runtime.connect(ConnectTrigger::Retry),
            Err(DesktopProviderError::InvalidTransition)
        );
        drain(&mut runtime);
        driver
            .lock()
            .expect("driver state lock is available")
            .fail_stop = false;
        let replacement = runtime
            .retry_unconfirmed_stop()
            .expect("cleanup eventually confirms the stop")
            .expect("cleanup starts the desired replacement");
        assert!(replacement > generation);
        assert_eq!(
            runtime.state().expect("state is available"),
            DesktopProviderState::Connecting {
                generation: replacement,
                trigger: ConnectTrigger::Retry,
            }
        );
    }

    #[test]
    fn interleaved_power_and_network_events_reconnect_in_either_order() {
        let (mut runtime, _) = runtime(16);
        let first = runtime
            .connect(ConnectTrigger::Initial)
            .expect("initial session starts");
        runtime
            .session_established(first)
            .expect("initial session establishes");
        runtime
            .handle_power_event(PowerEvent::Suspending)
            .expect("suspend fences the session");
        runtime
            .handle_network_event(NetworkEvent::Unavailable)
            .expect("network loss is retained while suspended");
        assert_eq!(
            runtime
                .handle_power_event(PowerEvent::Resumed)
                .expect("resume is retained while offline"),
            None
        );
        let second = runtime
            .handle_network_event(NetworkEvent::Available)
            .expect("restoration starts the desired session")
            .expect("restoration returns a generation");
        runtime
            .session_established(second)
            .expect("second session establishes");

        runtime
            .handle_power_event(PowerEvent::Suspending)
            .expect("second suspend fences the session");
        runtime
            .handle_network_event(NetworkEvent::Unavailable)
            .expect("second network loss is retained");
        assert_eq!(
            runtime
                .handle_network_event(NetworkEvent::Available)
                .expect("restoration is retained while suspended"),
            None
        );
        let third = runtime
            .handle_power_event(PowerEvent::Resumed)
            .expect("resume starts the desired session")
            .expect("resume returns a generation");
        assert!(first < second && second < third);
    }

    #[test]
    fn restoration_during_unconfirmed_cleanup_is_not_lost() {
        let (mut runtime, driver) = runtime(16);
        let first = runtime
            .connect(ConnectTrigger::Initial)
            .expect("initial session starts");
        runtime
            .session_established(first)
            .expect("initial session establishes");
        driver
            .lock()
            .expect("driver state lock is available")
            .fail_stop = true;
        assert_eq!(
            runtime.handle_network_event(NetworkEvent::Unavailable),
            Err(DesktopProviderError::ProviderUnavailable)
        );
        assert_eq!(
            runtime
                .handle_network_event(NetworkEvent::Available)
                .expect("restoration is recorded during cleanup"),
            None
        );
        driver
            .lock()
            .expect("driver state lock is available")
            .fail_stop = false;
        let second = runtime
            .retry_unconfirmed_stop()
            .expect("cleanup succeeds")
            .expect("recorded restoration starts a replacement");
        assert!(second > first);
    }

    #[test]
    fn publication_queue_overflow_preserves_last_immutable_state() {
        let (mut runtime, driver) = runtime(1);
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        drain(&mut runtime);
        runtime
            .session_established(generation)
            .expect("session establishes");
        drain(&mut runtime);
        let accepted = runtime
            .publish(generation, publication())
            .expect("first publication fits");
        assert_eq!(
            runtime.publish(generation, publication()),
            Err(DesktopProviderError::EventQueueFull { maximum: 1 })
        );
        let retained = runtime
            .current_publication()
            .expect("publication lookup succeeds")
            .expect("accepted publication remains");
        assert!(Arc::ptr_eq(&accepted, &retained));
        assert_eq!(
            runtime.state().expect("state is available"),
            DesktopProviderState::RecoveryRequired {
                generation: Some(generation),
                reason: RecoveryReason::SemanticQueueOverflow,
            }
        );
        assert_eq!(
            runtime
                .metrics()
                .expect("metrics are available")
                .dropped_events,
            1
        );
        assert_eq!(
            driver.lock().expect("driver state lock is available").stops,
            vec![generation]
        );
    }

    #[test]
    fn provider_failure_exposes_only_a_coarse_recovery_class() {
        let (mut runtime, driver) = runtime(4);
        driver
            .lock()
            .expect("driver state lock is available")
            .fail_start = true;
        assert_eq!(
            runtime.connect(ConnectTrigger::Initial),
            Err(DesktopProviderError::ProviderUnavailable)
        );
        assert!(matches!(
            runtime.try_recv_event().expect("event queue is available"),
            Some(DesktopProviderEvent::RecoveryRequired {
                reason: RecoveryReason::ProviderFailure,
                ..
            })
        ));
    }

    #[test]
    fn drop_attempts_to_stop_an_active_generation() {
        let (mut runtime, driver) = runtime(4);
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        drop(runtime);
        assert_eq!(
            driver.lock().expect("driver state lock is available").stops,
            vec![generation]
        );
    }

    #[test]
    fn publication_event_debug_redacts_market_payload_and_subscription() {
        let (mut runtime, _) = runtime(4);
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        drain(&mut runtime);
        runtime
            .session_established(generation)
            .expect("session establishes");
        drain(&mut runtime);
        runtime
            .publish(generation, publication())
            .expect("publication fits");
        let event = runtime
            .try_recv_event()
            .expect("event receive succeeds")
            .expect("publication event exists");
        let debug = format!("{event:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("direct-provider"));
        assert!(!debug.contains("EmbeddedFixture"));
    }
}
