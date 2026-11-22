//! Typed error boundary shared across cryptocurrency exchange adapters.

use std::fmt;

/// Typed error returned by crypto adapter decoders, synchronizers, and HTTP/WS clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// Invalid or overflowing fixed-point decimal text.
    Decimal(String),
    /// Malformed wire JSON or unexpected schema from an exchange.
    MalformedPayload(String),
    /// Unsupported bar period, market kind, or symbol format.
    Unsupported(String),
    /// Order book invariant violation (crossed, invalid price/qty, sequence gap).
    OrderBook(String),
    /// History request cancelled, inconsistent, or empty.
    History(String),
    /// Transport, HTTP status, or WebSocket failure.
    Transport(String),
}

impl CryptoError {
    /// Converts into an owned `String` at legacy runtime trait boundaries.
    #[must_use]
    pub fn into_message(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decimal(msg)
            | Self::MalformedPayload(msg)
            | Self::Unsupported(msg)
            | Self::OrderBook(msg)
            | Self::History(msg)
            | Self::Transport(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CryptoError {}

/// Convenience result alias using [`CryptoError`].
pub type CryptoResult<T> = Result<T, CryptoError>;
