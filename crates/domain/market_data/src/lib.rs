//! Provider-neutral, fixed-point canonical market-data values.

use core::fmt;
use std::error::Error;

/// Versioned rules used to construct one deterministic bar series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BarDefinition {
    pub definition_id: String,
    pub version: u32,
    pub interval_seconds: u32,
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
        if self.interval_seconds == 0 {
            return Err(MarketDataValidationError::ZeroBarInterval);
        }
        Ok(())
    }
}

/// One fixed-point OHLCV bar in source-sequence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketBar {
    pub source_sequence: u64,
    pub exchange_timestamp_seconds: i64,
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
    ZeroSourceSequence,
    InvalidOhlc { source_sequence: u64 },
    NegativeVolume { source_sequence: u64 },
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
        }
    }
}

impl Error for MarketDataValidationError {}
