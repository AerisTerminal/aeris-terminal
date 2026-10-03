use crate::ProtocolError;
use core::{cmp::Ordering, fmt};

/// Maximum decimal scale carried across the Rithmic wire boundary.
pub const MAXIMUM_RITHMIC_DECIMAL_SCALE: u8 = 18;
#[cfg(any(rithmic_kit, test))]
const MAXIMUM_DECIMAL_TEXT_BYTES: usize = 64;

/// Signed fixed-point value exchanged with the order and P&L plants.
///
/// Rithmic carries prices as IEEE-754 doubles and many account values as decimal
/// strings. Both cross this boundary only through exact conversions: a value is
/// accepted when its decimal form round-trips without rounding, and rejected
/// otherwise. The scale is part of the value and is never inferred by callers.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct RithmicDecimal {
    units: i64,
    scale: u8,
}

impl RithmicDecimal {
    /// Creates a value from an integer coefficient and an explicit decimal scale.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidNumber`] when `scale` exceeds
    /// [`MAXIMUM_RITHMIC_DECIMAL_SCALE`].
    pub const fn try_new(units: i64, scale: u8) -> Result<Self, ProtocolError> {
        if scale > MAXIMUM_RITHMIC_DECIMAL_SCALE {
            return Err(ProtocolError::InvalidNumber("decimal.scale"));
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

    /// Converts to `target_scale` only when the exact value is preserved, for
    /// example to express a provider fill price at the instrument price scale.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnrepresentableDecimal`] when the conversion would
    /// round or overflow, or [`ProtocolError::InvalidNumber`] for an invalid scale.
    pub fn rescale_exact(self, target_scale: u8) -> Result<Self, ProtocolError> {
        const FIELD: &str = "decimal.rescale";
        if target_scale > MAXIMUM_RITHMIC_DECIMAL_SCALE {
            return Err(ProtocolError::InvalidNumber("decimal.scale"));
        }
        let units = match target_scale.cmp(&self.scale) {
            Ordering::Equal => self.units,
            Ordering::Greater => self
                .units
                .checked_mul(power_of_ten(target_scale - self.scale))
                .ok_or(ProtocolError::UnrepresentableDecimal(FIELD))?,
            Ordering::Less => {
                let factor = power_of_ten(self.scale - target_scale);
                if self.units % factor != 0 {
                    return Err(ProtocolError::UnrepresentableDecimal(FIELD));
                }
                self.units / factor
            }
        };
        Ok(Self {
            units,
            scale: target_scale,
        })
    }

    /// Reports numeric equality independent of scale.
    #[must_use]
    pub fn same_value(self, other: Self) -> bool {
        let scale = self.scale.max(other.scale);
        widened(self, scale) == widened(other, scale)
    }

    /// Converts a provider double using the shortest decimal that round-trips to
    /// the received IEEE-754 value, so no digit is invented or rounded away.
    #[cfg(any(rithmic_kit, test))]
    pub(crate) fn from_wire_f64(field: &'static str, value: f64) -> Result<Self, ProtocolError> {
        if !value.is_finite() {
            return Err(ProtocolError::InvalidNumber(field));
        }
        Self::from_decimal_text(field, &value.to_string())
            .map_err(|_| ProtocolError::UnrepresentableDecimal(field))
    }

    /// Parses a provider decimal string such as `-1250.50`; its scale is the
    /// number of fractional digits the provider sent.
    #[cfg(any(rithmic_kit, test))]
    pub(crate) fn from_wire_str(field: &'static str, value: &str) -> Result<Self, ProtocolError> {
        if value.is_empty() {
            return Err(ProtocolError::EmptyField(field));
        }
        Self::from_decimal_text(field, value)
    }

    /// Produces the wire double for this value, rejecting any value whose double
    /// would not convert back to exactly the same decimal.
    #[cfg(any(rithmic_kit, test))]
    pub(crate) fn to_wire_f64(self, field: &'static str) -> Result<f64, ProtocolError> {
        let value = self
            .to_string()
            .parse::<f64>()
            .map_err(|_| ProtocolError::UnrepresentableDecimal(field))?;
        let round_trip = Self::from_wire_f64(field, value)?;
        if !round_trip.same_value(self) {
            return Err(ProtocolError::UnrepresentableDecimal(field));
        }
        Ok(value)
    }

    #[cfg(any(rithmic_kit, test))]
    fn from_decimal_text(field: &'static str, text: &str) -> Result<Self, ProtocolError> {
        if text.len() > MAXIMUM_DECIMAL_TEXT_BYTES {
            return Err(ProtocolError::FieldTooLong {
                field,
                maximum: MAXIMUM_DECIMAL_TEXT_BYTES,
            });
        }
        let (negative, magnitude) = match text.strip_prefix('-') {
            Some(magnitude) => (true, magnitude),
            None => (false, text),
        };
        let (whole, fraction) = match magnitude.split_once('.') {
            Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
            Some(_) => return Err(ProtocolError::InvalidNumber(field)),
            None => (magnitude, ""),
        };
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(ProtocolError::InvalidNumber(field));
        }
        let scale = u8::try_from(fraction.len())
            .ok()
            .filter(|scale| *scale <= MAXIMUM_RITHMIC_DECIMAL_SCALE)
            .ok_or(ProtocolError::UnrepresentableDecimal(field))?;
        let mut units: i64 = 0;
        for digit in whole.bytes().chain(fraction.bytes()) {
            units = units
                .checked_mul(10)
                .and_then(|units| {
                    let digit = i64::from(digit - b'0');
                    if negative {
                        units.checked_sub(digit)
                    } else {
                        units.checked_add(digit)
                    }
                })
                .ok_or(ProtocolError::UnrepresentableDecimal(field))?;
        }
        Ok(Self { units, scale })
    }
}

impl fmt::Display for RithmicDecimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.units < 0 { "-" } else { "" };
        let magnitude = self.units.unsigned_abs();
        if self.scale == 0 {
            return write!(formatter, "{sign}{magnitude}");
        }
        let divisor = 10_u64.pow(u32::from(self.scale));
        write!(
            formatter,
            "{sign}{}.{:0width$}",
            magnitude / divisor,
            magnitude % divisor,
            width = usize::from(self.scale)
        )
    }
}

impl fmt::Debug for RithmicDecimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RithmicDecimal({self})")
    }
}

fn power_of_ten(exponent: u8) -> i64 {
    10_i64.pow(u32::from(exponent))
}

fn widened(value: RithmicDecimal, scale: u8) -> i128 {
    i128::from(value.units) * 10_i128.pow(u32::from(scale - value.scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimal(units: i64, scale: u8) -> RithmicDecimal {
        RithmicDecimal::try_new(units, scale).expect("fixture scale is bounded")
    }

    #[test]
    fn instrument_scaled_prices_round_trip_through_wire_doubles() {
        for (price, expected) in [
            (decimal(451_225, 2), 4_512.25),
            (decimal(1, 1), 0.1),
            (decimal(-375, 2), -3.75),
            (decimal(2_015_000_000, 7), 201.5),
            (decimal(7, 0), 7.0),
        ] {
            let wire = price.to_wire_f64("price").expect("tick price is exact");
            assert!((wire - expected).abs() < f64::EPSILON);
            let decoded = RithmicDecimal::from_wire_f64("price", wire).expect("wire decodes");
            assert!(decoded.same_value(price));
            assert_eq!(
                decoded.rescale_exact(price.scale()).expect("exact rescale"),
                price
            );
        }
    }

    #[test]
    fn non_representable_values_are_rejected_not_rounded() {
        assert_eq!(
            decimal(1_234_567_890_123_456_789, 2).to_wire_f64("price"),
            Err(ProtocolError::UnrepresentableDecimal("price"))
        );
        assert_eq!(
            RithmicDecimal::from_wire_f64("price", f64::NAN),
            Err(ProtocolError::InvalidNumber("price"))
        );
        assert_eq!(
            RithmicDecimal::from_wire_f64("price", 1e-30),
            Err(ProtocolError::UnrepresentableDecimal("price"))
        );
        assert_eq!(
            decimal(451_225, 2).rescale_exact(1),
            Err(ProtocolError::UnrepresentableDecimal("decimal.rescale"))
        );
        assert_eq!(
            RithmicDecimal::try_new(1, 19),
            Err(ProtocolError::InvalidNumber("decimal.scale"))
        );
    }

    #[test]
    fn provider_decimal_strings_keep_their_explicit_scale() {
        assert_eq!(
            RithmicDecimal::from_wire_str("balance", "-1250.50").expect("decimal parses"),
            decimal(-125_050, 2)
        );
        assert_eq!(
            RithmicDecimal::from_wire_str("balance", "0").expect("integer parses"),
            decimal(0, 0)
        );
        for malformed in [
            "", "1.", ".5", "1e5", "+1", "1,000.00", "nan", "--1", "1.2.3",
        ] {
            assert!(
                RithmicDecimal::from_wire_str("balance", malformed).is_err(),
                "{malformed:?} must be rejected"
            );
        }
        assert_eq!(decimal(-5, 3).to_string(), "-0.005");
        assert_eq!(decimal(451_225, 2).to_string(), "4512.25");
    }
}
