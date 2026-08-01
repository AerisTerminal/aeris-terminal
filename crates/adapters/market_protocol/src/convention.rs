//! Wire unit conventions and shared conversion bounds.
//!
//! The decimal convention states the exact price and quantity units a caller expects on
//! the wire, so conversion can reject any value that would silently change scale. The
//! constants here bound snapshot assembly and whole-second timestamp conversion.

use crate::errors::ProtobufAdapterError;
use axiusflow_protocols::MAX_STREAM_SNAPSHOT_ITEMS;

pub(crate) const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// Maximum nonempty frames accepted for one atomic snapshot assembly.
pub const MAX_MARKET_BAR_SNAPSHOT_CHUNKS: usize = MAX_STREAM_SNAPSHOT_ITEMS;

/// Expected wire units for price and quantity decimals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecimalConvention {
    price_unit: String,
    quantity_unit: String,
}

impl DecimalConvention {
    /// Creates an explicit unit convention for converting decimals to bare domain mantissas.
    ///
    /// # Errors
    ///
    /// Returns an error when either expected unit is empty.
    pub fn try_new(
        price_unit: impl Into<String>,
        quantity_unit: impl Into<String>,
    ) -> Result<Self, ProtobufAdapterError> {
        let price_unit = price_unit.into();
        let quantity_unit = quantity_unit.into();
        if price_unit.trim().is_empty() {
            return Err(ProtobufAdapterError::EmptyDecimalConventionUnit(
                "price_unit",
            ));
        }
        if quantity_unit.trim().is_empty() {
            return Err(ProtobufAdapterError::EmptyDecimalConventionUnit(
                "quantity_unit",
            ));
        }
        Ok(Self {
            price_unit,
            quantity_unit,
        })
    }

    /// Returns the required unit for OHLC values.
    #[must_use]
    pub fn price_unit(&self) -> &str {
        &self.price_unit
    }

    /// Returns the required unit for volume values.
    #[must_use]
    pub fn quantity_unit(&self) -> &str {
        &self.quantity_unit
    }
}
