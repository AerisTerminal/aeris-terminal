//! Provider-neutral instrument identity and versioned reference data.

use core::fmt;
use std::error::Error;

const MAX_DECIMAL_SCALE: u8 = 18;

/// A stable Axiusflow instrument identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InstrumentId(String);

impl InstrumentId {
    /// Creates an instrument identifier from a non-empty platform-issued value.
    ///
    /// # Errors
    ///
    /// Returns [`InstrumentValidationError::EmptyInstrumentId`] for an empty value.
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstrumentValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(InstrumentValidationError::EmptyInstrumentId);
        }
        Ok(Self(value))
    }

    /// Returns the serialized identifier value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Asset classes understood by the provider-neutral instrument model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetClass {
    Equity,
    Option,
    Future,
    ForeignExchange,
    CryptoAsset,
    FixedIncome,
    Fund,
    Index,
}

/// The lifecycle state of an instrument revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstrumentLifecycle {
    Active,
    Halted,
    Delisted,
    Expired,
}

/// Decimal precision rules for authoritative price and quantity values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstrumentPrecision {
    price_scale: u8,
    quantity_scale: u8,
}

impl InstrumentPrecision {
    /// Creates bounded decimal scale rules.
    ///
    /// # Errors
    ///
    /// Returns [`InstrumentValidationError::ScaleOutOfRange`] when either scale
    /// exceeds the platform's maximum authoritative decimal scale.
    pub fn try_new(price_scale: u8, quantity_scale: u8) -> Result<Self, InstrumentValidationError> {
        if price_scale > MAX_DECIMAL_SCALE {
            return Err(InstrumentValidationError::ScaleOutOfRange {
                field: "price_scale",
                value: price_scale,
            });
        }
        if quantity_scale > MAX_DECIMAL_SCALE {
            return Err(InstrumentValidationError::ScaleOutOfRange {
                field: "quantity_scale",
                value: quantity_scale,
            });
        }
        Ok(Self {
            price_scale,
            quantity_scale,
        })
    }

    #[must_use]
    pub fn price_scale(self) -> u8 {
        self.price_scale
    }

    #[must_use]
    pub fn quantity_scale(self) -> u8 {
        self.quantity_scale
    }
}

/// An immutable, versioned view of provider-neutral instrument reference data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentRevision {
    pub instrument_id: InstrumentId,
    pub revision: u64,
    pub asset_class: AssetClass,
    pub symbol: String,
    pub venue_id: String,
    pub trading_currency: String,
    pub precision: InstrumentPrecision,
    pub lifecycle: InstrumentLifecycle,
}

impl InstrumentRevision {
    /// Validates fields whose correctness is independent of a provider adapter.
    ///
    /// # Errors
    ///
    /// Returns an instrument validation error for a zero revision or an empty
    /// symbol, venue identifier, or trading currency.
    pub fn validate(&self) -> Result<(), InstrumentValidationError> {
        if self.revision == 0 {
            return Err(InstrumentValidationError::ZeroRevision);
        }
        if self.symbol.trim().is_empty() {
            return Err(InstrumentValidationError::EmptyField("symbol"));
        }
        if self.venue_id.trim().is_empty() {
            return Err(InstrumentValidationError::EmptyField("venue_id"));
        }
        if self.trading_currency.trim().is_empty() {
            return Err(InstrumentValidationError::EmptyField("trading_currency"));
        }
        Ok(())
    }
}

/// Validation failures for instrument reference data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstrumentValidationError {
    EmptyInstrumentId,
    EmptyField(&'static str),
    ScaleOutOfRange { field: &'static str, value: u8 },
    ZeroRevision,
}

impl fmt::Display for InstrumentValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInstrumentId => formatter.write_str("instrument_id must not be empty"),
            Self::EmptyField(field) => write!(formatter, "{field} must not be empty"),
            Self::ScaleOutOfRange { field, value } => {
                write!(
                    formatter,
                    "{field} scale {value} exceeds {MAX_DECIMAL_SCALE}"
                )
            }
            Self::ZeroRevision => formatter.write_str("instrument revision must be non-zero"),
        }
    }
}

impl Error for InstrumentValidationError {}
