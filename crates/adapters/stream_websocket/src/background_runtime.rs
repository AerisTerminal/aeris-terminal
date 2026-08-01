//! Background thread bridge exposing the plain-loopback owner as a neutral runtime port.
//!
//! This module owns the bounded command and event channels, the worker loop that drives
//! the lifecycle owner, translation from adapter lifecycle events into neutral market
//! stream events, and shutdown/join behavior. It never touches WebSocket framing
//! directly; it drives the owner in `plain_loopback_owner`.

use crate::plain_loopback_owner::{
    PLAIN_LOOPBACK_RUNTIME_JOIN_TIMEOUT, PlainLoopbackCommand, PlainLoopbackLifecycleError,
    PlainLoopbackLifecycleEvent, PlainLoopbackLifecycleState, PlainLoopbackMarketWebSocketOwner,
    WebSocketControlSignal,
};
use crate::session::{MarketWebSocketPublication, WebSocketRecoveryReason};
use axiusflow_application::{
    MarketStreamCommand, MarketStreamCommandOffer, MarketStreamControlSignal, MarketStreamEvent,
    MarketStreamPublication, MarketStreamPublicationError, MarketStreamRuntimePort,
    ReplayStreamUpdate, ResnapshotReason,
};
use core::fmt;
use std::{
    error::Error,
    num::NonZeroUsize,
    sync::mpsc::{
        Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Bounds for the plain-loopback background owner bridge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainLoopbackRuntimeConfig {
    command_capacity: NonZeroUsize,
    event_capacity: NonZeroUsize,
    backpressure_wait: Duration,
}

impl PlainLoopbackRuntimeConfig {
    /// Creates bounded worker-channel limits.
    ///
    /// # Errors
    ///
    /// Returns an error when the backpressure polling interval is zero.
    pub fn try_new(
        command_capacity: NonZeroUsize,
        event_capacity: NonZeroUsize,
        backpressure_wait: Duration,
    ) -> Result<Self, PlainLoopbackRuntimeConfigError> {
        if backpressure_wait.is_zero() {
            return Err(PlainLoopbackRuntimeConfigError::ZeroBackpressureWait);
        }
        Ok(Self {
            command_capacity,
            event_capacity,
            backpressure_wait,
        })
    }
}

/// Invalid plain-loopback background runtime limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackRuntimeConfigError {
    ZeroBackpressureWait,
}

impl fmt::Display for PlainLoopbackRuntimeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plain loopback background runtime configuration rejected: {self:?}"
        )
    }
}

impl Error for PlainLoopbackRuntimeConfigError {}

/// Commands accepted by the bounded background owner thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackRuntimeCommand {
    Connect,
    Recover {
        request_id: u64,
        reason: ResnapshotReason,
    },
    Shutdown,
}

/// Background lifecycle output suitable for a bounded application/UI bridge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlainLoopbackRuntimeEvent {
    Connected {
        connection_epoch: u64,
        attempt: usize,
    },
    Publication(Box<MarketWebSocketPublication>),
    RecoverySnapshot {
        request_id: u64,
        publication: Box<MarketWebSocketPublication>,
    },
    Control(WebSocketControlSignal),
    StreamInvalid {
        connection_epoch: Option<u64>,
        reason: WebSocketRecoveryReason,
        dropped_events: usize,
    },
    ConnectionAttemptFailed {
        request_id: Option<u64>,
        attempt: usize,
        remaining: usize,
    },
    ReconnectExhausted {
        request_id: Option<u64>,
        attempts: usize,
        reason: WebSocketRecoveryReason,
        dropped_events: usize,
    },
    RecoveryRejected {
        request_id: u64,
        active_request_id: u64,
    },
    WorkerFailed(PlainLoopbackLifecycleError),
    Stopped {
        close_handshake_complete: bool,
        dropped_events: usize,
    },
}

/// Failures that prevent the bounded background owner thread from starting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlainLoopbackRuntimeStartError {
    EventCapacityBelowOwnerCapacity {
        runtime_event_capacity: usize,
        owner_event_capacity: usize,
    },
    ThreadSpawn(String),
}

impl fmt::Display for PlainLoopbackRuntimeStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plain loopback background runtime failed to start: {self:?}"
        )
    }
}

impl Error for PlainLoopbackRuntimeStartError {}

/// Failure returned when the background owner thread panics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackRuntimeJoinError {
    ShutdownDeadlineExceeded,
    WorkerPanicked,
}

impl fmt::Display for PlainLoopbackRuntimeJoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plain loopback background runtime join failed: {self:?}"
        )
    }
}

impl Error for PlainLoopbackRuntimeJoinError {}

/// Failure exposed through the provider-neutral runtime port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlainLoopbackRuntimePortError {
    CommandDisconnected,
    EventDisconnected,
    InvalidPublication(MarketStreamPublicationError),
    WorkerFailed(PlainLoopbackLifecycleError),
}

impl fmt::Display for PlainLoopbackRuntimePortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "plain loopback runtime port failed: {self:?}")
    }
}

impl Error for PlainLoopbackRuntimePortError {}

/// Bounded worker handle for the blocking plain-loopback connection owner.
///
/// The worker is software loopback evidence only. It keeps socket reads, decoding,
/// and model mutation off the consumer thread, but is not an evented production
/// runtime and does not add TLS, authentication, entitlement, or provider support.
pub struct PlainLoopbackBackgroundRuntime {
    commands: Option<SyncSender<PlainLoopbackRuntimeCommand>>,
    events: Option<Receiver<PlainLoopbackRuntimeEvent>>,
    worker: Option<JoinHandle<()>>,
}

impl PlainLoopbackBackgroundRuntime {
    /// Starts one background thread with exclusive custody of the socket owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime output queue cannot hold one complete owner
    /// event burst or when the operating system cannot create the worker thread.
    pub fn try_start(
        owner: PlainLoopbackMarketWebSocketOwner,
        config: PlainLoopbackRuntimeConfig,
    ) -> Result<Self, PlainLoopbackRuntimeStartError> {
        let owner_event_capacity = owner.event_capacity();
        if config.event_capacity.get() < owner_event_capacity {
            return Err(
                PlainLoopbackRuntimeStartError::EventCapacityBelowOwnerCapacity {
                    runtime_event_capacity: config.event_capacity.get(),
                    owner_event_capacity,
                },
            );
        }
        let (command_tx, command_rx) = sync_channel(config.command_capacity.get());
        let (event_tx, event_rx) = sync_channel(config.event_capacity.get());
        let worker = thread::Builder::new()
            .name("axiusflow-plain-loopback-market-runtime".to_string())
            .spawn(move || run_plain_loopback_runtime(owner, config, &command_rx, &event_tx))
            .map_err(|error| PlainLoopbackRuntimeStartError::ThreadSpawn(error.to_string()))?;
        Ok(Self {
            commands: Some(command_tx),
            events: Some(event_rx),
            worker: Some(worker),
        })
    }

    /// Offers one command without blocking or growing memory.
    ///
    /// # Errors
    ///
    /// Returns the standard bounded-channel full or disconnected result.
    pub fn try_send_command(
        &self,
        command: PlainLoopbackRuntimeCommand,
    ) -> Result<(), TrySendError<PlainLoopbackRuntimeCommand>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(command));
        };
        commands.try_send(command)
    }

    /// Receives one worker event without blocking.
    ///
    /// # Errors
    ///
    /// Returns the standard empty or disconnected result.
    pub fn try_recv_event(&self) -> Result<PlainLoopbackRuntimeEvent, TryRecvError> {
        let Some(events) = self.events.as_ref() else {
            return Err(TryRecvError::Disconnected);
        };
        events.try_recv()
    }

    /// Waits at most `timeout` for one worker event.
    ///
    /// # Errors
    ///
    /// Returns the standard timeout or disconnected result.
    pub fn recv_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<PlainLoopbackRuntimeEvent, RecvTimeoutError> {
        let Some(events) = self.events.as_ref() else {
            return Err(RecvTimeoutError::Disconnected);
        };
        events.recv_timeout(timeout)
    }

    /// Disconnects both bounded channels and waits only for the hard runtime join deadline.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker panics or does not stop before the deadline.
    pub fn join(mut self) -> Result<(), PlainLoopbackRuntimeJoinError> {
        self.events.take();
        self.commands.take();
        join_runtime_worker(self.worker.take(), PLAIN_LOOPBACK_RUNTIME_JOIN_TIMEOUT)
    }
}

impl Drop for PlainLoopbackBackgroundRuntime {
    fn drop(&mut self) {
        self.events.take();
        self.commands.take();
        self.worker.take();
    }
}

impl MarketStreamRuntimePort for PlainLoopbackBackgroundRuntime {
    type Error = PlainLoopbackRuntimePortError;

    fn try_send_stream_command(
        &self,
        command: MarketStreamCommand,
    ) -> Result<MarketStreamCommandOffer, Self::Error> {
        let command = match command {
            MarketStreamCommand::Connect => PlainLoopbackRuntimeCommand::Connect,
            MarketStreamCommand::Recover { request_id, reason } => {
                PlainLoopbackRuntimeCommand::Recover { request_id, reason }
            }
            MarketStreamCommand::Shutdown => PlainLoopbackRuntimeCommand::Shutdown,
        };
        match self.try_send_command(command) {
            Ok(()) => Ok(MarketStreamCommandOffer::Accepted),
            Err(TrySendError::Full(_)) => Ok(MarketStreamCommandOffer::Full),
            Err(TrySendError::Disconnected(_)) => {
                Err(PlainLoopbackRuntimePortError::CommandDisconnected)
            }
        }
    }

    fn try_recv_stream_event(&self) -> Result<Option<MarketStreamEvent>, Self::Error> {
        match self.try_recv_event() {
            Ok(PlainLoopbackRuntimeEvent::WorkerFailed(error)) => {
                Err(PlainLoopbackRuntimePortError::WorkerFailed(error))
            }
            Ok(event) => translate_neutral_runtime_event(event)
                .map(Some)
                .map_err(PlainLoopbackRuntimePortError::InvalidPublication),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(PlainLoopbackRuntimePortError::EventDisconnected)
            }
        }
    }
}

fn translate_neutral_runtime_event(
    event: PlainLoopbackRuntimeEvent,
) -> Result<MarketStreamEvent, MarketStreamPublicationError> {
    let event = match event {
        PlainLoopbackRuntimeEvent::Connected {
            connection_epoch,
            attempt,
        } => MarketStreamEvent::Connected {
            connection_epoch,
            attempt,
        },
        PlainLoopbackRuntimeEvent::Publication(publication) => {
            MarketStreamEvent::Publication(Box::new(neutral_publication(*publication)?))
        }
        PlainLoopbackRuntimeEvent::RecoverySnapshot {
            request_id,
            publication,
        } => MarketStreamEvent::RecoverySnapshot {
            request_id,
            publication: Box::new(neutral_publication(*publication)?),
        },
        PlainLoopbackRuntimeEvent::Control(signal) => MarketStreamEvent::Control(match signal {
            WebSocketControlSignal::Ping => MarketStreamControlSignal::KeepaliveRequest,
            WebSocketControlSignal::Pong => MarketStreamControlSignal::KeepaliveResponse,
        }),
        PlainLoopbackRuntimeEvent::StreamInvalid {
            connection_epoch,
            reason,
            dropped_events,
        } => MarketStreamEvent::StreamInvalid {
            connection_epoch,
            reason: neutral_resnapshot_reason(reason),
            dropped_events,
        },
        PlainLoopbackRuntimeEvent::ConnectionAttemptFailed {
            request_id,
            attempt,
            remaining,
        } => MarketStreamEvent::RecoveryAttemptFailed {
            request_id,
            attempt,
            remaining,
        },
        PlainLoopbackRuntimeEvent::ReconnectExhausted {
            request_id,
            attempts,
            reason,
            dropped_events,
        } => MarketStreamEvent::RecoveryExhausted {
            request_id,
            attempts,
            reason: neutral_resnapshot_reason(reason),
            dropped_events,
        },
        PlainLoopbackRuntimeEvent::RecoveryRejected {
            request_id,
            active_request_id,
        } => MarketStreamEvent::RecoveryRejected {
            request_id,
            active_request_id,
        },
        PlainLoopbackRuntimeEvent::Stopped {
            close_handshake_complete,
            dropped_events,
        } => MarketStreamEvent::Stopped {
            graceful: close_handshake_complete,
            dropped_events,
        },
        PlainLoopbackRuntimeEvent::WorkerFailed(_) => {
            unreachable!("worker failures are returned as runtime port errors")
        }
    };
    Ok(event)
}

fn neutral_publication(
    publication: MarketWebSocketPublication,
) -> Result<MarketStreamPublication, MarketStreamPublicationError> {
    MarketStreamPublication::try_new(
        publication.subscription_id,
        publication.update,
        publication.generation,
    )
}

const fn neutral_resnapshot_reason(reason: WebSocketRecoveryReason) -> ResnapshotReason {
    match reason {
        WebSocketRecoveryReason::ModelRecovery(reason) => reason,
        WebSocketRecoveryReason::PublicationQueueOverflow => ResnapshotReason::QueueOverflow,
        WebSocketRecoveryReason::PeerClosed | WebSocketRecoveryReason::TransportFailure => {
            ResnapshotReason::TransportReset
        }
        WebSocketRecoveryReason::MessageTooLarge
        | WebSocketRecoveryReason::TextMessage
        | WebSocketRecoveryReason::UnexpectedRawFrame
        | WebSocketRecoveryReason::DecodeFailure
        | WebSocketRecoveryReason::SubscriptionMismatch => ResnapshotReason::TransportReset,
    }
}

#[derive(Clone, Copy)]
struct PendingRuntimeRecovery {
    request_id: u64,
}

fn run_plain_loopback_runtime(
    mut owner: PlainLoopbackMarketWebSocketOwner,
    config: PlainLoopbackRuntimeConfig,
    commands: &Receiver<PlainLoopbackRuntimeCommand>,
    events: &SyncSender<PlainLoopbackRuntimeEvent>,
) {
    let mut pending_output = None;
    let mut pending_recovery = None;
    let mut stop_after_events = false;
    loop {
        if !flush_runtime_output(
            &mut owner,
            events,
            config.backpressure_wait,
            &mut pending_output,
        ) {
            return;
        }
        if pending_output.is_some() {
            continue;
        }
        if owner.queued_events() != 0 {
            pending_output = drain_runtime_event(&mut owner, &mut pending_recovery);
            continue;
        }
        if stop_after_events {
            return;
        }
        match commands.try_recv() {
            Ok(command) => {
                let (stop, output) =
                    apply_runtime_command(&mut owner, command, &mut pending_recovery);
                stop_after_events |= stop;
                pending_output = output;
            }
            Err(TryRecvError::Disconnected) => {
                stop_runtime_owner(&mut owner);
                return;
            }
            Err(TryRecvError::Empty) => match owner.state() {
                PlainLoopbackLifecycleState::Idle
                | PlainLoopbackLifecycleState::Exhausted { .. } => {
                    match commands.recv_timeout(config.backpressure_wait) {
                        Ok(command) => {
                            let (stop, output) =
                                apply_runtime_command(&mut owner, command, &mut pending_recovery);
                            stop_after_events |= stop;
                            pending_output = output;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            stop_runtime_owner(&mut owner);
                            return;
                        }
                    }
                }
                PlainLoopbackLifecycleState::Stopped => return,
                _ => {
                    if let Err(error) = owner.step() {
                        pending_output = Some(PlainLoopbackRuntimeEvent::WorkerFailed(error));
                    }
                }
            },
        }
    }
}

fn flush_runtime_output(
    owner: &mut PlainLoopbackMarketWebSocketOwner,
    events: &SyncSender<PlainLoopbackRuntimeEvent>,
    wait: Duration,
    pending_output: &mut Option<PlainLoopbackRuntimeEvent>,
) -> bool {
    let Some(output) = pending_output.take() else {
        return true;
    };
    match events.try_send(output) {
        Ok(()) => true,
        Err(TrySendError::Disconnected(_)) => {
            stop_runtime_owner(owner);
            false
        }
        Err(TrySendError::Full(output)) => {
            *pending_output = Some(output);
            thread::sleep(wait);
            true
        }
    }
}

fn apply_runtime_command(
    owner: &mut PlainLoopbackMarketWebSocketOwner,
    command: PlainLoopbackRuntimeCommand,
    pending_recovery: &mut Option<PendingRuntimeRecovery>,
) -> (bool, Option<PlainLoopbackRuntimeEvent>) {
    let owner_command = match command {
        PlainLoopbackRuntimeCommand::Connect => PlainLoopbackCommand::Connect,
        PlainLoopbackRuntimeCommand::Recover { request_id, reason } => {
            if let Some(active) = pending_recovery
                && active.request_id != request_id
            {
                return (
                    false,
                    Some(PlainLoopbackRuntimeEvent::RecoveryRejected {
                        request_id,
                        active_request_id: active.request_id,
                    }),
                );
            }
            *pending_recovery = Some(PendingRuntimeRecovery { request_id });
            PlainLoopbackCommand::Recover { reason }
        }
        PlainLoopbackRuntimeCommand::Shutdown => PlainLoopbackCommand::Shutdown,
    };
    let stop = command == PlainLoopbackRuntimeCommand::Shutdown;
    let result = owner
        .try_enqueue_command(owner_command)
        .and_then(|()| owner.step().map(|_| ()));
    (
        stop,
        result.err().map(PlainLoopbackRuntimeEvent::WorkerFailed),
    )
}

fn drain_runtime_event(
    owner: &mut PlainLoopbackMarketWebSocketOwner,
    pending_recovery: &mut Option<PendingRuntimeRecovery>,
) -> Option<PlainLoopbackRuntimeEvent> {
    owner
        .drain_events(NonZeroUsize::MIN)
        .pop()
        .map(|event| translate_runtime_event(event, pending_recovery))
}

fn translate_runtime_event(
    event: PlainLoopbackLifecycleEvent,
    pending_recovery: &mut Option<PendingRuntimeRecovery>,
) -> PlainLoopbackRuntimeEvent {
    match event {
        PlainLoopbackLifecycleEvent::Connected {
            connection_epoch,
            attempt,
        } => PlainLoopbackRuntimeEvent::Connected {
            connection_epoch,
            attempt,
        },
        PlainLoopbackLifecycleEvent::Publication(publication) => {
            if matches!(&publication.update, ReplayStreamUpdate::Snapshot(_))
                && let Some(recovery) = pending_recovery.take()
            {
                PlainLoopbackRuntimeEvent::RecoverySnapshot {
                    request_id: recovery.request_id,
                    publication,
                }
            } else {
                PlainLoopbackRuntimeEvent::Publication(publication)
            }
        }
        PlainLoopbackLifecycleEvent::Control(signal) => PlainLoopbackRuntimeEvent::Control(signal),
        PlainLoopbackLifecycleEvent::RecoveryRequired {
            connection_epoch,
            reason,
            dropped_events,
        } => PlainLoopbackRuntimeEvent::StreamInvalid {
            connection_epoch,
            reason,
            dropped_events,
        },
        PlainLoopbackLifecycleEvent::ConnectionAttemptFailed { attempt, remaining } => {
            PlainLoopbackRuntimeEvent::ConnectionAttemptFailed {
                request_id: pending_recovery.map(|recovery| recovery.request_id),
                attempt,
                remaining,
            }
        }
        PlainLoopbackLifecycleEvent::ReconnectExhausted {
            attempts,
            reason,
            dropped_events,
        } => PlainLoopbackRuntimeEvent::ReconnectExhausted {
            request_id: pending_recovery.take().map(|recovery| recovery.request_id),
            attempts,
            reason,
            dropped_events,
        },
        PlainLoopbackLifecycleEvent::Stopped {
            close_handshake_complete,
            dropped_events,
        } => PlainLoopbackRuntimeEvent::Stopped {
            close_handshake_complete,
            dropped_events,
        },
    }
}

fn stop_runtime_owner(owner: &mut PlainLoopbackMarketWebSocketOwner) {
    if owner.state() != PlainLoopbackLifecycleState::Stopped {
        let _ = owner.try_enqueue_command(PlainLoopbackCommand::Shutdown);
        let _ = owner.step();
    }
}

fn join_runtime_worker(
    worker: Option<JoinHandle<()>>,
    timeout: Duration,
) -> Result<(), PlainLoopbackRuntimeJoinError> {
    let Some(worker) = worker else {
        return Ok(());
    };
    let deadline = Instant::now() + timeout;
    while !worker.is_finished() {
        if Instant::now() >= deadline {
            return Err(PlainLoopbackRuntimeJoinError::ShutdownDeadlineExceeded);
        }
        thread::sleep(Duration::from_millis(1));
    }
    worker
        .join()
        .map_err(|_| PlainLoopbackRuntimeJoinError::WorkerPanicked)
}
