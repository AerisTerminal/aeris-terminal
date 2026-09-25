//! Provider-neutral instrument identity and versioned reference data.

use core::fmt;
use std::error::Error;

const MAX_DECIMAL_SCALE: u8 = 18;
const MAXIMUM_SESSION_SEGMENTS: usize = 32;

/// A stable `Aeris` instrument identity.
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

/// One exact fixed-point contract value such as tick size or point value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstrumentDecimal {
    units: i64,
    scale: u8,
}

impl InstrumentDecimal {
    /// Creates a positive contract value with an explicit decimal scale.
    ///
    /// # Errors
    /// Returns an error for a non-positive coefficient or scale above 18.
    pub const fn try_new(units: i64, scale: u8) -> Result<Self, InstrumentValidationError> {
        if units <= 0 {
            return Err(InstrumentValidationError::NonPositiveContractValue);
        }
        if scale > MAX_DECIMAL_SCALE {
            return Err(InstrumentValidationError::ScaleOutOfRange {
                field: "contract_value",
                value: scale,
            });
        }
        Ok(Self { units, scale })
    }

    #[must_use]
    pub const fn units(self) -> i64 {
        self.units
    }

    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }
}

/// Calendar date carried without a local-time or timezone assumption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContractDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl ContractDate {
    /// Validates the Gregorian date.
    ///
    /// # Errors
    /// Returns an error when the date is not a real calendar day.
    pub const fn validate(self) -> Result<(), InstrumentValidationError> {
        if self.year == 0 || self.month == 0 || self.month > 12 || self.day == 0 {
            return Err(InstrumentValidationError::InvalidContractDate);
        }
        let maximum_day = match self.month {
            2 if is_leap_year(self.year) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if self.day > maximum_day {
            return Err(InstrumentValidationError::InvalidContractDate);
        }
        Ok(())
    }
}

/// One weekly exchange-session segment in the named IANA timezone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHours {
    /// ISO weekday where Monday is 1 and Sunday is 7.
    pub weekday: u8,
    pub open_seconds: u32,
    pub close_seconds: u32,
    pub timezone: String,
}

impl SessionHours {
    /// Validates weekday, wall-clock bounds, and timezone identity.
    ///
    /// # Errors
    /// Returns an error when the segment cannot be interpreted safely.
    pub fn validate(&self) -> Result<(), InstrumentValidationError> {
        if !(1..=7).contains(&self.weekday)
            || self.open_seconds >= 86_400
            || self.close_seconds > 86_400
            || self.open_seconds == self.close_seconds
        {
            return Err(InstrumentValidationError::InvalidSessionHours);
        }
        if self.timezone.trim().is_empty() || self.timezone.len() > 128 {
            return Err(InstrumentValidationError::InvalidSessionHours);
        }
        Ok(())
    }
}

/// Source evidence for one contract-metadata revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentMetadataProvenance {
    pub provider_id: String,
    pub provider_symbol: String,
    pub session_generation: u64,
}

/// Provider-sourced futures contract terms used by trading and risk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractMetadata {
    pub tick_size: Option<InstrumentDecimal>,
    pub point_value: Option<InstrumentDecimal>,
    pub currency: String,
    pub expiry: Option<ContractDate>,
    pub first_notice: Option<ContractDate>,
    pub last_trade: Option<ContractDate>,
    pub session_hours: Vec<SessionHours>,
    pub provenance: InstrumentMetadataProvenance,
}

impl ContractMetadata {
    /// Validates all available contract terms without inventing missing provider values.
    ///
    /// # Errors
    /// Returns an error for malformed currency, date, hours, or source evidence.
    pub fn validate(&self) -> Result<(), InstrumentValidationError> {
        if self.currency.trim().is_empty() || self.currency.len() > 16 {
            return Err(InstrumentValidationError::EmptyField("contract.currency"));
        }
        for date in [self.expiry, self.first_notice, self.last_trade]
            .into_iter()
            .flatten()
        {
            date.validate()?;
        }
        if self.session_hours.len() > MAXIMUM_SESSION_SEGMENTS {
            return Err(InstrumentValidationError::TooManySessionSegments);
        }
        for segment in &self.session_hours {
            segment.validate()?;
        }
        if self.provenance.provider_id.trim().is_empty()
            || self.provenance.provider_symbol.trim().is_empty()
            || self.provenance.session_generation == 0
        {
            return Err(InstrumentValidationError::InvalidMetadataProvenance);
        }
        Ok(())
    }
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
    pub contract: Option<ContractMetadata>,
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
        if let Some(contract) = &self.contract {
            contract.validate()?;
            if contract.currency != self.trading_currency {
                return Err(InstrumentValidationError::CurrencyMismatch);
            }
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
    CurrencyMismatch,
    InvalidContractDate,
    InvalidMetadataProvenance,
    InvalidSessionHours,
    NonPositiveContractValue,
    TooManySessionSegments,
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
            Self::CurrencyMismatch => {
                formatter.write_str("contract currency differs from instrument currency")
            }
            Self::InvalidContractDate => formatter.write_str("contract date is invalid"),
            Self::InvalidMetadataProvenance => {
                formatter.write_str("contract metadata provenance is invalid")
            }
            Self::InvalidSessionHours => formatter.write_str("session hours are invalid"),
            Self::NonPositiveContractValue => {
                formatter.write_str("contract value must be positive")
            }
            Self::TooManySessionSegments => {
                formatter.write_str("contract has too many session segments")
            }
        }
    }
}

impl Error for InstrumentValidationError {}

const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}
