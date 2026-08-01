//! Error model shared by every deterministic conformance harness in this crate.
//!
//! Harness failures are classified by the boundary that rejected the input so a
//! caller can distinguish a driver failure from a semantic divergence without
//! parsing message text.

use core::fmt;
use std::error::Error;

/// Reason one fixture provider packet could not be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureDecodeError {
    TruncatedEthernetOrIp,
    UnsupportedEtherType,
    UnsupportedIpv4Header,
    UnsupportedIpProtocol(u8),
    InvalidProviderLength(usize),
    InvalidProviderMagic,
    ZeroSequence,
    CanonicalEvent,
}

/// Boundary that rejected a conformance run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConformanceHarnessError {
    Driver(String),
    Realtime(String),
    MarketStream(String),
    WebSocket(String),
    MissingFrame(usize),
    ProfileDivergence,
}

impl fmt::Display for ConformanceHarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ingest conformance failed: {self:?}")
    }
}

impl Error for ConformanceHarnessError {}

pub(crate) fn websocket_error(error: impl fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::WebSocket(error.to_string())
}

pub(crate) fn market_stream_error(error: impl fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::MarketStream(error.to_string())
}

pub(crate) fn realtime_error(error: impl fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::Realtime(error.to_string())
}
