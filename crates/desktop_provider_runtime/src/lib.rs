//! Worker-owned desktop provider session lifecycle and publication fencing.
//!
//! The runtime loads opaque credentials from the operating-system vault for each
//! connection attempt, bounds all semantic output, fences callbacks by a local
//! generation, and exposes only shared immutable market publications. It has no
//! cloud client or payload-upload boundary.

use axiusflow_application::MarketStreamPublication;
use axiusflow_market_data::MarketEvent;
use axiusflow_observability::{
    DiagnosticsQueue, FeedDiagnosticsSnapshot, LatencyTimestampChain, LocalLatencyMetric,
    MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES,
};
pub use axiusflow_platform_runtime::NetworkEvent;
use axiusflow_platform_runtime::{CredentialVault, PowerEvent};
use core::fmt;
use std::{
    collections::VecDeque,
    error::Error,
    marker::PhantomData,
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    rc::Rc,
    sync::Arc,
    thread::{self, ThreadId},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroize;

mod market_worker;
mod provider_diagnostics;
mod session_contract;

pub use market_worker::{
    DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopMarketWorkerError,
    HistoryCompletionBinding, HistoryCompletionInstall,
};
pub use provider_diagnostics::{ProviderFeedDiagnostics, ProviderFeedDiagnosticsError};
pub use session_contract::{
    AuthenticationState, InstrumentDescriptor, MAXIMUM_DISCOVERY_FIELD_BYTES,
    ProviderContractError, ProviderEnvironment, ProviderInvalidationReason, ProviderSessionCommand,
    ProviderSessionEvent, ProviderSubscription,
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
    /// Creates a nonzero session generation for adapter-owned fixtures and callbacks.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopProviderConfig {
    event_capacity: NonZeroUsize,
    maximum_credential_bytes: NonZeroUsize,
    diagnostics: Option<DesktopProviderDiagnosticsConfig>,
}

impl DesktopProviderConfig {
    /// Creates bounded semantic-event and credential limits.
    #[must_use]
    pub const fn new(event_capacity: NonZeroUsize, maximum_credential_bytes: NonZeroUsize) -> Self {
        Self {
            event_capacity,
            maximum_credential_bytes,
            diagnostics: None,
        }
    }

    /// Enables production diagnostics for the selected provider environment.
    ///
    /// # Errors
    ///
    /// Returns an error when the environment identity is unsafe or oversized.
    pub fn with_diagnostics(
        mut self,
        environment: ProviderEnvironment,
        detailed_latency_maximum_nanos: Option<NonZeroU64>,
    ) -> Result<Self, ProviderContractError> {
        environment.validate()?;
        for (field, value) in [
            ("provider_id", environment.provider_id.as_str()),
            ("system_id", environment.system_id.as_str()),
            ("environment", environment.environment.as_str()),
        ] {
            if value.len() > MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES {
                return Err(ProviderContractError::FieldTooLong {
                    field,
                    maximum: MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES,
                });
            }
            if value.chars().any(char::is_control) {
                return Err(ProviderContractError::ControlCharacter(field));
            }
        }
        self.diagnostics = Some(DesktopProviderDiagnosticsConfig {
            environment,
            detailed_latency_maximum_nanos,
        });
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopProviderDiagnosticsConfig {
    environment: ProviderEnvironment,
    detailed_latency_maximum_nanos: Option<NonZeroU64>,
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
    DiagnosticsUnavailable,
    SystemClockUnavailable,
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
    diagnostics: Option<ProviderRuntimeDiagnostics>,
    _not_send: PhantomData<Rc<()>>,
}

struct ProviderRuntimeDiagnostics {
    feed: ProviderFeedDiagnostics,
    epoch: Instant,
}

impl ProviderRuntimeDiagnostics {
    fn monotonic_nanos(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
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
        let diagnostics = config
            .diagnostics
            .as_ref()
            .map(|diagnostics| {
                ProviderFeedDiagnostics::try_new(
                    diagnostics.environment.provider_id.clone(),
                    diagnostics.environment.system_id.clone(),
                    diagnostics.environment.environment.clone(),
                    diagnostics.detailed_latency_maximum_nanos,
                )
                .map(|feed| ProviderRuntimeDiagnostics {
                    feed,
                    epoch: Instant::now(),
                })
                .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
            })
            .transpose()?;
        let event_capacity = config.event_capacity;
        Ok(Self {
            owner_thread: thread::current().id(),
            vault,
            driver,
            credential_key,
            config,
            state: DesktopProviderState::Disconnected,
            last_generation: 0,
            events: VecDeque::with_capacity(event_capacity.get()),
            current_publication: None,
            metrics: DesktopProviderMetrics::default(),
            connection_desired: false,
            suspended: false,
            network_available: true,
            diagnostics,
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

    /// Publishes the current production diagnostics snapshot when its cadence allows.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access, clock failure, or diagnostics failure.
    pub fn try_diagnostics_snapshot(
        &mut self,
    ) -> Result<Option<FeedDiagnosticsSnapshot>, DesktopProviderError> {
        self.ensure_owner()?;
        self.diagnostics_observe_runtime_memory()?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(None);
        };
        let wall_clock_unix_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DesktopProviderError::SystemClockUnavailable)?
            .as_nanos();
        let wall_clock_unix_nanos = i64::try_from(wall_clock_unix_nanos)
            .map_err(|_| DesktopProviderError::SystemClockUnavailable)?;
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .try_snapshot(timestamp, wall_clock_unix_nanos)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    /// Records one live trade callback in production feed diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, stale generation, or diagnostics failure.
    pub fn record_trade_diagnostics(
        &mut self,
        generation: SessionGeneration,
        provider_timestamp_unix_nanos: Option<i64>,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .record_runtime_trade(generation, provider_timestamp_unix_nanos, timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    /// Records one generation-fenced market callback in production diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, stale generation, or diagnostics failure.
    pub fn record_market_event_diagnostics(
        &mut self,
        generation: SessionGeneration,
        event: &MarketEvent,
        message_timestamp_unix_nanos: Option<i64>,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .record_runtime_market_event(generation, event, message_timestamp_unix_nanos, timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    /// Records one live heartbeat callback in production feed diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, stale generation, or diagnostics failure.
    pub fn record_heartbeat_diagnostics(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .record_runtime_heartbeat(generation, timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    /// Observes one fixed-capacity queue in production diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or invalid queue evidence.
    pub fn observe_diagnostics_queue(
        &mut self,
        queue: DiagnosticsQueue,
        current_items: usize,
        item_capacity: usize,
        current_bytes: usize,
        byte_capacity: usize,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        let Some(diagnostics) = &mut self.diagnostics else {
            return Ok(());
        };
        diagnostics
            .feed
            .observe_queue(
                queue,
                current_items,
                item_capacity,
                current_bytes,
                byte_capacity,
            )
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    /// Records one correctly labelled local processing interval.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or invalid timestamp evidence.
    pub fn record_latency_diagnostics(
        &mut self,
        generation: SessionGeneration,
        metric: LocalLatencyMetric,
        chain: &LatencyTimestampChain,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        if self.active_generation() != Some(generation) {
            return self.reject_stale();
        }
        let Some(diagnostics) = &mut self.diagnostics else {
            return Ok(());
        };
        diagnostics
            .feed
            .record_latency_chain(metric, chain)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    /// Records one UI update safely conflated before a frame was submitted.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or diagnostics failure.
    pub fn record_coalesced_ui_update_diagnostics(
        &mut self,
        generation: SessionGeneration,
        count: u64,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        if self.active_generation() != Some(generation) {
            return self.reject_stale();
        }
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .record_coalesced_ui_update(timestamp, count)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
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
                    self.diagnostics_require_recovery(RecoveryReason::CredentialUnavailable)?;
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
        self.diagnostics_begin_session(generation)?;
        if self
            .driver
            .start_session(generation, credentials.as_slice())
            .is_err()
        {
            self.state = DesktopProviderState::RecoveryRequired {
                generation: Some(generation),
                reason: RecoveryReason::ProviderFailure,
            };
            self.diagnostics_require_recovery(RecoveryReason::ProviderFailure)?;
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
            self.latch_queue_overflow(generation, true);
            return Err(error);
        }
        Ok(generation)
    }

    /// Records connection intent and starts now when environmental fences allow.
    ///
    /// Unlike [`Self::connect`], this method can be called while suspended or
    /// offline. The intent is retained and the matching native restoration event
    /// starts a fresh generation.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread use, a stopped or already-active
    /// runtime, or a provider/vault/queue failure while starting immediately.
    pub fn request_connection(
        &mut self,
    ) -> Result<Option<SessionGeneration>, DesktopProviderError> {
        self.ensure_owner()?;
        if matches!(
            self.state,
            DesktopProviderState::Stopped
                | DesktopProviderState::Connecting { .. }
                | DesktopProviderState::Streaming { .. }
        ) {
            return Err(DesktopProviderError::InvalidTransition);
        }
        self.connection_desired = true;
        if self.suspended
            || !self.network_available
            || matches!(self.state, DesktopProviderState::StopUnconfirmed { .. })
        {
            return Ok(None);
        }
        let trigger = if matches!(self.state, DesktopProviderState::RecoveryRequired { .. }) {
            ConnectTrigger::Retry
        } else {
            ConnectTrigger::Initial
        };
        self.connect(trigger).map(Some)
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
            self.latch_queue_overflow(generation, true);
            return Err(error);
        }
        self.state = DesktopProviderState::Streaming { generation };
        self.diagnostics_mark_streaming(generation)?;
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
            self.latch_queue_overflow(generation, false);
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
        self.diagnostics_record_publication(generation)?;
        self.diagnostics_observe_event_queue()?;
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
        self.session_invalid_with_reason(generation, RecoveryReason::TransportInvalid, None)
    }

    /// Invalidates an active generation while retaining the provider's coarse,
    /// non-secret reason in feed diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale callback, invalid lifecycle state, wrong
    /// thread, provider stop failure, or a full semantic queue.
    pub fn session_invalid_for_provider(
        &mut self,
        generation: SessionGeneration,
        reason: ProviderInvalidationReason,
    ) -> Result<(), DesktopProviderError> {
        self.session_invalid_with_reason(generation, RecoveryReason::TransportInvalid, Some(reason))
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
        self.session_invalid_with_reason(generation, RecoveryReason::SemanticQueueOverflow, None)
    }

    fn session_invalid_with_reason(
        &mut self,
        generation: SessionGeneration,
        reason: RecoveryReason,
        provider_reason: Option<ProviderInvalidationReason>,
    ) -> Result<(), DesktopProviderError> {
        self.ensure_owner()?;
        if self.active_generation() != Some(generation) {
            return self.reject_stale();
        }
        self.state = DesktopProviderState::RecoveryRequired {
            generation: Some(generation),
            reason,
        };
        if let Some(provider_reason) = provider_reason {
            self.diagnostics_observe_invalidation(generation, provider_reason)?;
        } else {
            self.diagnostics_require_recovery(reason)?;
        }
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
                let fence_result = self.fence_into(DesktopProviderState::Suspended, None);
                self.diagnostics_mark_disconnected()?;
                fence_result?;
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
                let fence_result = self.fence_into(DesktopProviderState::NetworkUnavailable, None);
                self.diagnostics_mark_disconnected()?;
                fence_result?;
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
            self.diagnostics_mark_stopped()?;
            self.record_event(DesktopProviderEvent::Stopped)?;
            return Ok(None);
        }
        if self.suspended {
            self.state = DesktopProviderState::Suspended;
            self.diagnostics_mark_disconnected()?;
            self.record_event(DesktopProviderEvent::Suspended)?;
            return Ok(None);
        }
        if !self.network_available {
            self.state = DesktopProviderState::NetworkUnavailable;
            self.diagnostics_mark_disconnected()?;
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
        let event = self.events.pop_front();
        self.diagnostics_observe_event_queue()?;
        Ok(event)
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
        let fence_result = self.fence_into(DesktopProviderState::Stopped, None);
        self.diagnostics_mark_stopped()?;
        fence_result?;
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

    fn diagnostics_now(&self) -> Option<u64> {
        self.diagnostics
            .as_ref()
            .map(ProviderRuntimeDiagnostics::monotonic_nanos)
    }

    fn diagnostics_begin_session(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .begin_runtime_session(generation, timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_mark_streaming(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .mark_runtime_streaming(generation, timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_require_recovery(
        &mut self,
        reason: RecoveryReason,
    ) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .require_runtime_recovery(reason, timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_observe_invalidation(
        &mut self,
        generation: SessionGeneration,
        reason: ProviderInvalidationReason,
    ) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .observe_event(
                &ProviderSessionEvent::Invalidated {
                    generation: Some(generation),
                    reason,
                },
                timestamp,
            )
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_require_recorded_queue_recovery(&mut self) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .require_runtime_recovery_after_recorded_overflow(timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_mark_disconnected(&mut self) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .mark_runtime_disconnected(timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_mark_stopped(&mut self) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .mark_runtime_stopped(timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_record_publication(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopProviderError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(DesktopProviderError::DiagnosticsUnavailable)?
            .feed
            .record_publication(generation, timestamp)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_record_stale_callback(&mut self) {
        let Some(timestamp) = self.diagnostics_now() else {
            return;
        };
        if let Some(diagnostics) = &mut self.diagnostics {
            let _ = diagnostics.feed.record_stale_callback(timestamp);
        }
    }

    fn diagnostics_record_semantic_queue_overflow(&mut self) {
        let Some(timestamp) = self.diagnostics_now() else {
            return;
        };
        if let Some(diagnostics) = &mut self.diagnostics {
            let _ = diagnostics.feed.record_semantic_queue_overflow(timestamp);
        }
    }

    fn diagnostics_observe_event_queue(&mut self) -> Result<(), DesktopProviderError> {
        let Some(diagnostics) = &mut self.diagnostics else {
            return Ok(());
        };
        let item_bytes = size_of::<DesktopProviderEvent>();
        diagnostics
            .feed
            .observe_queue(
                DiagnosticsQueue::SemanticEvent,
                self.events.len(),
                self.config.event_capacity.get(),
                self.events.len().saturating_mul(item_bytes),
                self.config.event_capacity.get().saturating_mul(item_bytes),
            )
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
    }

    fn diagnostics_observe_runtime_memory(&mut self) -> Result<(), DesktopProviderError> {
        let Some(diagnostics) = &mut self.diagnostics else {
            return Ok(());
        };
        let fixed_bytes = size_of::<Self>();
        let event_bytes = size_of::<DesktopProviderEvent>();
        let current_bytes =
            fixed_bytes.saturating_add(self.events.len().saturating_mul(event_bytes));
        let bound_bytes = fixed_bytes
            .saturating_add(self.config.event_capacity.get().saturating_mul(event_bytes));
        diagnostics
            .feed
            .observe_runtime_memory(current_bytes, bound_bytes)
            .map_err(|_| DesktopProviderError::DiagnosticsUnavailable)
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
            self.diagnostics_record_semantic_queue_overflow();
            return Err(DesktopProviderError::EventQueueFull {
                maximum: self.config.event_capacity.get(),
            });
        }
        self.events.push_back(event);
        self.diagnostics_observe_event_queue()?;
        Ok(())
    }

    fn latch_queue_overflow(
        &mut self,
        generation: SessionGeneration,
        overflow_already_recorded: bool,
    ) {
        self.state = DesktopProviderState::RecoveryRequired {
            generation: Some(generation),
            reason: RecoveryReason::SemanticQueueOverflow,
        };
        if overflow_already_recorded {
            let _ = self.diagnostics_require_recorded_queue_recovery();
        } else {
            let _ = self.diagnostics_require_recovery(RecoveryReason::SemanticQueueOverflow);
        }
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
        self.diagnostics_record_stale_callback();
        Err(DesktopProviderError::StaleGeneration)
    }

    fn ensure_owner(&self) -> Result<(), DesktopProviderError> {
        if thread::current().id() != self.owner_thread {
            return Err(DesktopProviderError::WorkerThreadMismatch);
        }
        Ok(())
    }

    /// Verifies adapter callback ownership without exposing the driver.
    ///
    /// # Errors
    /// Returns an error for wrong-thread access.
    pub fn driver_matches(
        &self,
        predicate: impl FnOnce(&D) -> bool,
    ) -> Result<bool, DesktopProviderError> {
        self.ensure_owner()?;
        Ok(predicate(&self.driver))
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
        DesktopProviderRuntime, DesktopProviderState, MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES,
        NetworkEvent, ProviderContractError, ProviderEnvironment, ProviderInvalidationReason,
        ProviderSessionDriver, RecoveryReason, SessionGeneration,
    };
    use axiusflow_application::{
        EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
        MarketStreamPublication, ReplayStreamUpdate,
    };
    use axiusflow_observability::{
        DiagnosticsQueue, FeedConnectionState, FeedRecoveryReason, LatencyBoundary,
        LatencyTimestampChain, LocalLatencyMetric,
    };
    use axiusflow_platform_runtime::{CredentialVault, PowerEvent};
    use std::{
        num::{NonZeroU64, NonZeroUsize},
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

    fn diagnostics_environment() -> ProviderEnvironment {
        ProviderEnvironment {
            provider_id: "rithmic".to_string(),
            system_id: "RITHMIC_TEST".to_string(),
            environment: "Test".to_string(),
        }
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

    #[test]
    fn production_runtime_owns_lifecycle_publication_and_queue_diagnostics() {
        let driver = RecordingDriver::default();
        let mut runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(8, 64)
                .with_diagnostics(diagnostics_environment(), None)
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        runtime
            .session_established(generation)
            .expect("session streams");
        runtime
            .publish(generation, publication())
            .expect("publication records");
        runtime
            .record_trade_diagnostics(generation, Some(1_800_000_001_000_000_000))
            .expect("live trade records");

        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Streaming);
        assert_eq!(snapshot.session_generation.map(NonZeroU64::get), Some(1));
        assert_eq!(snapshot.counters.trades, 1);
        assert_eq!(snapshot.counters.publications, 1);
        assert!(snapshot.last_message_age_nanos.is_some());
        assert_eq!(
            snapshot.queues[DiagnosticsQueue::SemanticEvent as usize].current_items,
            3
        );
    }

    #[test]
    fn production_runtime_records_generation_fenced_ui_diagnostics() {
        let mut runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            RecordingDriver::default(),
            "provider-session",
            config(8, 64)
                .with_diagnostics(
                    diagnostics_environment(),
                    Some(NonZeroU64::new(100).unwrap_or(NonZeroU64::MIN)),
                )
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        runtime
            .session_established(generation)
            .expect("session streams");
        runtime
            .observe_diagnostics_queue(DiagnosticsQueue::UiUpdate, 2, 8, 128, 512)
            .expect("bounded UI queue records");

        let mut chain = LatencyTimestampChain::new();
        chain.set(LatencyBoundary::ModelPublish, 10);
        chain.set(LatencyBoundary::UiEnqueue, 20);
        chain.set(LatencyBoundary::FrameSubmit, 30);
        chain.set(LatencyBoundary::Present, 40);
        for metric in [
            LocalLatencyMetric::ModelPublishToUiEnqueue,
            LocalLatencyMetric::UiEnqueueToFrameSubmit,
            LocalLatencyMetric::FrameSubmitToPresent,
        ] {
            runtime
                .record_latency_diagnostics(generation, metric, &chain)
                .expect("UI latency records");
        }
        runtime
            .record_coalesced_ui_update_diagnostics(generation, 3)
            .expect("UI conflation records");

        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.counters.coalesced_ui_updates, 3);
        assert!(snapshot.memory.current_bytes > 0);
        assert!(snapshot.memory.configured_bound_bytes >= snapshot.memory.current_bytes);
        assert_eq!(
            snapshot.queues[DiagnosticsQueue::UiUpdate as usize].high_water_items,
            2
        );
        for metric in [
            LocalLatencyMetric::ModelPublishToUiEnqueue,
            LocalLatencyMetric::UiEnqueueToFrameSubmit,
            LocalLatencyMetric::FrameSubmitToPresent,
        ] {
            assert_eq!(
                snapshot.detailed_latency[metric as usize]
                    .expect("detailed histogram publishes")
                    .sample_count,
                1
            );
        }

        runtime.stop().expect("session stops");
        assert_eq!(
            runtime.record_coalesced_ui_update_diagnostics(generation, 1),
            Err(DesktopProviderError::StaleGeneration)
        );
    }

    #[test]
    fn credential_failure_requires_authentication_recovery_diagnostics() {
        let mut runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: None,
                fail_load: false,
            },
            RecordingDriver::default(),
            "provider-session",
            config(4, 64)
                .with_diagnostics(diagnostics_environment(), None)
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");

        assert_eq!(
            runtime.connect(ConnectTrigger::Initial),
            Err(DesktopProviderError::CredentialMissing)
        );
        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Recovering);
        assert_eq!(
            snapshot.recovery_reason,
            Some(FeedRecoveryReason::Authentication)
        );
    }

    #[test]
    fn provider_invalidation_retains_exact_silence_diagnostics() {
        let mut runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            RecordingDriver::default(),
            "provider-session",
            config(4, 64)
                .with_diagnostics(diagnostics_environment(), None)
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        runtime
            .session_established(generation)
            .expect("session streams");

        runtime
            .session_invalid_for_provider(generation, ProviderInvalidationReason::HeartbeatSilence)
            .expect("provider silence fences the session");
        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("recovery snapshot publishes");
        assert_eq!(
            snapshot.recovery_reason,
            Some(FeedRecoveryReason::HeartbeatSilence)
        );
    }

    #[test]
    fn failed_provider_stop_does_not_leave_diagnostics_streaming() {
        let driver = RecordingDriver::default();
        let state = driver.0.clone();
        let mut runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(8, 64)
                .with_diagnostics(diagnostics_environment(), None)
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        runtime
            .session_established(generation)
            .expect("session establishes");
        state
            .lock()
            .expect("driver state lock is available")
            .fail_stop = true;

        assert_eq!(
            runtime.handle_network_event(NetworkEvent::Unavailable),
            Err(DesktopProviderError::ProviderUnavailable)
        );
        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Disconnected);
        assert_eq!(
            runtime.state().expect("state is available"),
            DesktopProviderState::StopUnconfirmed {
                generation,
                recovery: None,
            }
        );
    }

    #[test]
    fn rejected_semantic_enqueue_records_one_diagnostics_overflow() {
        let mut runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            RecordingDriver::default(),
            "provider-session",
            config(1, 64)
                .with_diagnostics(diagnostics_environment(), None)
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session-starting event fills the queue");

        assert_eq!(
            runtime.session_established(generation),
            Err(DesktopProviderError::EventQueueFull { maximum: 1 })
        );
        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Recovering);
        assert_eq!(snapshot.counters.overflows, 1);
    }

    #[test]
    fn diagnostics_identity_is_tighter_than_general_provider_discovery() {
        let oversized = ProviderEnvironment {
            provider_id: "x".repeat(MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES + 1),
            system_id: "test".to_string(),
            environment: "test".to_string(),
        };
        oversized
            .validate()
            .expect("general discovery accepts a 129-byte identity");
        assert_eq!(
            config(4, 64).with_diagnostics(oversized, None),
            Err(ProviderContractError::FieldTooLong {
                field: "provider_id",
                maximum: MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES,
            })
        );

        let control = ProviderEnvironment {
            provider_id: "rithmic\nfixture".to_string(),
            system_id: "test".to_string(),
            environment: "test".to_string(),
        };
        control
            .validate()
            .expect("general discovery preserves provider-native identity text");
        assert_eq!(
            config(4, 64).with_diagnostics(control, None),
            Err(ProviderContractError::ControlCharacter("provider_id"))
        );
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
    fn requested_connection_waits_for_native_environmental_restoration() {
        let (mut runtime, driver) = runtime(8);
        runtime
            .handle_network_event(NetworkEvent::Unavailable)
            .expect("idle network loss is recorded");
        assert_eq!(
            runtime
                .request_connection()
                .expect("offline connection intent is retained"),
            None
        );
        assert!(
            driver
                .lock()
                .expect("driver state lock is available")
                .starts
                .is_empty()
        );
        let generation = runtime
            .handle_network_event(NetworkEvent::Available)
            .expect("restoration is accepted")
            .expect("restoration starts the requested connection");
        let starts = &driver
            .lock()
            .expect("driver state lock is available")
            .starts;
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, generation);
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
    fn confirmed_stop_retry_marks_production_diagnostics_stopped() {
        let driver = RecordingDriver::default();
        let state = driver.0.clone();
        let mut runtime = DesktopProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(8, 64)
                .with_diagnostics(diagnostics_environment(), None)
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        runtime
            .session_established(generation)
            .expect("session establishes");
        state
            .lock()
            .expect("driver state lock is available")
            .fail_stop = true;
        assert_eq!(
            runtime.stop(),
            Err(DesktopProviderError::ProviderUnavailable)
        );
        state
            .lock()
            .expect("driver state lock is available")
            .fail_stop = false;
        assert_eq!(
            runtime
                .retry_unconfirmed_stop()
                .expect("cleanup confirms the stop"),
            None
        );

        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Stopped);
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
