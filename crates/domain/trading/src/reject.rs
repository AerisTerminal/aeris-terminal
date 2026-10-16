//! Typed practice market gates, independent of any provider implementation.
use aeris_instruments::SessionPhase;
use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    MarketClosed {
        phase: SessionPhase,
        next_open: Option<i64>,
    },
    SessionUnknown,
    QuoteUnavailable,
    QuoteStale {
        age_ms: i64,
        limit_ms: i64,
    },
    ClientIdReused,
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MarketClosed { phase, next_open } => {
                write!(f, "market closed ({phase:?}, next open {next_open:?})")
            }
            Self::SessionUnknown => f.write_str("market session unknown"),
            Self::QuoteUnavailable => f.write_str("live market quote unavailable"),
            Self::QuoteStale { age_ms, limit_ms } => {
                write!(f, "market quote stale ({age_ms} ms exceeds {limit_ms} ms)")
            }
            Self::ClientIdReused => {
                f.write_str("ClientIdReused: client order id has different instructions")
            }
        }
    }
}

impl std::error::Error for RejectReason {}
