//! Provider-neutral trading contracts.
//!
//! These values contain no provider wire types, persistence handles, or UI state. The
//! in-process trading runtime is their sole mutable owner.

use aeris_instruments::InstrumentId;
use core::fmt;
use std::error::Error;

/// Maximum decimal scale accepted by canonical trading values.
pub const MAXIMUM_DECIMAL_SCALE: u8 = 18;
/// Maximum bytes accepted in one trading identity or presentation field.
pub const MAXIMUM_TRADING_FIELD_BYTES: usize = 256;

macro_rules! string_id {
    ($name:ident, $empty:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Creates a bounded non-empty identity.
            ///
            /// # Errors
            /// Returns a validation error when the identity is blank or oversized.
            pub fn try_new(value: impl Into<String>) -> Result<Self, TradingValidationError> {
                let value = value.into();
                validate_field($empty, &value)?;
                Ok(Self(value))
            }

            /// Returns the stable serialized identity.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(TradingAccountId, "account_id");
string_id!(OrderId, "order_id");
string_id!(ClientOrderId, "client_order_id");
string_id!(OrderEventId, "order_event_id");
string_id!(FillId, "fill_id");

/// One signed fixed-point amount. Its scale is part of the value and never inferred.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedPoint {
    units: i64,
    scale: u8,
}

impl FixedPoint {
    /// Creates a fixed-point amount with a bounded explicit decimal scale.
    ///
    /// # Errors
    /// Returns a validation error when `scale` exceeds 18.
    pub const fn try_new(units: i64, scale: u8) -> Result<Self, TradingValidationError> {
        if scale > MAXIMUM_DECIMAL_SCALE {
            return Err(TradingValidationError::ScaleOutOfRange(scale));
        }
        Ok(Self { units, scale })
    }

    /// Returns the signed integer coefficient.
    #[must_use]
    pub const fn units(self) -> i64 {
        self.units
    }

    /// Returns the decimal scale.
    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }

    /// Checked addition without implicit rescaling.
    ///
    /// # Errors
    /// Returns an error for different scales or integer overflow.
    pub fn checked_add(self, other: Self) -> Result<Self, TradingValidationError> {
        if self.scale != other.scale {
            return Err(TradingValidationError::ScaleMismatch);
        }
        let units = self
            .units
            .checked_add(other.units)
            .ok_or(TradingValidationError::ArithmeticOverflow)?;
        Ok(Self {
            units,
            scale: self.scale,
        })
    }

    /// Converts to another scale only when the exact value can be preserved.
    ///
    /// # Errors
    /// Returns an error for overflow or a conversion that would require rounding.
    pub fn exact_rescale(self, target_scale: u8) -> Result<Self, TradingValidationError> {
        if target_scale > MAXIMUM_DECIMAL_SCALE {
            return Err(TradingValidationError::ScaleOutOfRange(target_scale));
        }
        match target_scale.cmp(&self.scale) {
            std::cmp::Ordering::Equal => Ok(self),
            std::cmp::Ordering::Greater => {
                let factor = power_of_ten(target_scale - self.scale)?;
                let units = self
                    .units
                    .checked_mul(factor)
                    .ok_or(TradingValidationError::ArithmeticOverflow)?;
                Ok(Self {
                    units,
                    scale: target_scale,
                })
            }
            std::cmp::Ordering::Less => {
                let factor = power_of_ten(self.scale - target_scale)?;
                if self.units % factor != 0 {
                    return Err(TradingValidationError::InexactRescale);
                }
                Ok(Self {
                    units: self.units / factor,
                    scale: target_scale,
                })
            }
        }
    }
}

/// Projects open-position P/L at one price using exact fixed-point contract terms.
///
/// This is the canonical calculation shared by the trading owner and read-only
/// presentation projections. Quantity scale participates in the result so
/// fractional quantities cannot be mistaken for whole contracts.
///
/// # Errors
/// Returns a validation error when price scales differ, contract terms are
/// invalid, or the rounded result cannot be represented at `currency_scale`.
/// Values finer than the account currency precision are rounded to nearest,
/// with ties away from zero. This is an explicit monetary projection policy,
/// not an implicit `FixedPoint` rescale.
pub fn project_unrealized_pnl(
    entry: FixedPoint,
    mark: FixedPoint,
    net_quantity: FixedPoint,
    point_value: FixedPoint,
    currency_scale: u8,
) -> Result<FixedPoint, TradingValidationError> {
    if entry.scale() != mark.scale() {
        return Err(TradingValidationError::ScaleMismatch);
    }
    if entry.units() <= 0 || mark.units() <= 0 {
        return Err(TradingValidationError::NonPositivePrice);
    }
    if point_value.units() <= 0 {
        return Err(TradingValidationError::NonPositivePointValue);
    }
    let price_difference = i128::from(mark.units()) - i128::from(entry.units());
    let raw = price_difference
        .checked_mul(i128::from(net_quantity.units()))
        .and_then(|value| value.checked_mul(i128::from(point_value.units())))
        .ok_or(TradingValidationError::ArithmeticOverflow)?;
    let raw_scale = entry
        .scale()
        .checked_add(net_quantity.scale())
        .and_then(|scale| scale.checked_add(point_value.scale()))
        .ok_or(TradingValidationError::ArithmeticOverflow)?;
    fixed_point_from_i128_rounded(raw, raw_scale, currency_scale)
}

fn fixed_point_from_i128_rounded(
    units: i128,
    source_scale: u8,
    target_scale: u8,
) -> Result<FixedPoint, TradingValidationError> {
    if target_scale > MAXIMUM_DECIMAL_SCALE {
        return Err(TradingValidationError::ScaleOutOfRange(target_scale));
    }
    let adjusted = match target_scale.cmp(&source_scale) {
        std::cmp::Ordering::Equal => units,
        std::cmp::Ordering::Greater => {
            let factor = power_of_ten_i128(target_scale - source_scale)?;
            units
                .checked_mul(factor)
                .ok_or(TradingValidationError::ArithmeticOverflow)?
        }
        std::cmp::Ordering::Less => {
            let divisor = power_of_ten_i128(source_scale - target_scale)?;
            round_divide_i128(units, divisor)?
        }
    };
    let adjusted =
        i64::try_from(adjusted).map_err(|_| TradingValidationError::ArithmeticOverflow)?;
    FixedPoint::try_new(adjusted, target_scale)
}

fn round_divide_i128(
    numerator: i128,
    positive_denominator: i128,
) -> Result<i128, TradingValidationError> {
    debug_assert!(positive_denominator > 0);
    let quotient = numerator / positive_denominator;
    let remainder = numerator % positive_denominator;
    if remainder == 0 {
        return Ok(quotient);
    }
    let doubled_remainder = remainder
        .checked_abs()
        .and_then(|value| value.checked_mul(2))
        .ok_or(TradingValidationError::ArithmeticOverflow)?;
    if doubled_remainder < positive_denominator {
        return Ok(quotient);
    }
    quotient
        .checked_add(numerator.signum())
        .ok_or(TradingValidationError::ArithmeticOverflow)
}

/// Provenance kept on every canonical trading mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingProvenance {
    pub venue_id: String,
    pub provider_id: String,
    pub session_generation: u64,
    pub source_sequence: u64,
    pub observed_unix_nanos: i64,
}

impl TradingProvenance {
    /// Validates venue/provider identity, generation, ordering, and time evidence.
    ///
    /// # Errors
    /// Returns an error when required evidence is absent.
    pub fn validate(&self) -> Result<(), TradingValidationError> {
        validate_field("venue_id", &self.venue_id)?;
        validate_field("provider_id", &self.provider_id)?;
        if self.session_generation == 0 || self.source_sequence == 0 {
            return Err(TradingValidationError::ZeroSequence);
        }
        if self.observed_unix_nanos <= 0 {
            return Err(TradingValidationError::InvalidTimestamp);
        }
        Ok(())
    }
}

/// Whether an account can route real orders or is locally simulated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountEnvironment {
    Simulated,
    Live,
}

impl AccountEnvironment {
    /// Stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simulated => "simulated",
            Self::Live => "live",
        }
    }
}

/// Provider-neutral broker or simulated trading account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingAccount {
    pub id: TradingAccountId,
    pub display_name: String,
    pub environment: AccountEnvironment,
    pub currency: String,
    pub currency_scale: u8,
    /// User-selected opening equity for a locally simulated account. Provider
    /// accounts and legacy local accounts may not expose an opening balance.
    pub starting_equity: Option<FixedPoint>,
}

impl TradingAccount {
    /// Validates account presentation and currency precision.
    ///
    /// # Errors
    /// Returns a validation error for blank/oversized fields or an invalid scale.
    pub fn validate(&self) -> Result<(), TradingValidationError> {
        validate_field("display_name", &self.display_name)?;
        validate_field("currency", &self.currency)?;
        if self.currency_scale > MAXIMUM_DECIMAL_SCALE {
            return Err(TradingValidationError::ScaleOutOfRange(self.currency_scale));
        }
        if self
            .starting_equity
            .is_some_and(|equity| equity.units() <= 0 || equity.scale() != self.currency_scale)
        {
            return Err(TradingValidationError::InvalidStartingEquity);
        }
        if self.environment == AccountEnvironment::Simulated
            && !self.display_name.to_ascii_uppercase().contains("SIM")
        {
            return Err(TradingValidationError::SimulationLabelMissing);
        }
        Ok(())
    }
}

/// Buy or sell direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    /// Stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }

    /// Signed quantity multiplier used by position accounting.
    #[must_use]
    pub const fn sign(self) -> i64 {
        match self {
            Self::Buy => 1,
            Self::Sell => -1,
        }
    }
}

/// Supported canonical order instructions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
}

impl OrderType {
    /// Stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Market => "market",
            Self::Limit => "limit",
            Self::Stop => "stop",
            Self::StopLimit => "stop_limit",
        }
    }
}

/// Canonical time-in-force instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeInForce {
    Day,
    GoodTillCancelled,
    ImmediateOrCancel,
    FillOrKill,
}

impl TimeInForce {
    /// Stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::GoodTillCancelled => "gtc",
            Self::ImmediateOrCancel => "ioc",
            Self::FillOrKill => "fok",
        }
    }
}

/// Current lifecycle state of one canonical order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderStatus {
    Pending,
    Working,
    PendingModify,
    PartiallyFilled,
    PendingCancel,
    Filled,
    Cancelled,
    Rejected,
}

impl OrderStatus {
    /// Stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Working => "working",
            Self::PendingModify => "pending_modify",
            Self::PartiallyFilled => "partially_filled",
            Self::PendingCancel => "pending_cancel",
            Self::Filled => "filled",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
        }
    }

    /// Whether the order still owns live quantity and must survive retention.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(
            self,
            Self::Pending
                | Self::Working
                | Self::PendingModify
                | Self::PartiallyFilled
                | Self::PendingCancel
        )
    }

    /// Whether the venue may execute additional quantity in this state.
    #[must_use]
    pub const fn is_executable(self) -> bool {
        matches!(self, Self::Working | Self::PartiallyFilled)
    }
}

/// One provider-neutral order owned by the trading runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub id: OrderId,
    pub client_order_id: ClientOrderId,
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub quantity: FixedPoint,
    pub filled_quantity: FixedPoint,
    pub limit_price: Option<FixedPoint>,
    pub stop_price: Option<FixedPoint>,
    pub status: OrderStatus,
    pub submitted_unix_nanos: i64,
    pub provenance: TradingProvenance,
}

impl Order {
    /// Validates canonical order shape and fixed-point consistency.
    ///
    /// # Errors
    /// Returns an error for non-positive quantity/time or missing/extraneous prices.
    pub fn validate(&self) -> Result<(), TradingValidationError> {
        if self.quantity.units() <= 0 {
            return Err(TradingValidationError::NonPositiveQuantity);
        }
        if self.filled_quantity.units() < 0
            || self.filled_quantity.scale() != self.quantity.scale()
            || self.filled_quantity.units() > self.quantity.units()
            || (self.status.is_open() && self.filled_quantity == self.quantity)
        {
            return Err(TradingValidationError::InvalidFilledQuantity);
        }
        match self.status {
            OrderStatus::PartiallyFilled
                if self.filled_quantity.units() == 0
                    || self.filled_quantity.units() == self.quantity.units() =>
            {
                return Err(TradingValidationError::InvalidFilledQuantity);
            }
            OrderStatus::Filled if self.filled_quantity != self.quantity => {
                return Err(TradingValidationError::InvalidFilledQuantity);
            }
            OrderStatus::Pending | OrderStatus::Working | OrderStatus::Rejected
                if self.filled_quantity.units() != 0 =>
            {
                return Err(TradingValidationError::InvalidFilledQuantity);
            }
            _ => {}
        }
        if self.submitted_unix_nanos <= 0 {
            return Err(TradingValidationError::InvalidTimestamp);
        }
        self.provenance.validate()?;
        if self.limit_price.is_some_and(|price| price.units() <= 0)
            || self.stop_price.is_some_and(|price| price.units() <= 0)
        {
            return Err(TradingValidationError::NonPositivePrice);
        }
        match self.order_type {
            OrderType::Market if self.limit_price.is_none() && self.stop_price.is_none() => {}
            OrderType::Limit if self.limit_price.is_some() && self.stop_price.is_none() => {}
            OrderType::Stop if self.limit_price.is_none() && self.stop_price.is_some() => {}
            OrderType::StopLimit if self.limit_price.is_some() && self.stop_price.is_some() => {}
            _ => return Err(TradingValidationError::InvalidOrderPrices),
        }
        if self
            .limit_price
            .zip(self.stop_price)
            .is_some_and(|(limit, stop)| limit.scale() != stop.scale())
        {
            return Err(TradingValidationError::ScaleMismatch);
        }
        Ok(())
    }
}

/// Durable canonical order-event kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderEventKind {
    Accepted,
    Modified,
    Filled,
    Cancelled,
    Rejected,
}

impl OrderEventKind {
    /// Stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Modified => "modified",
            Self::Filled => "filled",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
        }
    }
}

/// One immutable lifecycle event for an order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderEvent {
    pub id: OrderEventId,
    pub order_id: OrderId,
    pub sequence: u64,
    pub kind: OrderEventKind,
    pub event_unix_nanos: i64,
    pub detail: Option<String>,
    pub provenance: TradingProvenance,
}

impl OrderEvent {
    /// Validates event ordering, time, detail bounds, and provenance.
    ///
    /// # Errors
    /// Returns a validation error when required evidence is invalid.
    pub fn validate(&self) -> Result<(), TradingValidationError> {
        if self.sequence == 0 {
            return Err(TradingValidationError::ZeroSequence);
        }
        if self.event_unix_nanos <= 0 {
            return Err(TradingValidationError::InvalidTimestamp);
        }
        if let Some(detail) = &self.detail {
            validate_field("event_detail", detail)?;
        }
        self.provenance.validate()
    }
}

/// One immutable execution fill.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fill {
    pub id: FillId,
    pub order_id: OrderId,
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub execution_unix_nanos: i64,
    pub provenance: TradingProvenance,
}

impl Fill {
    /// Validates positive fixed-point values, time, and provenance.
    ///
    /// # Errors
    /// Returns a validation error when any canonical fill field is invalid.
    pub fn validate(&self) -> Result<(), TradingValidationError> {
        if self.price.units() <= 0 {
            return Err(TradingValidationError::NonPositivePrice);
        }
        if self.quantity.units() <= 0 {
            return Err(TradingValidationError::NonPositiveQuantity);
        }
        if self.execution_unix_nanos <= 0 {
            return Err(TradingValidationError::InvalidTimestamp);
        }
        self.provenance.validate()
    }
}

/// Canonical net position and `PnL` for one account/instrument pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub net_quantity: FixedPoint,
    pub average_entry_price: Option<FixedPoint>,
    pub realized_pnl: FixedPoint,
    pub unrealized_pnl: FixedPoint,
    pub last_fill_unix_nanos: i64,
}

/// Account-level `PnL` projection in an explicit currency scale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountPnl {
    pub account_id: TradingAccountId,
    pub currency: String,
    pub realized: FixedPoint,
    pub unrealized: FixedPoint,
    pub equity: Option<FixedPoint>,
}

/// Validation failures at the provider-neutral trading boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TradingValidationError {
    ArithmeticOverflow,
    EmptyField(&'static str),
    FieldTooLong(&'static str),
    InexactRescale,
    InvalidOrderPrices,
    InvalidStartingEquity,
    InvalidFilledQuantity,
    InvalidTimestamp,
    NonPositivePrice,
    NonPositivePointValue,
    NonPositiveQuantity,
    ScaleMismatch,
    ScaleOutOfRange(u8),
    SimulationLabelMissing,
    ZeroSequence,
}

impl fmt::Display for TradingValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArithmeticOverflow => formatter.write_str("fixed-point arithmetic overflowed"),
            Self::EmptyField(field) => write!(formatter, "{field} must not be empty"),
            Self::FieldTooLong(field) => write!(formatter, "{field} exceeds the byte limit"),
            Self::InexactRescale => {
                formatter.write_str("fixed-point rescale would require rounding")
            }
            Self::InvalidOrderPrices => formatter.write_str("prices do not match the order type"),
            Self::InvalidStartingEquity => formatter
                .write_str("starting equity must be positive and use the account currency scale"),
            Self::InvalidFilledQuantity => {
                formatter.write_str("filled quantity does not match the order lifecycle")
            }
            Self::InvalidTimestamp => formatter.write_str("timestamp must be positive"),
            Self::NonPositivePrice => formatter.write_str("price must be positive"),
            Self::NonPositivePointValue => formatter.write_str("point value must be positive"),
            Self::NonPositiveQuantity => formatter.write_str("quantity must be positive"),
            Self::ScaleMismatch => formatter.write_str("fixed-point scales do not match"),
            Self::ScaleOutOfRange(scale) => write!(formatter, "decimal scale {scale} exceeds 18"),
            Self::SimulationLabelMissing => {
                formatter.write_str("simulated account display name must contain SIM")
            }
            Self::ZeroSequence => formatter.write_str("generation and sequence must be non-zero"),
        }
    }
}

impl Error for TradingValidationError {}

fn validate_field(field: &'static str, value: &str) -> Result<(), TradingValidationError> {
    if value.trim().is_empty() {
        return Err(TradingValidationError::EmptyField(field));
    }
    if value.len() > MAXIMUM_TRADING_FIELD_BYTES {
        return Err(TradingValidationError::FieldTooLong(field));
    }
    Ok(())
}

fn power_of_ten(exponent: u8) -> Result<i64, TradingValidationError> {
    10_i64
        .checked_pow(u32::from(exponent))
        .ok_or(TradingValidationError::ArithmeticOverflow)
}

fn power_of_ten_i128(exponent: u8) -> Result<i128, TradingValidationError> {
    10_i128
        .checked_pow(u32::from(exponent))
        .ok_or(TradingValidationError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_never_rounds_implicitly() {
        let value = FixedPoint::try_new(1_250, 3).expect("value");
        assert_eq!(
            value.exact_rescale(2).expect("exact"),
            FixedPoint::try_new(125, 2).expect("value")
        );
        assert_eq!(
            FixedPoint::try_new(1_251, 3)
                .expect("value")
                .exact_rescale(2),
            Err(TradingValidationError::InexactRescale)
        );
    }

    #[test]
    fn unrealized_pnl_projection_respects_direction_and_fractional_quantity_scale() {
        let entry = FixedPoint::try_new(10_000, 2).expect("entry");
        let mark = FixedPoint::try_new(10_025, 2).expect("mark");
        let point_value = FixedPoint::try_new(5_000, 2).expect("point value");
        assert_eq!(
            project_unrealized_pnl(
                entry,
                mark,
                FixedPoint::try_new(150, 2).expect("quantity"),
                point_value,
                2,
            ),
            Ok(FixedPoint::try_new(1_875, 2).expect("long pnl"))
        );
        assert_eq!(
            project_unrealized_pnl(
                entry,
                mark,
                FixedPoint::try_new(-2, 0).expect("quantity"),
                point_value,
                2,
            ),
            Ok(FixedPoint::try_new(-2_500, 2).expect("short pnl"))
        );
    }

    #[test]
    fn high_precision_crypto_pnl_reduces_before_narrowing_to_i64() {
        let entry = FixedPoint::try_new(8_400_000_000_000, 8).expect("entry");
        let mark = FixedPoint::try_new(8_410_000_000_000, 8).expect("mark");
        let quantity = FixedPoint::try_new(1_500_000_000, 8).expect("quantity");
        let point_value = FixedPoint::try_new(100_000_000, 8).expect("point value");
        assert_eq!(
            project_unrealized_pnl(entry, mark, quantity, point_value, 2),
            Ok(FixedPoint::try_new(150_000, 2).expect("pnl"))
        );
    }

    #[test]
    fn pnl_projection_rounds_sub_cent_values_instead_of_failing_market_updates() {
        let entry = FixedPoint::try_new(10_000, 2).expect("entry");
        let mark = FixedPoint::try_new(10_001, 2).expect("mark");
        let point_value = FixedPoint::try_new(1, 0).expect("point value");
        let positive = FixedPoint::try_new(50, 2).expect("0.5 quantity");
        let negative = FixedPoint::try_new(-50, 2).expect("-0.5 quantity");

        assert_eq!(
            project_unrealized_pnl(entry, mark, positive, point_value, 2),
            Ok(FixedPoint::try_new(1, 2).expect("rounded pnl"))
        );
        assert_eq!(
            project_unrealized_pnl(entry, mark, negative, point_value, 2),
            Ok(FixedPoint::try_new(-1, 2).expect("rounded pnl"))
        );
    }

    #[test]
    fn simulated_accounts_are_visually_explicit() {
        let account = TradingAccount {
            id: TradingAccountId::try_new("paper-1").expect("id"),
            display_name: "Practice".to_string(),
            environment: AccountEnvironment::Simulated,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: None,
        };
        assert_eq!(
            account.validate(),
            Err(TradingValidationError::SimulationLabelMissing)
        );
    }

    #[test]
    fn order_lifecycle_validates_partial_and_terminal_filled_quantity() {
        let quantity = FixedPoint::try_new(4, 0).expect("quantity");
        let mut order = Order {
            id: OrderId::try_new("order-1").expect("order id"),
            client_order_id: ClientOrderId::try_new("client-1").expect("client id"),
            account_id: TradingAccountId::try_new("account-1").expect("account id"),
            instrument_id: InstrumentId::try_new("instrument:test:ES").expect("instrument"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Day,
            quantity,
            filled_quantity: FixedPoint::try_new(1, 0).expect("filled quantity"),
            limit_price: Some(FixedPoint::try_new(5_000, 2).expect("limit")),
            stop_price: None,
            status: OrderStatus::PartiallyFilled,
            submitted_unix_nanos: 1,
            provenance: TradingProvenance {
                venue_id: "test".to_string(),
                provider_id: "test".to_string(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: 1,
            },
        };
        assert!(order.validate().is_ok());
        assert!(order.status.is_open());
        assert!(order.status.is_executable());

        order.status = OrderStatus::Filled;
        assert_eq!(
            order.validate(),
            Err(TradingValidationError::InvalidFilledQuantity)
        );
        order.filled_quantity = quantity;
        assert!(order.validate().is_ok());
        assert!(!order.status.is_open());
        assert!(!order.status.is_executable());
    }
}
