//! Bounded binary WebSocket message adapter for market-bar client sessions.
//!
//! This crate validates WebSocket message semantics and feeds the existing bounded
//! binary decoder and single-writer client model. It does not establish TLS,
//! authenticate, enforce entitlements, connect to a provider, or claim production
//! transport readiness.

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketGeneration, MarketStreamCommand,
    MarketStreamCommandOffer, MarketStreamControlSignal, MarketStreamEvent,
    MarketStreamPublication, MarketStreamPublicationError, MarketStreamRuntimePort,
    ProvenancedMarketBar, ReplayProvenance, ReplayStreamUpdate, ReplayValidationError,
    ResnapshotReason,
};
use axiusflow_market_protocol_adapter::{
    BinaryMarketBarStreamDecoder, BinaryMarketStreamError, DecimalConvention,
};
use core::fmt;
use std::{
    collections::VecDeque,
    error::Error,
    net::{IpAddr, SocketAddr, TcpStream},
    num::NonZeroUsize,
    sync::mpsc::{
        Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tungstenite::{
    Message, WebSocket, client::client_with_config, http::Uri, protocol::WebSocketConfig,
};

/// Explicit memory and publication bounds for one WebSocket market session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketWebSocketConfig {
    message_bytes: NonZeroUsize,
    frame_bytes: NonZeroUsize,
    buffered_bytes: NonZeroUsize,
    publication_capacity: NonZeroUsize,
    model_item_capacity: NonZeroUsize,
}

impl MarketWebSocketConfig {
    /// Creates one configuration after checking that a message fits the decoder buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the WebSocket message bound exceeds the binary decoder's
    /// total buffered-byte bound.
    pub fn try_new(
        message_bytes: NonZeroUsize,
        frame_bytes: NonZeroUsize,
        buffered_bytes: NonZeroUsize,
        publication_capacity: NonZeroUsize,
        model_item_capacity: NonZeroUsize,
    ) -> Result<Self, MarketWebSocketConfigError> {
        if message_bytes.get() > buffered_bytes.get() {
            return Err(MarketWebSocketConfigError::MessageExceedsDecoderBuffer {
                message: message_bytes.get(),
                buffered: buffered_bytes.get(),
            });
        }
        Ok(Self {
            message_bytes,
            frame_bytes,
            buffered_bytes,
            publication_capacity,
            model_item_capacity,
        })
    }

    /// Produces the matching bounded tungstenite configuration.
    #[must_use]
    pub fn websocket_config(self) -> WebSocketConfig {
        WebSocketConfig::default()
            .read_buffer_size(self.message_bytes.get().min(64 * 1_024))
            .write_buffer_size(0)
            .max_write_buffer_size(self.message_bytes.get().saturating_add(128))
            .max_message_size(Some(self.message_bytes.get()))
            .max_frame_size(Some(self.message_bytes.get()))
    }

    #[must_use]
    pub const fn message_bytes(self) -> usize {
        self.message_bytes.get()
    }

    #[must_use]
    pub const fn publication_capacity(self) -> usize {
        self.publication_capacity.get()
    }
}

/// Invalid combinations of WebSocket and decoder bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketWebSocketConfigError {
    MessageExceedsDecoderBuffer { message: usize, buffered: usize },
}

impl fmt::Display for MarketWebSocketConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "market WebSocket configuration rejected: {self:?}"
        )
    }
}

impl Error for MarketWebSocketConfigError {}

/// Observable state of one bounded WebSocket market session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketWebSocketState {
    AwaitingSnapshot,
    Streaming,
    RecoveryRequired(WebSocketRecoveryReason),
}

/// Transport-level reason ordered mutation is blocked until reconnect and resnapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSocketRecoveryReason {
    PeerClosed,
    TransportFailure,
    MessageTooLarge,
    TextMessage,
    UnexpectedRawFrame,
    DecodeFailure,
    SubscriptionMismatch,
    ModelRecovery(ResnapshotReason),
    PublicationQueueOverflow,
}

/// One validated update paired with the immutable generation it published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketWebSocketPublication {
    pub subscription_id: String,
    pub update: ReplayStreamUpdate,
    pub generation: MarketGeneration<ProvenancedMarketBar>,
}

/// Result of accepting one WebSocket message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSocketMessageOutcome {
    BinaryAccepted { published: usize },
    Ping,
    Pong,
}

/// Single-writer bounded adapter from WebSocket messages to immutable generations.
#[derive(Debug, PartialEq)]
pub struct MarketWebSocketSession {
    subscription_id: String,
    config: MarketWebSocketConfig,
    decoder: BinaryMarketBarStreamDecoder,
    model: MarketBarClientModel,
    publications: VecDeque<MarketWebSocketPublication>,
    state: MarketWebSocketState,
}

impl MarketWebSocketSession {
    /// Creates a disconnected logical session that requires an initial snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty subscription identifier or invalid decoder bounds.
    pub fn try_new(
        subscription_id: impl Into<String>,
        decimal_convention: DecimalConvention,
        provenance: ReplayProvenance,
        config: MarketWebSocketConfig,
    ) -> Result<Self, MarketWebSocketError> {
        let subscription_id = subscription_id.into();
        if subscription_id.trim().is_empty() {
            return Err(MarketWebSocketError::EmptySubscriptionId);
        }
        let decoder = BinaryMarketBarStreamDecoder::try_new(
            decimal_convention,
            provenance,
            config.frame_bytes,
            config.buffered_bytes,
        )?;
        Ok(Self {
            subscription_id,
            config,
            decoder,
            model: MarketBarClientModel::new(config.model_item_capacity),
            publications: VecDeque::with_capacity(config.publication_capacity.get()),
            state: MarketWebSocketState::AwaitingSnapshot,
        })
    }

    /// Accepts one bounded WebSocket message and atomically commits all resulting updates.
    ///
    /// Binary messages may contain partial or coalesced length-prefixed frames. Text,
    /// close, oversized, malformed, mismatched, or overflowing input latches recovery.
    /// Ping and Pong are lifecycle signals and do not mutate market state.
    ///
    /// # Errors
    ///
    /// Returns the fault that latched recovery, or `RecoveryRequired` until the caller
    /// establishes a new connection boundary with [`Self::begin_reconnect`].
    pub fn accept_message(
        &mut self,
        message: Message,
    ) -> Result<WebSocketMessageOutcome, MarketWebSocketError> {
        if let MarketWebSocketState::RecoveryRequired(reason) = self.state {
            return Err(MarketWebSocketError::RecoveryRequired(reason));
        }
        match message {
            Message::Binary(bytes) => self.accept_binary(bytes.as_ref()),
            Message::Ping(_) => Ok(WebSocketMessageOutcome::Ping),
            Message::Pong(_) => Ok(WebSocketMessageOutcome::Pong),
            Message::Text(_) => {
                self.latch_recovery(
                    WebSocketRecoveryReason::TextMessage,
                    ResnapshotReason::TransportReset,
                );
                Err(MarketWebSocketError::TextMessageRejected)
            }
            Message::Close(_) => {
                self.latch_recovery(
                    WebSocketRecoveryReason::PeerClosed,
                    ResnapshotReason::TransportReset,
                );
                Err(MarketWebSocketError::PeerClosed)
            }
            Message::Frame(_) => {
                self.latch_recovery(
                    WebSocketRecoveryReason::UnexpectedRawFrame,
                    ResnapshotReason::TransportReset,
                );
                Err(MarketWebSocketError::UnexpectedRawFrame)
            }
        }
    }

    fn accept_binary(
        &mut self,
        bytes: &[u8],
    ) -> Result<WebSocketMessageOutcome, MarketWebSocketError> {
        if bytes.len() > self.config.message_bytes.get() {
            let maximum = self.config.message_bytes.get();
            self.latch_recovery(
                WebSocketRecoveryReason::MessageTooLarge,
                ResnapshotReason::TransportReset,
            );
            return Err(MarketWebSocketError::MessageLimitExceeded {
                requested: bytes.len(),
                maximum,
            });
        }

        let mut candidate_decoder = self.decoder.clone();
        let projected = match candidate_decoder.push(bytes) {
            Ok(projected) => projected,
            Err(error) => {
                self.latch_recovery(
                    WebSocketRecoveryReason::DecodeFailure,
                    ResnapshotReason::TransportReset,
                );
                return Err(MarketWebSocketError::Decoder(error));
            }
        };
        let mut candidate_model = self.model.clone();
        let mut candidate_publications = Vec::with_capacity(projected.len());
        for projected_update in projected {
            if projected_update.subscription_id != self.subscription_id {
                let actual = projected_update.subscription_id;
                self.latch_recovery(
                    WebSocketRecoveryReason::SubscriptionMismatch,
                    ResnapshotReason::TransportReset,
                );
                return Err(MarketWebSocketError::SubscriptionMismatch {
                    expected: self.subscription_id.clone(),
                    actual,
                });
            }
            let update = projected_update.update;
            match candidate_model.apply_update(update.clone()) {
                Ok(MarketBarModelOutcome::Published(generation)) => {
                    candidate_publications.push(MarketWebSocketPublication {
                        subscription_id: self.subscription_id.clone(),
                        update,
                        generation,
                    });
                }
                Ok(MarketBarModelOutcome::Duplicate) => {}
                Ok(MarketBarModelOutcome::ResnapshotRequired(reason)) => {
                    self.latch_recovery(WebSocketRecoveryReason::ModelRecovery(reason), reason);
                    return Err(MarketWebSocketError::ModelRecoveryRequired(reason));
                }
                Err(error) => {
                    self.latch_recovery(
                        WebSocketRecoveryReason::DecodeFailure,
                        ResnapshotReason::TransportReset,
                    );
                    return Err(MarketWebSocketError::Application(error));
                }
            }
        }

        let requested = self
            .publications
            .len()
            .saturating_add(candidate_publications.len());
        if requested > self.config.publication_capacity.get() {
            let maximum = self.config.publication_capacity.get();
            self.latch_recovery(
                WebSocketRecoveryReason::PublicationQueueOverflow,
                ResnapshotReason::QueueOverflow,
            );
            return Err(MarketWebSocketError::PublicationQueueOverflow { requested, maximum });
        }

        let published = candidate_publications.len();
        self.decoder = candidate_decoder;
        self.model = candidate_model;
        self.publications.extend(candidate_publications);
        self.state = if self.model.requires_snapshot() {
            MarketWebSocketState::AwaitingSnapshot
        } else {
            MarketWebSocketState::Streaming
        };
        Ok(WebSocketMessageOutcome::BinaryAccepted { published })
    }

    /// Returns the bounded tungstenite configuration owned by the connection layer.
    #[must_use]
    pub fn websocket_config(&self) -> WebSocketConfig {
        self.config.websocket_config()
    }

    /// Marks a lifecycle publication queue overflow and blocks ordered mutation.
    pub fn notify_publication_queue_overflow(&mut self) {
        if !matches!(self.state, MarketWebSocketState::RecoveryRequired(_)) {
            self.latch_recovery(
                WebSocketRecoveryReason::PublicationQueueOverflow,
                ResnapshotReason::QueueOverflow,
            );
        }
    }

    /// Marks a socket/read failure and blocks further input until reconnect.
    pub fn notify_transport_failure(&mut self) {
        if !matches!(self.state, MarketWebSocketState::RecoveryRequired(_)) {
            self.latch_recovery(
                WebSocketRecoveryReason::TransportFailure,
                ResnapshotReason::TransportReset,
            );
        }
    }

    /// Marks a consumer-requested atomic resnapshot boundary.
    pub fn require_resnapshot(&mut self, reason: ResnapshotReason) {
        self.latch_recovery(WebSocketRecoveryReason::ModelRecovery(reason), reason);
    }

    /// Starts an explicit reconnect boundary and requires a replacement snapshot.
    pub fn begin_reconnect(&mut self) {
        self.decoder.reset();
        self.model
            .require_resnapshot(ResnapshotReason::TransportReset);
        self.publications.clear();
        self.state = MarketWebSocketState::AwaitingSnapshot;
    }

    /// Removes at most `maximum` validated publications from the bounded queue.
    #[must_use]
    pub fn drain_publications(&mut self, maximum: NonZeroUsize) -> Vec<MarketWebSocketPublication> {
        let count = maximum.get().min(self.publications.len());
        self.publications.drain(..count).collect()
    }

    #[must_use]
    pub const fn publication_capacity(&self) -> usize {
        self.config.publication_capacity.get()
    }

    #[must_use]
    pub const fn state(&self) -> MarketWebSocketState {
        self.state
    }

    #[must_use]
    pub fn queued_publications(&self) -> usize {
        self.publications.len()
    }

    #[must_use]
    pub const fn current_generation(&self) -> Option<&MarketGeneration<ProvenancedMarketBar>> {
        self.model.current_generation()
    }

    fn latch_recovery(&mut self, reason: WebSocketRecoveryReason, model_reason: ResnapshotReason) {
        self.decoder.reset();
        self.model.require_resnapshot(model_reason);
        self.publications.clear();
        self.state = MarketWebSocketState::RecoveryRequired(reason);
    }
}

/// Failures at the bounded WebSocket-to-client-model boundary.
#[derive(Debug)]
pub enum MarketWebSocketError {
    EmptySubscriptionId,
    MessageLimitExceeded { requested: usize, maximum: usize },
    TextMessageRejected,
    UnexpectedRawFrame,
    PeerClosed,
    SubscriptionMismatch { expected: String, actual: String },
    PublicationQueueOverflow { requested: usize, maximum: usize },
    ModelRecoveryRequired(ResnapshotReason),
    RecoveryRequired(WebSocketRecoveryReason),
    Decoder(BinaryMarketStreamError),
    Application(ReplayValidationError),
}

impl From<BinaryMarketStreamError> for MarketWebSocketError {
    fn from(error: BinaryMarketStreamError) -> Self {
        Self::Decoder(error)
    }
}

impl fmt::Display for MarketWebSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "market WebSocket session failed: {self:?}")
    }
}

impl Error for MarketWebSocketError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Decoder(error) => Some(error),
            Self::Application(error) => Some(error),
            _ => None,
        }
    }
}

/// Validated insecure endpoint restricted to the local machine for lifecycle evidence.
///
/// This type deliberately rejects `wss://`, hostnames, and non-loopback addresses. It
/// cannot be used as a production transport or as evidence for TLS behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlainLoopbackWebSocketEndpoint {
    uri: String,
    address: SocketAddr,
}

impl PlainLoopbackWebSocketEndpoint {
    /// Validates one explicit-port `ws://` URI whose host is a loopback IP address.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed URIs, credentials, non-plain schemes, missing
    /// ports, hostnames, or non-loopback IP addresses.
    pub fn try_new(value: &str) -> Result<Self, PlainLoopbackEndpointError> {
        let uri = value
            .parse::<Uri>()
            .map_err(|_| PlainLoopbackEndpointError::InvalidUri)?;
        if uri.scheme_str() != Some("ws") {
            return Err(PlainLoopbackEndpointError::PlainWebSocketRequired);
        }
        let authority = uri
            .authority()
            .ok_or(PlainLoopbackEndpointError::MissingAuthority)?;
        if authority.as_str().contains('@') {
            return Err(PlainLoopbackEndpointError::CredentialsForbidden);
        }
        let port = uri
            .port_u16()
            .ok_or(PlainLoopbackEndpointError::ExplicitPortRequired)?;
        let host = uri.host().ok_or(PlainLoopbackEndpointError::MissingHost)?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let address = host
            .parse::<IpAddr>()
            .map_err(|_| PlainLoopbackEndpointError::IpAddressRequired)?;
        if !address.is_loopback() {
            return Err(PlainLoopbackEndpointError::LoopbackRequired(address));
        }
        Ok(Self {
            uri: uri.to_string(),
            address: SocketAddr::new(address, port),
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.uri
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }
}

/// Endpoint validation failures for the plain-loopback evidence connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackEndpointError {
    InvalidUri,
    PlainWebSocketRequired,
    MissingAuthority,
    CredentialsForbidden,
    ExplicitPortRequired,
    MissingHost,
    IpAddressRequired,
    LoopbackRequired(IpAddr),
}

impl fmt::Display for PlainLoopbackEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plain loopback WebSocket endpoint rejected: {self:?}"
        )
    }
}

impl Error for PlainLoopbackEndpointError {}

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
const PLAIN_LOOPBACK_RUNTIME_JOIN_TIMEOUT: Duration = Duration::from_millis(250);

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
