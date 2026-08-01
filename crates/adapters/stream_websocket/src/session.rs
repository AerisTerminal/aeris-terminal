//! Bounded WebSocket message semantics for one market-bar client session.
//!
//! This module owns explicit memory and publication bounds, message classification,
//! recovery reasons, and the session state machine that feeds the bounded binary decoder
//! and single-writer client model. It performs no connection setup.

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketGeneration, ProvenancedMarketBar,
    ReplayProvenance, ReplayStreamUpdate, ReplayValidationError, ResnapshotReason,
};
use axiusflow_market_protocol_adapter::{
    BinaryMarketBarStreamDecoder, BinaryMarketStreamError, DecimalConvention,
};
use core::fmt;
use std::{collections::VecDeque, error::Error, num::NonZeroUsize};
use tungstenite::{Message, protocol::WebSocketConfig};

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
