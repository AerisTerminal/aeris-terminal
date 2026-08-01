//! Single-connection plain-loopback lifecycle owner.
//!
//! This module owns connection establishment, the bounded command and event queues, the
//! reconnect budget, close-handshake completion, and the step state machine for one
//! loopback socket. The background thread bridge drives it through explicit commands.

use crate::endpoint::PlainLoopbackWebSocketEndpoint;
use crate::session::{
    MarketWebSocketPublication, MarketWebSocketSession, MarketWebSocketState,
    WebSocketMessageOutcome, WebSocketRecoveryReason,
};
use axiusflow_application::{MarketGeneration, ProvenancedMarketBar, ResnapshotReason};
use core::fmt;
use std::{
    collections::VecDeque,
    error::Error,
    net::TcpStream,
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, client::client_with_config};

/// Explicit queue, timeout, and reconnect bounds for one loopback connection owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainLoopbackLifecycleConfig {
    command_capacity: NonZeroUsize,
    event_capacity: NonZeroUsize,
    connection_attempt_limit: NonZeroUsize,
    io_timeout: Duration,
}

impl PlainLoopbackLifecycleConfig {
    /// Creates lifecycle limits with a non-zero socket I/O timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when the I/O timeout is zero.
    pub fn try_new(
        command_capacity: NonZeroUsize,
        event_capacity: NonZeroUsize,
        connection_attempt_limit: NonZeroUsize,
        io_timeout: Duration,
    ) -> Result<Self, PlainLoopbackLifecycleConfigError> {
        if io_timeout.is_zero() {
            return Err(PlainLoopbackLifecycleConfigError::ZeroIoTimeout);
        }
        Ok(Self {
            command_capacity,
            event_capacity,
            connection_attempt_limit,
            io_timeout,
        })
    }
}

/// Invalid loopback lifecycle configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackLifecycleConfigError {
    ZeroIoTimeout,
}

impl fmt::Display for PlainLoopbackLifecycleConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plain loopback lifecycle configuration rejected: {self:?}"
        )
    }
}

impl Error for PlainLoopbackLifecycleConfigError {}

/// Bounded commands accepted by the single connection owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackCommand {
    Connect,
    Recover { reason: ResnapshotReason },
    Shutdown,
}

/// WebSocket control messages observed by the owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSocketControlSignal {
    Ping,
    Pong,
}

/// Bounded lifecycle output consumed by a future background-runtime bridge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlainLoopbackLifecycleEvent {
    Connected {
        connection_epoch: u64,
        attempt: usize,
    },
    Publication(Box<MarketWebSocketPublication>),
    Control(WebSocketControlSignal),
    RecoveryRequired {
        connection_epoch: Option<u64>,
        reason: WebSocketRecoveryReason,
        dropped_events: usize,
    },
    ConnectionAttemptFailed {
        attempt: usize,
        remaining: usize,
    },
    ReconnectExhausted {
        attempts: usize,
        reason: WebSocketRecoveryReason,
        dropped_events: usize,
    },
    Stopped {
        close_handshake_complete: bool,
        dropped_events: usize,
    },
}

/// Observable lifecycle state for the adapter-owned loopback socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackLifecycleState {
    Idle,
    AwaitingSnapshot {
        connection_epoch: u64,
    },
    Streaming {
        connection_epoch: u64,
    },
    ReconnectPending {
        reason: WebSocketRecoveryReason,
        attempts: usize,
    },
    Exhausted {
        attempts: usize,
        reason: WebSocketRecoveryReason,
    },
    Stopped,
}

/// One bounded unit of progress made by [`PlainLoopbackMarketWebSocketOwner::step`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackLifecycleStep {
    Idle,
    Connected,
    MessageAccepted,
    RecoveryScheduled,
    ConnectionAttemptFailed,
    ReconnectExhausted,
    Stopped,
}

type PlainLoopbackSocket = WebSocket<TcpStream>;

const PLAIN_LOOPBACK_CLOSE_TIMEOUT: Duration = Duration::from_millis(100);
pub(crate) const PLAIN_LOOPBACK_RUNTIME_JOIN_TIMEOUT: Duration = Duration::from_millis(250);

/// Blocking single-owner plain-loopback socket lifecycle used only for software evidence.
///
/// The owner is intentionally not `Clone`. Calls to [`Self::step`] own connection,
/// read, decode, model mutation, reconnect, and shutdown ordering on one thread.
/// Production networking still requires an evented TLS/authenticated connector.
#[derive(Debug)]
pub struct PlainLoopbackMarketWebSocketOwner {
    endpoint: PlainLoopbackWebSocketEndpoint,
    config: PlainLoopbackLifecycleConfig,
    session: MarketWebSocketSession,
    socket: Option<PlainLoopbackSocket>,
    commands: VecDeque<PlainLoopbackCommand>,
    events: VecDeque<PlainLoopbackLifecycleEvent>,
    state: PlainLoopbackLifecycleState,
    connection_epoch: u64,
    attempts_in_cycle: usize,
    recovery_cause: Option<WebSocketRecoveryReason>,
}

impl PlainLoopbackMarketWebSocketOwner {
    /// Creates an owner only when its lifecycle queue can hold the session's
    /// maximum publication burst.
    ///
    /// # Errors
    ///
    /// Returns an error when lifecycle event capacity is below publication capacity.
    pub fn try_new(
        endpoint: PlainLoopbackWebSocketEndpoint,
        config: PlainLoopbackLifecycleConfig,
        session: MarketWebSocketSession,
    ) -> Result<Self, PlainLoopbackLifecycleError> {
        let publication_capacity = session.publication_capacity();
        if config.event_capacity.get() < publication_capacity {
            return Err(
                PlainLoopbackLifecycleError::EventCapacityBelowPublicationCapacity {
                    event_capacity: config.event_capacity.get(),
                    publication_capacity,
                },
            );
        }
        Ok(Self {
            endpoint,
            config,
            session,
            socket: None,
            commands: VecDeque::with_capacity(config.command_capacity.get()),
            events: VecDeque::with_capacity(config.event_capacity.get()),
            state: PlainLoopbackLifecycleState::Idle,
            connection_epoch: 0,
            attempts_in_cycle: 0,
            recovery_cause: None,
        })
    }

    /// Enqueues one lifecycle command without exceeding the declared item bound.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or when the command queue is full.
    pub fn try_enqueue_command(
        &mut self,
        command: PlainLoopbackCommand,
    ) -> Result<(), PlainLoopbackLifecycleError> {
        if self.state == PlainLoopbackLifecycleState::Stopped {
            return Err(PlainLoopbackLifecycleError::AlreadyStopped);
        }
        if self.commands.len() >= self.config.command_capacity.get() {
            return Err(PlainLoopbackLifecycleError::CommandQueueFull {
                maximum: self.config.command_capacity.get(),
            });
        }
        self.commands.push_back(command);
        Ok(())
    }

    /// Processes at most one command, connection attempt, or inbound message.
    ///
    /// Connected reads block only up to the configured I/O timeout. Reconnect attempts
    /// are counted per recovery cycle and stop in an explicit exhausted state.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid command state, epoch overflow, or lifecycle-event
    /// overflow. Socket/protocol failures become bounded recovery events and states.
    pub fn step(&mut self) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        if let Some(command) = self.commands.pop_front() {
            return self.process_command(command);
        }
        match self.state {
            PlainLoopbackLifecycleState::Idle => Ok(PlainLoopbackLifecycleStep::Idle),
            PlainLoopbackLifecycleState::ReconnectPending { .. } => self.connect(),
            PlainLoopbackLifecycleState::AwaitingSnapshot { .. }
            | PlainLoopbackLifecycleState::Streaming { .. } => self.read_message(),
            PlainLoopbackLifecycleState::Exhausted { .. } => {
                Ok(PlainLoopbackLifecycleStep::ReconnectExhausted)
            }
            PlainLoopbackLifecycleState::Stopped => Ok(PlainLoopbackLifecycleStep::Stopped),
        }
    }

    fn process_command(
        &mut self,
        command: PlainLoopbackCommand,
    ) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        match command {
            PlainLoopbackCommand::Connect => match self.state {
                PlainLoopbackLifecycleState::Idle
                | PlainLoopbackLifecycleState::Exhausted { .. } => {
                    self.attempts_in_cycle = 0;
                    self.recovery_cause = None;
                    self.connect()
                }
                _ => Err(PlainLoopbackLifecycleError::ConnectWhileActive(self.state)),
            },
            PlainLoopbackCommand::Recover { reason } => self.request_recovery(reason),
            PlainLoopbackCommand::Shutdown => self.shutdown(),
        }
    }

    fn request_recovery(
        &mut self,
        reason: ResnapshotReason,
    ) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        self.socket = None;
        self.session.require_resnapshot(reason);
        let dropped_events = self.events.len();
        self.events.clear();
        self.attempts_in_cycle = 0;
        let cause = WebSocketRecoveryReason::ModelRecovery(reason);
        self.recovery_cause = Some(cause);
        self.state = PlainLoopbackLifecycleState::ReconnectPending {
            reason: cause,
            attempts: 0,
        };
        self.emit_event(PlainLoopbackLifecycleEvent::RecoveryRequired {
            connection_epoch: (self.connection_epoch != 0).then_some(self.connection_epoch),
            reason: cause,
            dropped_events,
        })?;
        Ok(PlainLoopbackLifecycleStep::RecoveryScheduled)
    }

    fn connect(&mut self) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        self.attempts_in_cycle = self.attempts_in_cycle.saturating_add(1);
        self.session.begin_reconnect();
        let stream =
            match TcpStream::connect_timeout(&self.endpoint.address(), self.config.io_timeout) {
                Ok(stream) => stream,
                Err(error) => return self.connection_attempt_failed(error.to_string()),
            };
        if let Err(error) = configure_plain_stream(&stream, self.config.io_timeout) {
            return self.connection_attempt_failed(error.to_string());
        }
        let connection = client_with_config(
            self.endpoint.as_str(),
            stream,
            Some(self.session.websocket_config()),
        );
        let (socket, response) = match connection {
            Ok(connection) => connection,
            Err(error) => return self.connection_attempt_failed(error.to_string()),
        };
        if response.status().as_u16() != 101 {
            return self.connection_attempt_failed(format!(
                "unexpected upgrade status {}",
                response.status()
            ));
        }
        self.connection_epoch = self
            .connection_epoch
            .checked_add(1)
            .ok_or(PlainLoopbackLifecycleError::ConnectionEpochOverflow)?;
        self.socket = Some(socket);
        self.state = PlainLoopbackLifecycleState::AwaitingSnapshot {
            connection_epoch: self.connection_epoch,
        };
        self.emit_event(PlainLoopbackLifecycleEvent::Connected {
            connection_epoch: self.connection_epoch,
            attempt: self.attempts_in_cycle,
        })?;
        Ok(PlainLoopbackLifecycleStep::Connected)
    }

    fn connection_attempt_failed(
        &mut self,
        _error: String,
    ) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        self.socket = None;
        self.session.notify_transport_failure();
        let cause = *self
            .recovery_cause
            .get_or_insert(WebSocketRecoveryReason::TransportFailure);
        let limit = self.config.connection_attempt_limit.get();
        if self.attempts_in_cycle >= limit {
            self.state = PlainLoopbackLifecycleState::Exhausted {
                attempts: self.attempts_in_cycle,
                reason: cause,
            };
            self.emit_reconnect_exhausted(self.attempts_in_cycle, cause, 0);
            return Ok(PlainLoopbackLifecycleStep::ReconnectExhausted);
        }
        self.state = PlainLoopbackLifecycleState::ReconnectPending {
            reason: cause,
            attempts: self.attempts_in_cycle,
        };
        self.emit_event(PlainLoopbackLifecycleEvent::ConnectionAttemptFailed {
            attempt: self.attempts_in_cycle,
            remaining: limit.saturating_sub(self.attempts_in_cycle),
        })?;
        Ok(PlainLoopbackLifecycleStep::ConnectionAttemptFailed)
    }

    fn read_message(&mut self) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        let required = self.session.publication_capacity();
        let available = self
            .config
            .event_capacity
            .get()
            .saturating_sub(self.events.len());
        if available < required {
            self.force_event_overflow(0);
            return Err(PlainLoopbackLifecycleError::EventQueueFull {
                requested: self.events.len().saturating_add(required),
                maximum: self.config.event_capacity.get(),
            });
        }
        let result = self
            .socket
            .as_mut()
            .ok_or(PlainLoopbackLifecycleError::MissingConnectedSocket)?
            .read();
        let Ok(message) = result else {
            self.session.notify_transport_failure();
            self.schedule_recovery(WebSocketRecoveryReason::TransportFailure)?;
            return Ok(PlainLoopbackLifecycleStep::RecoveryScheduled);
        };
        let Ok(outcome) = self.session.accept_message(message) else {
            let reason = match self.session.state() {
                MarketWebSocketState::RecoveryRequired(reason) => reason,
                MarketWebSocketState::AwaitingSnapshot | MarketWebSocketState::Streaming => {
                    WebSocketRecoveryReason::TransportFailure
                }
            };
            self.schedule_recovery(reason)?;
            return Ok(PlainLoopbackLifecycleStep::RecoveryScheduled);
        };
        self.finish_message(outcome)
    }

    fn finish_message(
        &mut self,
        outcome: WebSocketMessageOutcome,
    ) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        match outcome {
            WebSocketMessageOutcome::Ping => {
                if self
                    .socket
                    .as_mut()
                    .ok_or(PlainLoopbackLifecycleError::MissingConnectedSocket)?
                    .flush()
                    .is_err()
                {
                    self.session.notify_transport_failure();
                    self.schedule_recovery(WebSocketRecoveryReason::TransportFailure)?;
                    return Ok(PlainLoopbackLifecycleStep::RecoveryScheduled);
                }
                self.emit_event(PlainLoopbackLifecycleEvent::Control(
                    WebSocketControlSignal::Ping,
                ))?;
            }
            WebSocketMessageOutcome::Pong => {
                self.emit_event(PlainLoopbackLifecycleEvent::Control(
                    WebSocketControlSignal::Pong,
                ))?;
            }
            WebSocketMessageOutcome::BinaryAccepted { .. } => {
                self.publish_session_updates()?;
                if self.session.state() == MarketWebSocketState::Streaming {
                    self.attempts_in_cycle = 0;
                    self.recovery_cause = None;
                    self.state = PlainLoopbackLifecycleState::Streaming {
                        connection_epoch: self.connection_epoch,
                    };
                }
            }
        }
        Ok(PlainLoopbackLifecycleStep::MessageAccepted)
    }

    fn publish_session_updates(&mut self) -> Result<(), PlainLoopbackLifecycleError> {
        let pending = self.session.queued_publications();
        let available = self
            .config
            .event_capacity
            .get()
            .saturating_sub(self.events.len());
        if pending > available {
            self.force_event_overflow(0);
            return Err(PlainLoopbackLifecycleError::EventQueueFull {
                requested: self.events.len().saturating_add(pending),
                maximum: self.config.event_capacity.get(),
            });
        }
        let Some(maximum) = NonZeroUsize::new(pending) else {
            return Ok(());
        };
        for publication in self.session.drain_publications(maximum) {
            self.events
                .push_back(PlainLoopbackLifecycleEvent::Publication(Box::new(
                    publication,
                )));
        }
        Ok(())
    }

    fn schedule_recovery(
        &mut self,
        reason: WebSocketRecoveryReason,
    ) -> Result<(), PlainLoopbackLifecycleError> {
        self.socket = None;
        let cause = *self.recovery_cause.get_or_insert(reason);
        self.install_recovery_state(cause);
        match self.state {
            PlainLoopbackLifecycleState::Exhausted { attempts, reason } => {
                self.emit_reconnect_exhausted(attempts, reason, 0);
            }
            _ => self.emit_event(PlainLoopbackLifecycleEvent::RecoveryRequired {
                connection_epoch: (self.connection_epoch != 0).then_some(self.connection_epoch),
                reason: cause,
                dropped_events: 0,
            })?,
        }
        Ok(())
    }

    fn force_event_overflow(&mut self, additional_dropped: usize) {
        self.socket = None;
        self.session.notify_publication_queue_overflow();
        let dropped_events = self.events.len().saturating_add(additional_dropped);
        self.events.clear();
        let cause = *self
            .recovery_cause
            .get_or_insert(WebSocketRecoveryReason::PublicationQueueOverflow);
        self.install_recovery_state(cause);
        match self.state {
            PlainLoopbackLifecycleState::Exhausted { attempts, reason } => {
                self.emit_reconnect_exhausted(attempts, reason, dropped_events);
            }
            _ => self
                .events
                .push_back(PlainLoopbackLifecycleEvent::RecoveryRequired {
                    connection_epoch: (self.connection_epoch != 0).then_some(self.connection_epoch),
                    reason: cause,
                    dropped_events,
                }),
        }
    }

    fn install_recovery_state(&mut self, reason: WebSocketRecoveryReason) {
        if self.attempts_in_cycle >= self.config.connection_attempt_limit.get() {
            self.state = PlainLoopbackLifecycleState::Exhausted {
                attempts: self.attempts_in_cycle,
                reason,
            };
        } else {
            self.state = PlainLoopbackLifecycleState::ReconnectPending {
                reason,
                attempts: self.attempts_in_cycle,
            };
        }
    }

    fn emit_reconnect_exhausted(
        &mut self,
        attempts: usize,
        reason: WebSocketRecoveryReason,
        mut dropped_events: usize,
    ) {
        if self.events.len() >= self.config.event_capacity.get() {
            dropped_events = dropped_events.saturating_add(self.events.len());
            self.events.clear();
        }
        self.events
            .push_back(PlainLoopbackLifecycleEvent::ReconnectExhausted {
                attempts,
                reason,
                dropped_events,
            });
    }

    fn emit_event(
        &mut self,
        event: PlainLoopbackLifecycleEvent,
    ) -> Result<(), PlainLoopbackLifecycleError> {
        if self.events.len() >= self.config.event_capacity.get() {
            self.force_event_overflow(1);
            return Err(PlainLoopbackLifecycleError::EventQueueFull {
                requested: self.events.len().saturating_add(1),
                maximum: self.config.event_capacity.get(),
            });
        }
        self.events.push_back(event);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<PlainLoopbackLifecycleStep, PlainLoopbackLifecycleError> {
        let close_handshake_complete = self.socket.take().is_none_or(|mut socket| {
            complete_close_handshake(&mut socket, PLAIN_LOOPBACK_CLOSE_TIMEOUT)
        });
        self.commands.clear();
        let dropped_events = self.events.len();
        self.events.clear();
        self.events.push_back(PlainLoopbackLifecycleEvent::Stopped {
            close_handshake_complete,
            dropped_events,
        });
        self.state = PlainLoopbackLifecycleState::Stopped;
        if close_handshake_complete {
            Ok(PlainLoopbackLifecycleStep::Stopped)
        } else {
            Err(PlainLoopbackLifecycleError::CloseHandshakeIncomplete)
        }
    }

    /// Removes at most `maximum` lifecycle events in production order.
    #[must_use]
    pub fn drain_events(&mut self, maximum: NonZeroUsize) -> Vec<PlainLoopbackLifecycleEvent> {
        let count = maximum.get().min(self.events.len());
        self.events.drain(..count).collect()
    }

    #[must_use]
    pub const fn state(&self) -> PlainLoopbackLifecycleState {
        self.state
    }

    #[must_use]
    pub fn queued_commands(&self) -> usize {
        self.commands.len()
    }

    #[must_use]
    pub fn queued_events(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub const fn event_capacity(&self) -> usize {
        self.config.event_capacity.get()
    }

    #[must_use]
    pub const fn current_generation(&self) -> Option<&MarketGeneration<ProvenancedMarketBar>> {
        self.session.current_generation()
    }
}

fn configure_plain_stream(stream: &TcpStream, timeout: Duration) -> Result<(), std::io::Error> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))
}

fn complete_close_handshake(socket: &mut PlainLoopbackSocket, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    if socket.get_mut().set_read_timeout(Some(timeout)).is_err()
        || socket.get_mut().set_write_timeout(Some(timeout)).is_err()
        || socket.close(None).is_err()
        || socket.flush().is_err()
    {
        return false;
    }
    for _ in 0..4 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || socket.get_mut().set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        match socket.read() {
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// Failures surfaced by the bounded plain-loopback lifecycle owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlainLoopbackLifecycleError {
    AlreadyStopped,
    CommandQueueFull {
        maximum: usize,
    },
    EventQueueFull {
        requested: usize,
        maximum: usize,
    },
    EventCapacityBelowPublicationCapacity {
        event_capacity: usize,
        publication_capacity: usize,
    },
    ConnectWhileActive(PlainLoopbackLifecycleState),
    MissingConnectedSocket,
    CloseHandshakeIncomplete,
    ConnectionEpochOverflow,
}

impl fmt::Display for PlainLoopbackLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plain loopback WebSocket lifecycle failed: {self:?}"
        )
    }
}

impl Error for PlainLoopbackLifecycleError {}
