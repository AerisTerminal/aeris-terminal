//! Worker-owned provider session lifecycle and generation fencing.
//!
//! The runtime loads opaque credentials from the operating-system vault for each
//! connection attempt and fences callbacks by a local generation. It has no cloud
//! client or payload-upload boundary.

use crate::session_contract::{
    ProviderContractError, ProviderEnvironment, ProviderInvalidationReason,
};
use core::fmt;
use std::{
    error::Error,
    marker::PhantomData,
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    rc::Rc,
    thread::{self, ThreadId},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tradingplot_market_data::MarketEvent;
use tradingplot_observability::{
    DiagnosticsQueue, FeedConnectionState, FeedCounter, FeedDiagnostics, FeedDiagnosticsSnapshot,
    FeedIdentity, FeedRecoveryReason, LatencyTimestampChain, LocalLatencyMetric,
    MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES, OrderBookDiagnosticsState,
};
pub use tradingplot_platform_runtime::NetworkEvent;
use tradingplot_platform_runtime::{CredentialVault, PowerEvent};
use zeroize::Zeroize;

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
pub enum RithmicProviderRuntimeState {
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

/// Explicit memory limits for one provider lifecycle owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicProviderRuntimeConfig {
    maximum_credential_bytes: NonZeroUsize,
    diagnostics: Option<RithmicProviderDiagnosticsConfig>,
}

impl RithmicProviderRuntimeConfig {
    /// Creates a bounded credential limit.
    #[must_use]
    pub const fn new(maximum_credential_bytes: NonZeroUsize) -> Self {
        Self {
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
        self.diagnostics = Some(RithmicProviderDiagnosticsConfig {
            environment,
            detailed_latency_maximum_nanos,
        });
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RithmicProviderDiagnosticsConfig {
    environment: ProviderEnvironment,
    detailed_latency_maximum_nanos: Option<NonZeroU64>,
}

/// Direct provider session boundary driven from one provider worker thread.
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

/// Redacted failure classes returned by the Rithmic provider runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicProviderRuntimeError {
    InvalidCredentialKey,
    WorkerThreadMismatch,
    InvalidTransition,
    GenerationExhausted,
    CredentialVaultUnavailable,
    CredentialMissing,
    CredentialTooLarge { requested: usize, maximum: usize },
    ProviderUnavailable,
    StaleGeneration,
    DiagnosticsUnavailable,
    SystemClockUnavailable,
}

impl fmt::Display for RithmicProviderRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic provider runtime failed: {self:?}")
    }
}

impl Error for RithmicProviderRuntimeError {}

/// Single-writer Rithmic provider lifecycle owner.
pub struct RithmicProviderRuntime<V, D: ProviderSessionDriver> {
    owner_thread: ThreadId,
    vault: V,
    driver: D,
    credential_key: String,
    config: RithmicProviderRuntimeConfig,
    state: RithmicProviderRuntimeState,
    last_generation: u64,
    connection_desired: bool,
    suspended: bool,
    network_available: bool,
    diagnostics: Option<ProviderRuntimeDiagnostics>,
    _not_send: PhantomData<Rc<()>>,
}

struct ProviderRuntimeDiagnostics {
    feed: FeedDiagnostics,
    epoch: Instant,
}

impl ProviderRuntimeDiagnostics {
    fn monotonic_nanos(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

impl<V, D: ProviderSessionDriver> fmt::Debug for RithmicProviderRuntime<V, D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicProviderRuntime")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<V, D> RithmicProviderRuntime<V, D>
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
        config: RithmicProviderRuntimeConfig,
    ) -> Result<Self, RithmicProviderRuntimeError> {
        let credential_key = credential_key.into();
        if credential_key.is_empty() || credential_key.chars().any(char::is_control) {
            return Err(RithmicProviderRuntimeError::InvalidCredentialKey);
        }
        let diagnostics = config
            .diagnostics
            .as_ref()
            .map(|diagnostics| {
                FeedIdentity::try_new(
                    diagnostics.environment.provider_id.clone(),
                    diagnostics.environment.system_id.clone(),
                    diagnostics.environment.environment.clone(),
                )
                .map(|identity| ProviderRuntimeDiagnostics {
                    feed: FeedDiagnostics::new(
                        identity,
                        diagnostics.detailed_latency_maximum_nanos,
                    ),
                    epoch: Instant::now(),
                })
                .map_err(|_| RithmicProviderRuntimeError::DiagnosticsUnavailable)
            })
            .transpose()?;
        Ok(Self {
            owner_thread: thread::current().id(),
            vault,
            driver,
            credential_key,
            config,
            state: RithmicProviderRuntimeState::Disconnected,
            last_generation: 0,
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
    pub fn state(&self) -> Result<RithmicProviderRuntimeState, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        Ok(self.state)
    }

    /// Publishes the current production diagnostics snapshot when its cadence allows.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access, clock failure, or diagnostics failure.
    pub fn try_diagnostics_snapshot(
        &mut self,
    ) -> Result<Option<FeedDiagnosticsSnapshot>, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        self.diagnostics_observe_runtime_memory()?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(None);
        };
        let wall_clock_unix_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RithmicProviderRuntimeError::SystemClockUnavailable)?
            .as_nanos();
        let wall_clock_unix_nanos = i64::try_from(wall_clock_unix_nanos)
            .map_err(|_| RithmicProviderRuntimeError::SystemClockUnavailable)?;
        self.diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed
            .try_snapshot(timestamp, wall_clock_unix_nanos)
            .map_err(|_| RithmicProviderRuntimeError::DiagnosticsUnavailable)
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
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        self.ensure_streaming_generation(generation)?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        let feed = &mut self
            .diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed;
        feed.record_message(timestamp, provider_timestamp_unix_nanos);
        feed.increment(FeedCounter::Trades);
        Ok(())
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
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        self.ensure_streaming_generation(generation)?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        let feed = &mut self
            .diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed;
        feed.record_message(timestamp, message_timestamp_unix_nanos);
        match event {
            MarketEvent::Trade(_) => feed.increment(FeedCounter::Trades),
            MarketEvent::Quote(_) => feed.increment(FeedCounter::Quotes),
            MarketEvent::DepthSnapshot(_) => {
                feed.increment(FeedCounter::DepthSnapshots);
                feed.set_order_book_state(OrderBookDiagnosticsState::Ready);
            }
            MarketEvent::DepthDelta(_) => feed.increment(FeedCounter::DepthDeltas),
        }
        Ok(())
    }

    /// Records one live heartbeat callback in production feed diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, stale generation, or diagnostics failure.
    pub fn record_heartbeat_diagnostics(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        self.ensure_streaming_generation(generation)?;
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed
            .record_heartbeat(timestamp);
        Ok(())
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
    ) -> Result<(), RithmicProviderRuntimeError> {
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
            .map_err(|_| RithmicProviderRuntimeError::DiagnosticsUnavailable)
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
    ) -> Result<(), RithmicProviderRuntimeError> {
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
            .map_err(|_| RithmicProviderRuntimeError::DiagnosticsUnavailable)
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
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        if self.active_generation() != Some(generation) {
            return self.reject_stale();
        }
        if self.diagnostics.is_none() {
            return Ok(());
        }
        self.diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed
            .increment_by(FeedCounter::CoalescedUiUpdates, count);
        Ok(())
    }

    /// Starts a fresh direct-provider session with vault-loaded credentials.
    ///
    /// Credential bytes are bounded, borrowed only for the provider call, and
    /// zeroized before this function returns.
    ///
    /// # Errors
    ///
    /// Returns a redacted vault, credential, provider, transition, generation,
    /// or ownership failure.
    pub fn connect(
        &mut self,
        trigger: ConnectTrigger,
    ) -> Result<SessionGeneration, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        if self.suspended || !self.network_available {
            return Err(RithmicProviderRuntimeError::InvalidTransition);
        }
        let valid_transition = matches!(
            (self.state, trigger),
            (
                RithmicProviderRuntimeState::Disconnected,
                ConnectTrigger::Initial | ConnectTrigger::Retry
            ) | (
                RithmicProviderRuntimeState::RecoveryRequired { .. },
                ConnectTrigger::Retry
            ) | (
                RithmicProviderRuntimeState::Suspended,
                ConnectTrigger::Resume
            ) | (
                RithmicProviderRuntimeState::NetworkUnavailable,
                ConnectTrigger::NetworkRestored
            )
        );
        if !valid_transition {
            return Err(RithmicProviderRuntimeError::InvalidTransition);
        }
        self.connection_desired = true;
        let generation_value = self
            .last_generation
            .checked_add(1)
            .ok_or(RithmicProviderRuntimeError::GenerationExhausted)?;
        let generation = SessionGeneration(
            NonZeroU64::new(generation_value)
                .ok_or(RithmicProviderRuntimeError::GenerationExhausted)?,
        );
        let credentials = match self.driver.credential_requirement() {
            ProviderCredentialRequirement::Required => match self.load_credentials() {
                Ok(credentials) => credentials,
                Err(error) => {
                    self.state = RithmicProviderRuntimeState::RecoveryRequired {
                        generation: None,
                        reason: RecoveryReason::CredentialUnavailable,
                    };
                    self.diagnostics_require_recovery(RecoveryReason::CredentialUnavailable)?;
                    return Err(error);
                }
            },
            ProviderCredentialRequirement::Public => CredentialBytes(Vec::new()),
        };
        self.last_generation = generation_value;
        self.state = RithmicProviderRuntimeState::Connecting {
            generation,
            trigger,
        };
        self.diagnostics_begin_session(generation)?;
        if self
            .driver
            .start_session(generation, credentials.as_slice())
            .is_err()
        {
            self.state = RithmicProviderRuntimeState::RecoveryRequired {
                generation: Some(generation),
                reason: RecoveryReason::ProviderFailure,
            };
            self.diagnostics_require_recovery(RecoveryReason::ProviderFailure)?;
            return Err(RithmicProviderRuntimeError::ProviderUnavailable);
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
    /// runtime, or a provider/vault failure while starting immediately.
    pub fn request_connection(
        &mut self,
    ) -> Result<Option<SessionGeneration>, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        if matches!(
            self.state,
            RithmicProviderRuntimeState::Stopped
                | RithmicProviderRuntimeState::Connecting { .. }
                | RithmicProviderRuntimeState::Streaming { .. }
        ) {
            return Err(RithmicProviderRuntimeError::InvalidTransition);
        }
        self.connection_desired = true;
        if self.suspended
            || !self.network_available
            || matches!(
                self.state,
                RithmicProviderRuntimeState::StopUnconfirmed { .. }
            )
        {
            return Ok(None);
        }
        let trigger = if matches!(
            self.state,
            RithmicProviderRuntimeState::RecoveryRequired { .. }
        ) {
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
    /// Returns an error for a stale callback, invalid transition, or wrong thread.
    pub fn session_established(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        match self.state {
            RithmicProviderRuntimeState::Connecting {
                generation: active, ..
            } if active == generation => {}
            _ if self.active_generation() != Some(generation) => return self.reject_stale(),
            _ => return Err(RithmicProviderRuntimeError::InvalidTransition),
        }
        self.state = RithmicProviderRuntimeState::Streaming { generation };
        self.diagnostics_mark_streaming()?;
        Ok(())
    }

    /// Invalidates the active provider stream and requires a fresh generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale callback, invalid lifecycle state, wrong
    /// thread, or provider stop failure.
    pub fn session_invalid(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.session_invalid_with_reason(generation, RecoveryReason::TransportInvalid, None)
    }

    /// Invalidates an active generation while retaining the provider's coarse,
    /// non-secret reason in feed diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale callback, invalid lifecycle state, wrong
    /// thread, or provider stop failure.
    pub fn session_invalid_for_provider(
        &mut self,
        generation: SessionGeneration,
        reason: ProviderInvalidationReason,
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.session_invalid_with_reason(generation, RecoveryReason::TransportInvalid, Some(reason))
    }

    /// Invalidates a generation whose bounded provider callback queue overflowed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale callback, invalid lifecycle state, wrong
    /// thread, or provider stop failure.
    pub fn session_callback_queue_overflow(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.session_invalid_with_reason(generation, RecoveryReason::SemanticQueueOverflow, None)
    }

    fn session_invalid_with_reason(
        &mut self,
        generation: SessionGeneration,
        reason: RecoveryReason,
        provider_reason: Option<ProviderInvalidationReason>,
    ) -> Result<(), RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        if self.active_generation() != Some(generation) {
            return self.reject_stale();
        }
        self.state = RithmicProviderRuntimeState::RecoveryRequired {
            generation: Some(generation),
            reason,
        };
        if let Some(provider_reason) = provider_reason {
            self.diagnostics_observe_invalidation(provider_reason)?;
        } else {
            self.diagnostics_require_recovery(reason)?;
        }
        let stop_failed = self.driver.stop_session(generation).is_err();
        if stop_failed {
            self.state = RithmicProviderRuntimeState::StopUnconfirmed {
                generation,
                recovery: Some(reason),
            };
        }
        if stop_failed {
            return Err(RithmicProviderRuntimeError::ProviderUnavailable);
        }
        Ok(())
    }

    /// Applies one native suspend or resume transition.
    ///
    /// Resume starts a fresh generation and reloads credentials from the vault.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle, provider, vault, or ownership error.
    pub fn handle_power_event(
        &mut self,
        event: PowerEvent,
    ) -> Result<Option<SessionGeneration>, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        match event {
            PowerEvent::Suspending => {
                if self.suspended || self.state == RithmicProviderRuntimeState::Stopped {
                    return Err(RithmicProviderRuntimeError::InvalidTransition);
                }
                self.suspended = true;
                if matches!(
                    self.state,
                    RithmicProviderRuntimeState::StopUnconfirmed { .. }
                ) {
                    return Ok(None);
                }
                let fence_result = self.fence_into(RithmicProviderRuntimeState::Suspended, None);
                self.diagnostics_mark_disconnected()?;
                fence_result?;
                Ok(None)
            }
            PowerEvent::Resumed => {
                if !self.suspended || self.state == RithmicProviderRuntimeState::Stopped {
                    return Err(RithmicProviderRuntimeError::InvalidTransition);
                }
                self.suspended = false;
                if matches!(
                    self.state,
                    RithmicProviderRuntimeState::StopUnconfirmed { .. }
                ) {
                    return Ok(None);
                }
                if !self.connection_desired {
                    self.state = if self.network_available {
                        RithmicProviderRuntimeState::Disconnected
                    } else {
                        RithmicProviderRuntimeState::NetworkUnavailable
                    };
                    return Ok(None);
                }
                if !self.network_available {
                    self.state = RithmicProviderRuntimeState::NetworkUnavailable;
                    return Ok(None);
                }
                self.state = RithmicProviderRuntimeState::Suspended;
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
    /// Returns a redacted lifecycle, provider, vault, or ownership error.
    pub fn handle_network_event(
        &mut self,
        event: NetworkEvent,
    ) -> Result<Option<SessionGeneration>, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        match event {
            NetworkEvent::Unavailable => {
                if !self.network_available || self.state == RithmicProviderRuntimeState::Stopped {
                    return Err(RithmicProviderRuntimeError::InvalidTransition);
                }
                self.network_available = false;
                if matches!(
                    self.state,
                    RithmicProviderRuntimeState::StopUnconfirmed { .. }
                ) {
                    return Ok(None);
                }
                let fence_result =
                    self.fence_into(RithmicProviderRuntimeState::NetworkUnavailable, None);
                self.diagnostics_mark_disconnected()?;
                fence_result?;
                Ok(None)
            }
            NetworkEvent::Available => {
                if self.network_available || self.state == RithmicProviderRuntimeState::Stopped {
                    return Err(RithmicProviderRuntimeError::InvalidTransition);
                }
                self.network_available = true;
                if matches!(
                    self.state,
                    RithmicProviderRuntimeState::StopUnconfirmed { .. }
                ) {
                    return Ok(None);
                }
                if !self.connection_desired {
                    self.state = if self.suspended {
                        RithmicProviderRuntimeState::Suspended
                    } else {
                        RithmicProviderRuntimeState::Disconnected
                    };
                    return Ok(None);
                }
                if self.suspended {
                    self.state = RithmicProviderRuntimeState::Suspended;
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
    /// Returns an error for wrong-thread use, an invalid state, or another provider
    /// stop failure.
    pub fn retry_unconfirmed_stop(
        &mut self,
    ) -> Result<Option<SessionGeneration>, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        let RithmicProviderRuntimeState::StopUnconfirmed {
            generation,
            recovery,
        } = self.state
        else {
            return Err(RithmicProviderRuntimeError::InvalidTransition);
        };
        if self.driver.stop_session(generation).is_err() {
            return Err(RithmicProviderRuntimeError::ProviderUnavailable);
        }
        if !self.connection_desired {
            self.state = RithmicProviderRuntimeState::Stopped;
            self.diagnostics_mark_stopped()?;
            return Ok(None);
        }
        if self.suspended {
            self.state = RithmicProviderRuntimeState::Suspended;
            self.diagnostics_mark_disconnected()?;
            return Ok(None);
        }
        if !self.network_available {
            self.state = RithmicProviderRuntimeState::NetworkUnavailable;
            self.diagnostics_mark_disconnected()?;
            return Ok(None);
        }
        let reason = recovery.unwrap_or(RecoveryReason::ProviderFailure);
        self.state = RithmicProviderRuntimeState::RecoveryRequired {
            generation: Some(generation),
            reason,
        };
        self.connect(ConnectTrigger::Retry).map(Some)
    }

    /// Fences the active generation and permanently stops this runtime.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread use, repeated shutdown, or provider stop
    /// failure.
    pub fn stop(&mut self) -> Result<(), RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        if self.state == RithmicProviderRuntimeState::Stopped {
            return Err(RithmicProviderRuntimeError::InvalidTransition);
        }
        self.connection_desired = false;
        if matches!(
            self.state,
            RithmicProviderRuntimeState::StopUnconfirmed { .. }
        ) {
            return self.retry_unconfirmed_stop().map(|_| ());
        }
        let fence_result = self.fence_into(RithmicProviderRuntimeState::Stopped, None);
        self.diagnostics_mark_stopped()?;
        fence_result
    }

    fn load_credentials(&self) -> Result<CredentialBytes, RithmicProviderRuntimeError> {
        let credentials = self
            .vault
            .load(&self.credential_key)
            .map_err(|_| RithmicProviderRuntimeError::CredentialVaultUnavailable)?
            .ok_or(RithmicProviderRuntimeError::CredentialMissing)?;
        let credentials = CredentialBytes(credentials);
        if credentials.len() > self.config.maximum_credential_bytes.get() {
            return Err(RithmicProviderRuntimeError::CredentialTooLarge {
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
    ) -> Result<(), RithmicProviderRuntimeError> {
        let Some(timestamp) = self.diagnostics_now() else {
            return Ok(());
        };
        self.diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed
            .begin_session(generation.0, timestamp)
            .map_err(|_| RithmicProviderRuntimeError::DiagnosticsUnavailable)
    }

    fn diagnostics_mark_streaming(&mut self) -> Result<(), RithmicProviderRuntimeError> {
        if self.diagnostics.is_none() {
            return Ok(());
        }
        self.diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed
            .set_connection_state(FeedConnectionState::Streaming);
        Ok(())
    }

    fn diagnostics_require_recovery(
        &mut self,
        reason: RecoveryReason,
    ) -> Result<(), RithmicProviderRuntimeError> {
        if self.diagnostics.is_none() {
            return Ok(());
        }
        let feed = &mut self
            .diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed;
        let reason = match reason {
            RecoveryReason::CredentialUnavailable => FeedRecoveryReason::Authentication,
            RecoveryReason::ProviderFailure | RecoveryReason::TransportInvalid => {
                FeedRecoveryReason::Transport
            }
            RecoveryReason::SemanticQueueOverflow => {
                feed.increment(FeedCounter::Overflows);
                FeedRecoveryReason::QueueOverflow
            }
        };
        feed.require_recovery(reason);
        Ok(())
    }

    fn diagnostics_observe_invalidation(
        &mut self,
        reason: ProviderInvalidationReason,
    ) -> Result<(), RithmicProviderRuntimeError> {
        if self.diagnostics.is_none() {
            return Ok(());
        }
        let feed = &mut self
            .diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed;
        let recovery = match reason {
            ProviderInvalidationReason::Transport => FeedRecoveryReason::Transport,
            ProviderInvalidationReason::Authentication => FeedRecoveryReason::Authentication,
            ProviderInvalidationReason::AgreementRequired => FeedRecoveryReason::AgreementRequired,
            ProviderInvalidationReason::UnsupportedSystem => FeedRecoveryReason::UnsupportedSystem,
            ProviderInvalidationReason::SchemaMismatch => FeedRecoveryReason::SchemaMismatch,
            ProviderInvalidationReason::HeartbeatSilence => FeedRecoveryReason::HeartbeatSilence,
            ProviderInvalidationReason::MessageSilence => FeedRecoveryReason::MessageSilence,
            ProviderInvalidationReason::SequenceGap => {
                feed.increment(FeedCounter::Gaps);
                feed.set_order_book_state(OrderBookDiagnosticsState::Recovering);
                FeedRecoveryReason::SequenceGap
            }
            ProviderInvalidationReason::QueueOverflow => {
                feed.increment(FeedCounter::Overflows);
                FeedRecoveryReason::QueueOverflow
            }
            ProviderInvalidationReason::MalformedMessage => {
                feed.increment(FeedCounter::MalformedMessages);
                FeedRecoveryReason::MalformedMessage
            }
        };
        feed.require_recovery(recovery);
        Ok(())
    }

    fn diagnostics_mark_disconnected(&mut self) -> Result<(), RithmicProviderRuntimeError> {
        if self.diagnostics.is_none() {
            return Ok(());
        }
        self.diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed
            .set_connection_state(FeedConnectionState::Disconnected);
        Ok(())
    }

    fn diagnostics_mark_stopped(&mut self) -> Result<(), RithmicProviderRuntimeError> {
        if self.diagnostics.is_none() {
            return Ok(());
        }
        self.diagnostics
            .as_mut()
            .ok_or(RithmicProviderRuntimeError::DiagnosticsUnavailable)?
            .feed
            .set_connection_state(FeedConnectionState::Stopped);
        Ok(())
    }

    fn diagnostics_record_stale_callback(&mut self) {
        if let Some(diagnostics) = &mut self.diagnostics {
            diagnostics.feed.increment(FeedCounter::StaleCallbacks);
        }
    }

    fn diagnostics_observe_runtime_memory(&mut self) -> Result<(), RithmicProviderRuntimeError> {
        let Some(diagnostics) = &mut self.diagnostics else {
            return Ok(());
        };
        let fixed_bytes = size_of::<Self>();
        diagnostics
            .feed
            .observe_runtime_memory(fixed_bytes, fixed_bytes)
            .map_err(|_| RithmicProviderRuntimeError::DiagnosticsUnavailable)
    }

    fn ensure_streaming_generation(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), RithmicProviderRuntimeError> {
        match self.state {
            RithmicProviderRuntimeState::Streaming { generation: active }
                if active == generation =>
            {
                Ok(())
            }
            _ if self.active_generation() != Some(generation) => self.reject_stale(),
            _ => Err(RithmicProviderRuntimeError::InvalidTransition),
        }
    }

    fn active_generation(&self) -> Option<SessionGeneration> {
        match self.state {
            RithmicProviderRuntimeState::Connecting { generation, .. }
            | RithmicProviderRuntimeState::Streaming { generation } => Some(generation),
            RithmicProviderRuntimeState::Disconnected
            | RithmicProviderRuntimeState::RecoveryRequired { .. }
            | RithmicProviderRuntimeState::StopUnconfirmed { .. }
            | RithmicProviderRuntimeState::Suspended
            | RithmicProviderRuntimeState::NetworkUnavailable
            | RithmicProviderRuntimeState::Stopped => None,
        }
    }

    fn fence_into(
        &mut self,
        state: RithmicProviderRuntimeState,
        recovery: Option<RecoveryReason>,
    ) -> Result<(), RithmicProviderRuntimeError> {
        let generation = self.active_generation();
        self.state = state;
        if let Some(generation) = generation
            && self.driver.stop_session(generation).is_err()
        {
            self.state = RithmicProviderRuntimeState::StopUnconfirmed {
                generation,
                recovery,
            };
            return Err(RithmicProviderRuntimeError::ProviderUnavailable);
        }
        Ok(())
    }

    fn reject_stale<T>(&mut self) -> Result<T, RithmicProviderRuntimeError> {
        self.diagnostics_record_stale_callback();
        Err(RithmicProviderRuntimeError::StaleGeneration)
    }

    fn ensure_owner(&self) -> Result<(), RithmicProviderRuntimeError> {
        if thread::current().id() != self.owner_thread {
            return Err(RithmicProviderRuntimeError::WorkerThreadMismatch);
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
    ) -> Result<bool, RithmicProviderRuntimeError> {
        self.ensure_owner()?;
        Ok(predicate(&self.driver))
    }
}

impl<V, D: ProviderSessionDriver> Drop for RithmicProviderRuntime<V, D> {
    fn drop(&mut self) {
        let generation = match self.state {
            RithmicProviderRuntimeState::Connecting { generation, .. }
            | RithmicProviderRuntimeState::Streaming { generation }
            | RithmicProviderRuntimeState::StopUnconfirmed { generation, .. } => Some(generation),
            RithmicProviderRuntimeState::Disconnected
            | RithmicProviderRuntimeState::RecoveryRequired { .. }
            | RithmicProviderRuntimeState::Suspended
            | RithmicProviderRuntimeState::NetworkUnavailable
            | RithmicProviderRuntimeState::Stopped => None,
        };
        if let Some(generation) = generation {
            let _ = self.driver.stop_session(generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectTrigger, MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES, NetworkEvent, ProviderContractError,
        ProviderEnvironment, ProviderInvalidationReason, ProviderSessionDriver, RecoveryReason,
        RithmicProviderRuntime, RithmicProviderRuntimeConfig, RithmicProviderRuntimeError,
        RithmicProviderRuntimeState, SessionGeneration,
    };
    use std::{
        num::{NonZeroU64, NonZeroUsize},
        sync::{Arc, Mutex},
    };
    use tradingplot_observability::{
        DiagnosticsQueue, FeedConnectionState, FeedRecoveryReason, LatencyBoundary,
        LatencyTimestampChain, LocalLatencyMetric,
    };
    use tradingplot_platform_runtime::{CredentialVault, PowerEvent};

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

    fn config(maximum_credential_bytes: usize) -> RithmicProviderRuntimeConfig {
        RithmicProviderRuntimeConfig::new(
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

    fn runtime() -> (
        RithmicProviderRuntime<MemoryVault, RecordingDriver>,
        Arc<Mutex<DriverState>>,
    ) {
        let driver = RecordingDriver::default();
        let state = driver.0.clone();
        let runtime = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(64),
        )
        .expect("valid runtime starts disconnected");
        (runtime, state)
    }

    #[test]
    fn production_runtime_owns_lifecycle_and_feed_diagnostics() {
        let driver = RecordingDriver::default();
        let mut runtime = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(64)
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
            .record_trade_diagnostics(generation, Some(1_800_000_001_000_000_000))
            .expect("live trade records");

        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Streaming);
        assert_eq!(snapshot.session_generation.map(NonZeroU64::get), Some(1));
        assert_eq!(snapshot.counters.trades, 1);
        assert_eq!(snapshot.counters.publications, 0);
        assert!(snapshot.last_message_age_nanos.is_some());
        assert_eq!(
            snapshot.queues[DiagnosticsQueue::SemanticEvent as usize].current_items,
            0
        );
    }

    #[test]
    fn lifecycle_fences_diagnostics_callbacks_when_metrics_are_disabled() {
        let (mut runtime, _) = runtime();
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        runtime
            .session_established(generation)
            .expect("session streams");
        let stale = SessionGeneration::new(
            NonZeroU64::new(generation.get() + 1).unwrap_or(NonZeroU64::MIN),
        );

        assert_eq!(
            runtime.record_trade_diagnostics(stale, None),
            Err(RithmicProviderRuntimeError::StaleGeneration)
        );
    }

    #[test]
    fn production_runtime_records_generation_fenced_ui_diagnostics() {
        let mut runtime = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            RecordingDriver::default(),
            "provider-session",
            config(64)
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
            Err(RithmicProviderRuntimeError::StaleGeneration)
        );
    }

    #[test]
    fn credential_failure_requires_authentication_recovery_diagnostics() {
        let mut runtime = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: None,
                fail_load: false,
            },
            RecordingDriver::default(),
            "provider-session",
            config(64)
                .with_diagnostics(diagnostics_environment(), None)
                .expect("diagnostics environment validates"),
        )
        .expect("diagnostics runtime opens");

        assert_eq!(
            runtime.connect(ConnectTrigger::Initial),
            Err(RithmicProviderRuntimeError::CredentialMissing)
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
        let mut runtime = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            RecordingDriver::default(),
            "provider-session",
            config(64)
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
        let mut runtime = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(64)
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
            Err(RithmicProviderRuntimeError::ProviderUnavailable)
        );
        let snapshot = runtime
            .try_diagnostics_snapshot()
            .expect("snapshot succeeds")
            .expect("first snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Disconnected);
        assert_eq!(
            runtime.state().expect("state is available"),
            RithmicProviderRuntimeState::StopUnconfirmed {
                generation,
                recovery: None,
            }
        );
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
            config(64).with_diagnostics(oversized, None),
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
            config(64).with_diagnostics(control, None),
            Err(ProviderContractError::ControlCharacter("provider_id"))
        );
    }

    #[test]
    fn vault_credentials_are_bounded_and_errors_are_redacted() {
        let driver = RecordingDriver::default();
        let mut missing = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: None,
                fail_load: false,
            },
            driver.clone(),
            "provider-session",
            config(8),
        )
        .expect("runtime configuration is valid");
        assert_eq!(
            missing.connect(ConnectTrigger::Initial),
            Err(RithmicProviderRuntimeError::CredentialMissing)
        );

        let mut oversized = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"secret-value".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(8),
        )
        .expect("runtime configuration is valid");
        assert_eq!(
            oversized.connect(ConnectTrigger::Initial),
            Err(RithmicProviderRuntimeError::CredentialTooLarge {
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
        let (mut runtime, driver) = runtime();
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
            runtime.session_established(first),
            Err(RithmicProviderRuntimeError::StaleGeneration)
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
            Err(RithmicProviderRuntimeError::StaleGeneration)
        );
        assert_eq!(
            driver.lock().expect("driver state lock is available").stops,
            vec![first, second]
        );
    }

    #[test]
    fn environmental_cycles_do_not_connect_an_idle_runtime() {
        let (mut runtime, driver) = runtime();
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
            RithmicProviderRuntimeState::Disconnected
        );
    }

    #[test]
    fn public_connect_cannot_bypass_environmental_fences() {
        let (mut runtime, driver) = runtime();
        runtime
            .handle_power_event(PowerEvent::Suspending)
            .expect("idle suspend is recorded");
        assert_eq!(
            runtime.connect(ConnectTrigger::Resume),
            Err(RithmicProviderRuntimeError::InvalidTransition)
        );
        runtime
            .handle_power_event(PowerEvent::Resumed)
            .expect("idle resume is recorded");
        runtime
            .handle_network_event(NetworkEvent::Unavailable)
            .expect("idle network loss is recorded");
        assert_eq!(
            runtime.connect(ConnectTrigger::NetworkRestored),
            Err(RithmicProviderRuntimeError::InvalidTransition)
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
        let (mut runtime, driver) = runtime();
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
        let (mut runtime, driver) = runtime();
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        runtime
            .session_established(generation)
            .expect("session establishes");
        driver
            .lock()
            .expect("driver state lock is available")
            .fail_stop = true;
        assert_eq!(
            runtime.session_invalid(generation),
            Err(RithmicProviderRuntimeError::ProviderUnavailable)
        );
        assert_eq!(
            runtime.state().expect("state is available"),
            RithmicProviderRuntimeState::StopUnconfirmed {
                generation,
                recovery: Some(RecoveryReason::TransportInvalid),
            }
        );
        assert_eq!(
            runtime.connect(ConnectTrigger::Retry),
            Err(RithmicProviderRuntimeError::InvalidTransition)
        );
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
            RithmicProviderRuntimeState::Connecting {
                generation: replacement,
                trigger: ConnectTrigger::Retry,
            }
        );
    }

    #[test]
    fn confirmed_stop_retry_marks_production_diagnostics_stopped() {
        let driver = RecordingDriver::default();
        let state = driver.0.clone();
        let mut runtime = RithmicProviderRuntime::try_new(
            MemoryVault {
                secret: Some(b"device-only-provider-token".to_vec()),
                fail_load: false,
            },
            driver,
            "provider-session",
            config(64)
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
            Err(RithmicProviderRuntimeError::ProviderUnavailable)
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
        let (mut runtime, _) = runtime();
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
        let (mut runtime, driver) = runtime();
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
            Err(RithmicProviderRuntimeError::ProviderUnavailable)
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
    fn provider_failure_exposes_only_a_coarse_recovery_class() {
        let (mut runtime, driver) = runtime();
        driver
            .lock()
            .expect("driver state lock is available")
            .fail_start = true;
        assert_eq!(
            runtime.connect(ConnectTrigger::Initial),
            Err(RithmicProviderRuntimeError::ProviderUnavailable)
        );
        assert_eq!(
            runtime.state().expect("state is available"),
            RithmicProviderRuntimeState::RecoveryRequired {
                generation: Some(SessionGeneration::new(NonZeroU64::MIN)),
                reason: RecoveryReason::ProviderFailure,
            }
        );
    }

    #[test]
    fn drop_attempts_to_stop_an_active_generation() {
        let (mut runtime, driver) = runtime();
        let generation = runtime
            .connect(ConnectTrigger::Initial)
            .expect("session starts");
        drop(runtime);
        assert_eq!(
            driver.lock().expect("driver state lock is available").stops,
            vec![generation]
        );
    }
}
