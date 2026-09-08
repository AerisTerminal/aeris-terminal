//! Provider-neutral, fixed-point canonical market-data values.

use core::fmt;
use std::error::Error;

mod chart_interval;
mod contracts;
mod order_book;

pub use chart_interval::{
    ChartAggregation, ChartInterval, RithmicChartAggregation, RithmicDailyAggregation,
    RithmicTimeUnit,
};
pub use contracts::{
    AggressorSide, BarPeriod, BarSeriesKey, BarUpdate, BookSide, DepthDelta, DepthLevel,
    DepthSnapshot, EventMetadata, MAXIMUM_MARKET_DATA_FIELD_BYTES, MarketEvent, MarketTrade,
    QualifiedTimestamp, TopOfBookQuote,
};
pub use order_book::{
    AggressorTradeVolumes, OrderBook, OrderBookApplyOutcome, OrderBookColumnLevel, OrderBookFrame,
    OrderBookPublication, OrderBookRecoveryReason, OrderBookRow, OrderBookState,
};

/// Versioned rules used to construct one deterministic bar series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BarDefinition {
    pub definition_id: String,
    pub version: u32,
    /// Fixed time cadence in seconds, or zero for a non-fixed series.
    pub interval_seconds: u32,
    /// Trades in each bar for a trade-count series, otherwise absent.
    pub trades_per_bar: Option<u32>,
    /// Calendar months in each bar for a calendar series, otherwise absent.
    pub calendar_months: Option<u32>,
}

impl BarDefinition {
    /// Validates identity, version, and interval fields.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty or a numeric field is zero.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        if self.definition_id.trim().is_empty() {
            return Err(MarketDataValidationError::EmptyBarDefinitionId);
        }
        if self.version == 0 {
            return Err(MarketDataValidationError::ZeroBarDefinitionVersion);
        }
        if self.interval_seconds == 0
            && self.trades_per_bar.is_none()
            && self.calendar_months.is_none()
        {
            return Err(MarketDataValidationError::ZeroBarInterval);
        }
        let cadence_count = usize::from(self.interval_seconds > 0)
            + usize::from(self.trades_per_bar.is_some())
            + usize::from(self.calendar_months.is_some());
        if cadence_count != 1 || self.trades_per_bar == Some(0) || self.calendar_months == Some(0) {
            return Err(MarketDataValidationError::InvalidBarCadence);
        }
        Ok(())
    }
}

/// One fixed-point OHLCV bar in source-sequence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketBar {
    pub source_sequence: u64,
    /// Whole exchange second used for fixed-time bucket arithmetic.
    pub exchange_timestamp_seconds: i64,
    /// Exact exchange ordering timestamp. Its whole second must match
    /// `exchange_timestamp_seconds`.
    pub exchange_timestamp_unix_nanos: i64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
}

impl MarketBar {
    /// Validates sequence and fixed-point OHLCV invariants.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero source sequence, invalid OHLC bounds, or
    /// negative volume.
    pub fn validate(self) -> Result<(), MarketDataValidationError> {
        if self.source_sequence == 0 {
            return Err(MarketDataValidationError::ZeroSourceSequence);
        }
        if self.exchange_timestamp_unix_nanos.div_euclid(1_000_000_000)
            != self.exchange_timestamp_seconds
        {
            return Err(MarketDataValidationError::InvalidTimestamp(
                "bar_exchange_timestamp",
            ));
        }
        if self.high < self.open.max(self.close)
            || self.low > self.open.min(self.close)
            || self.low > self.high
        {
            return Err(MarketDataValidationError::InvalidOhlc {
                source_sequence: self.source_sequence,
            });
        }
        if self.volume < 0 {
            return Err(MarketDataValidationError::NegativeVolume {
                source_sequence: self.source_sequence,
            });
        }
        Ok(())
    }
}

/// Validation failures for canonical market-data values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketDataValidationError {
    EmptyBarDefinitionId,
    ZeroBarDefinitionVersion,
    ZeroBarInterval,
    InvalidBarCadence,
    ZeroSourceSequence,
    InvalidOhlc { source_sequence: u64 },
    NegativeVolume { source_sequence: u64 },
    EmptyField(&'static str),
    FieldTooLong { field: &'static str, maximum: usize },
    InvalidTimestamp(&'static str),
    InvalidPrice,
    InvalidQuantity,
    InvalidQuote,
    InvalidDepth,
    DuplicateDepthPrice,
    InvalidPeriod,
    InvalidRevision,
    BarSeriesMetadataMismatch,
    BarSourceSequenceMismatch,
    SourceSequenceRegression,
    DepthGap { expected: u64, actual: u64 },
    DepthLimitExceeded { maximum: usize },
}

impl fmt::Display for MarketDataValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBarDefinitionId => {
                formatter.write_str("bar definition id must not be empty")
            }
            Self::ZeroBarDefinitionVersion => {
                formatter.write_str("bar definition version must be non-zero")
            }
            Self::ZeroBarInterval => formatter.write_str("bar interval must be non-zero"),
            Self::InvalidBarCadence => {
                formatter.write_str(
                    "bar cadence must be fixed time, non-zero trade count, or non-zero calendar-month count",
                )
            }
            Self::ZeroSourceSequence => formatter.write_str("source sequence must be non-zero"),
            Self::InvalidOhlc { source_sequence } => {
                write!(
                    formatter,
                    "invalid OHLC values at source sequence {source_sequence}"
                )
            }
            Self::NegativeVolume { source_sequence } => {
                write!(
                    formatter,
                    "negative volume at source sequence {source_sequence}"
                )
            }
            Self::EmptyField(field) => write!(formatter, "{field} must not be empty"),
            Self::FieldTooLong { field, maximum } => {
                write!(formatter, "{field} exceeds the maximum {maximum} bytes")
            }
            Self::InvalidTimestamp(field) => write!(formatter, "{field} timestamp is invalid"),
            Self::InvalidPrice => formatter.write_str("market-data price is invalid"),
            Self::InvalidQuantity => formatter.write_str("market-data quantity is invalid"),
            Self::InvalidQuote => formatter.write_str("top-of-book quote is invalid"),
            Self::InvalidDepth => formatter.write_str("depth update is invalid"),
            Self::DuplicateDepthPrice => {
                formatter.write_str("depth prices must be unique per side")
            }
            Self::InvalidPeriod => formatter.write_str("bar period is invalid"),
            Self::InvalidRevision => formatter.write_str("publication revision must be non-zero"),
            Self::BarSeriesMetadataMismatch => {
                formatter.write_str("bar series identity does not match event metadata")
            }
            Self::BarSourceSequenceMismatch => {
                formatter.write_str("bar source sequence does not match event metadata")
            }
            Self::SourceSequenceRegression => {
                formatter.write_str("source sequence did not advance")
            }
            Self::DepthGap { expected, actual } => {
                write!(
                    formatter,
                    "depth sequence gap: expected {expected}, received {actual}"
                )
            }
            Self::DepthLimitExceeded { maximum } => {
                write!(
                    formatter,
                    "depth level count exceeds configured maximum {maximum}"
                )
            }
        }
    }
}

impl Error for MarketDataValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_definition_requires_exactly_one_nonzero_cadence() {
        let definition = |interval_seconds, trades_per_bar, calendar_months| BarDefinition {
            definition_id: "fixture".to_string(),
            version: 1,
            interval_seconds,
            trades_per_bar,
            calendar_months,
        };
        assert!(definition(60, None, None).validate().is_ok());
        assert!(definition(0, Some(100), None).validate().is_ok());
        assert!(definition(0, None, Some(1)).validate().is_ok());
        assert_eq!(
            definition(0, None, None).validate(),
            Err(MarketDataValidationError::ZeroBarInterval)
        );
        assert_eq!(
            definition(60, Some(100), None).validate(),
            Err(MarketDataValidationError::InvalidBarCadence)
        );
        assert_eq!(
            definition(0, Some(0), None).validate(),
            Err(MarketDataValidationError::InvalidBarCadence)
        );
        assert_eq!(
            definition(0, None, Some(0)).validate(),
            Err(MarketDataValidationError::InvalidBarCadence)
        );
        assert_eq!(
            definition(0, Some(100), Some(1)).validate(),
            Err(MarketDataValidationError::InvalidBarCadence)
        );
    }

    #[test]
    fn market_bar_rejects_disagreeing_coarse_and_exact_exchange_time() {
        let mut bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 10,
            exchange_timestamp_unix_nanos: 10_123_456_000,
            open: 100,
            high: 100,
            low: 100,
            close: 100,
            volume: 1,
        };
        assert!(bar.validate().is_ok());
        bar.exchange_timestamp_seconds = 11;
        assert_eq!(
            bar.validate(),
            Err(MarketDataValidationError::InvalidTimestamp(
                "bar_exchange_timestamp"
            ))
        );
    }
}
