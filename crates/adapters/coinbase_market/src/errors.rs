//! Adapter and session failures.

use core::fmt;
use std::error::Error;

/// Reason a Coinbase adapter operation failed.
#[derive(Debug)]
pub enum CoinbaseError {
    InvalidConfiguration,
    InvalidMessage,
    SequenceGap { expected: u64, actual: u64 },
    DuplicateTrade,
    InvalidFixedPoint(String),
    InvalidTimestamp(String),
    Transport(String),
    ServerRejected(String),
    DeadlineExceeded,
}

impl fmt::Display for CoinbaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "coinbase adapter error: {self:?}")
    }
}

impl Error for CoinbaseError {}
