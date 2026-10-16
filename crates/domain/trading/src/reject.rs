//! Typed practice market gates, independent of any provider implementation.
use aeris_instruments::SessionPhase;
use core::fmt;

/// Provider-neutral, structured advisory returned by risk evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiskWarning {
    ApproachingDailyLossLimit,
    NoAttachedStop,
    ConsistencyLimitExceeded {
        current_percent: u8,
        limit_percent: u8,
    },
    FastStopReentry,
}

impl RiskWarning {
    /// Stable reason code, independent of display copy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApproachingDailyLossLimit => "approaching_daily_loss_limit",
            Self::NoAttachedStop => "no_attached_stop",
            Self::ConsistencyLimitExceeded { .. } => "consistency_limit_exceeded",
            Self::FastStopReentry => "fast_stop_reentry",
        }
    }
}

impl fmt::Display for RiskWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApproachingDailyLossLimit => {
                f.write_str("account is within 20% of its daily loss limit")
            }
            Self::NoAttachedStop => {
                f.write_str("order has no attached stop; loss at stop is unbounded")
            }
            Self::ConsistencyLimitExceeded {
                current_percent,
                limit_percent,
            } => write!(
                f,
                "largest winning trade is {current_percent}% of gross winning P/L; profile limit is {limit_percent}%"
            ),
            Self::FastStopReentry => {
                f.write_str("fast re-entry within 60 seconds of a filled stop")
            }
        }
    }
}

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
    OffGrid,
    WrongSideTrigger,
    TifNotSupported,
    CapacityExceeded,
    AccountLocked,
    InvalidQuantity,
    ClientIdTooLong,
    ScaleMismatch,
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
            Self::OffGrid => f.write_str("OffGrid: price is off the instrument price grid"),
            Self::WrongSideTrigger => {
                f.write_str("WrongSideTrigger: stop must trigger beyond the current bid or ask")
            }
            Self::TifNotSupported => {
                f.write_str("TifNotSupported: IOC and FOK are unavailable for stop orders")
            }
            Self::CapacityExceeded => {
                f.write_str("CapacityExceeded: trading order capacity reached")
            }
            Self::AccountLocked => f.write_str("AccountLocked: account is risk-locked"),
            Self::InvalidQuantity => f.write_str("order quantity must use whole provider units"),
            Self::ClientIdTooLong => {
                f.write_str("cTrader ClientOrderId must be at most 50 characters")
            }
            Self::ScaleMismatch => f.write_str("order scales do not match the instrument"),
        }
    }
}

impl std::error::Error for RejectReason {}
