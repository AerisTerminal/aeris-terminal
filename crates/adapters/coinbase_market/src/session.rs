//! Bounded synchronous WebSocket session against the live endpoint.
//!
//! The session subscribes to `heartbeats` and `market_trades`, reads with an
//! absolute deadline, and reports explicit outcomes. Reconnect and resnapshot
//! decisions belong to the caller; a sequence gap is surfaced, never hidden.

use crate::config::CoinbaseConfig;
use crate::decoder::{CanonicalTrade, CoinbaseDecoder, DecoderMetrics};
use crate::errors::CoinbaseError;
use crate::messages::subscribe_frame;
use crate::review::WEBSOCKET_ENDPOINT;
use std::time::{Duration, Instant};
use tungstenite::Message;

/// How one bounded session ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionOutcome {
    /// The requested collection window completed.
    Completed,
    /// A sequence gap forced an explicit stop; reconnect and resnapshot.
    SequenceGap,
    /// The server closed or errored the connection.
    ClosedByPeer,
}

/// Health counters for one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHealth {
    pub outcome: SessionOutcome,
    pub metrics: DecoderMetrics,
}

/// One bounded live session.
pub struct CoinbaseSession {
    config: CoinbaseConfig,
}

impl CoinbaseSession {
    /// Creates a session for one validated configuration.
    #[must_use]
    pub const fn new(config: CoinbaseConfig) -> Self {
        Self { config }
    }

    /// Collects trades for `window`, invoking `on_trade` for each canonical
    /// trade. Returns aggregate health when the window closes.
    ///
    /// # Errors
    ///
    /// Returns an error for transport, subscription, or deadline failures.
    pub fn collect(
        &self,
        window: Duration,
        on_trade: &mut impl FnMut(&CanonicalTrade),
    ) -> Result<SessionHealth, CoinbaseError> {
        let (mut socket, _response) = tungstenite::connect(WEBSOCKET_ENDPOINT)
            .map_err(|error| CoinbaseError::Transport(error.to_string()))?;
        for channel in ["heartbeats", "market_trades"] {
            socket
                .send(Message::Text(
                    subscribe_frame(&self.config.products, channel).into(),
                ))
                .map_err(|error| CoinbaseError::Transport(error.to_string()))?;
        }
        let deadline = Instant::now() + window;
        let mut decoder = CoinbaseDecoder::new();
        let mut outcome = SessionOutcome::Completed;
        loop {
            if Instant::now() >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let tungstenite::stream::MaybeTlsStream::Rustls(stream) = socket.get_mut() {
                stream
                    .get_mut()
                    .set_read_timeout(Some(remaining.min(Duration::from_secs(1))))
                    .map_err(|error| CoinbaseError::Transport(error.to_string()))?;
            }
            match socket.read() {
                Ok(Message::Text(text)) => {
                    if text.len() > self.config.maximum_message_bytes {
                        return Err(CoinbaseError::InvalidMessage);
                    }
                    match decoder.decode(text.as_bytes()) {
                        Ok(trades) => {
                            for trade in &trades {
                                on_trade(trade);
                            }
                        }
                        Err(CoinbaseError::SequenceGap { .. }) => {
                            outcome = SessionOutcome::SequenceGap;
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(Message::Ping(payload)) => {
                    socket
                        .send(Message::Pong(payload))
                        .map_err(|error| CoinbaseError::Transport(error.to_string()))?;
                }
                Ok(Message::Close(_)) => {
                    outcome = SessionOutcome::ClosedByPeer;
                    break;
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(error) => return Err(CoinbaseError::Transport(error.to_string())),
            }
        }
        let _ = socket.close(None);
        Ok(SessionHealth {
            outcome,
            metrics: decoder.metrics(),
        })
    }
}
