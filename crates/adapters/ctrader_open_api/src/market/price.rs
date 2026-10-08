use super::MarketDecodeError;

/// Wire prices are integers in 1/100000 of a price unit.
pub const WIRE_PRICE_DIGITS: u8 = 5;
const MAXIMUM_SYMBOL_DIGITS: u8 = 10;

/// Fixed-point scale of one symbol: a canonical price `p` means `p / 10^digits`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PriceScale {
    digits: u8,
}

impl PriceScale {
    /// # Errors
    /// Rejects negative or implausibly large digit counts.
    pub fn new(digits: i32) -> Result<Self, MarketDecodeError> {
        u8::try_from(digits)
            .ok()
            .filter(|digits| *digits <= MAXIMUM_SYMBOL_DIGITS)
            .map(|digits| Self { digits })
            .ok_or(MarketDecodeError::InvalidField("digits"))
    }

    #[must_use]
    pub const fn digits(self) -> u8 {
        self.digits
    }

    /// Rescale a wire price without rounding.
    ///
    /// # Errors
    /// Returns `InexactPrice` when the value has precision beyond the symbol
    /// digits, or `InvalidField` on overflow.
    pub fn from_wire(self, wire: i64) -> Result<i64, MarketDecodeError> {
        if self.digits <= WIRE_PRICE_DIGITS {
            let divisor = 10_i64.pow(u32::from(WIRE_PRICE_DIGITS - self.digits));
            if wire % divisor != 0 {
                return Err(MarketDecodeError::InexactPrice);
            }
            Ok(wire / divisor)
        } else {
            wire.checked_mul(10_i64.pow(u32::from(self.digits - WIRE_PRICE_DIGITS)))
                .ok_or(MarketDecodeError::InvalidField("price"))
        }
    }

    /// Rescale a symbol-scale price or distance to wire 1/100000 units without rounding.
    ///
    /// # Errors
    /// Returns `InexactPrice` when the wire scale cannot hold the value, or
    /// `InvalidField` on overflow.
    pub fn to_wire(self, price: i64) -> Result<i64, MarketDecodeError> {
        if self.digits <= WIRE_PRICE_DIGITS {
            price
                .checked_mul(10_i64.pow(u32::from(WIRE_PRICE_DIGITS - self.digits)))
                .ok_or(MarketDecodeError::InvalidField("price"))
        } else {
            let divisor = 10_i64.pow(u32::from(self.digits - WIRE_PRICE_DIGITS));
            if price % divisor != 0 {
                return Err(MarketDecodeError::InexactPrice);
            }
            Ok(price / divisor)
        }
    }

    /// The wire double for a positive symbol-scale price: the nearest double to its exact
    /// decimal, which is what the server parses back to the same decimal.
    ///
    /// # Errors
    /// Rejects zero or negative prices.
    pub fn to_decimal(self, price: i64) -> Result<f64, MarketDecodeError> {
        if price <= 0 {
            return Err(MarketDecodeError::InvalidField("price"));
        }
        let text = self.decimal_text(price);
        text.parse()
            .map_err(|_| MarketDecodeError::InvalidField("price"))
    }

    /// A positive wire double as a symbol-scale price, only when it is exactly the double
    /// of a decimal with at most `digits` places; anything finer is rejected, not rounded.
    ///
    /// # Errors
    /// Rejects non-finite, non-positive, out-of-range or inexact values.
    pub fn from_decimal(self, value: f64) -> Result<i64, MarketDecodeError> {
        if !value.is_finite() || value <= 0.0 {
            return Err(MarketDecodeError::InvalidField("price"));
        }
        let digits = usize::from(self.digits);
        let rounded = format!("{value:.digits$}");
        let price: i64 = rounded
            .replace('.', "")
            .parse()
            .map_err(|_| MarketDecodeError::InvalidField("price"))?;
        if self.to_decimal(price)?.to_bits() == value.to_bits() {
            Ok(price)
        } else {
            Err(MarketDecodeError::InexactPrice)
        }
    }

    fn decimal_text(self, price: i64) -> String {
        let digits = usize::from(self.digits);
        if digits == 0 {
            return price.to_string();
        }
        let divisor = 10_i64.pow(u32::from(self.digits));
        format!("{}.{:0digits$}", price / divisor, price % divisor)
    }

    /// Rescale a strictly positive unsigned wire price.
    ///
    /// # Errors
    /// Rejects zero, out-of-range, or inexact prices.
    pub fn from_wire_positive(self, wire: u64) -> Result<i64, MarketDecodeError> {
        let wire = i64::try_from(wire).map_err(|_| MarketDecodeError::InvalidField("price"))?;
        if wire <= 0 {
            return Err(MarketDecodeError::InvalidField("price"));
        }
        self.from_wire(wire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_prices_rescale_exactly_to_symbol_digits() {
        let five = PriceScale::new(5).expect("digits 5");
        let three = PriceScale::new(3).expect("digits 3");
        let two = PriceScale::new(2).expect("digits 2");
        assert_eq!(five.from_wire(108_543).expect("1.08543"), 108_543);
        assert_eq!(two.from_wire(15_012_000).expect("150.12"), 15_012);
        assert_eq!(three.from_wire(15_802_300).expect("158.023"), 158_023);
        assert_eq!(
            PriceScale::new(0)
                .expect("0")
                .from_wire(4_200_000)
                .expect("42"),
            42
        );
        assert_eq!(
            PriceScale::new(7)
                .expect("7")
                .from_wire(108_543)
                .expect("x"),
            10_854_300
        );
    }

    #[test]
    fn non_representable_prices_are_rejected_not_rounded() {
        let two = PriceScale::new(2).expect("digits 2");
        assert!(matches!(
            two.from_wire(15_012_300),
            Err(MarketDecodeError::InexactPrice)
        ));
        assert!(matches!(
            PriceScale::new(3).expect("3").from_wire(15_802_301),
            Err(MarketDecodeError::InexactPrice)
        ));
        assert!(matches!(
            two.from_wire_positive(0),
            Err(MarketDecodeError::InvalidField("price"))
        ));
        assert!(matches!(
            two.from_wire_positive(u64::MAX),
            Err(MarketDecodeError::InvalidField("price"))
        ));
        assert!(matches!(
            PriceScale::new(10).expect("10").from_wire(i64::MAX / 10),
            Err(MarketDecodeError::InvalidField("price"))
        ));
    }

    #[test]
    fn decimal_prices_round_trip_exactly_and_finer_values_are_rejected() {
        let five = PriceScale::new(5).expect("digits 5");
        let two = PriceScale::new(2).expect("digits 2");
        let zero = PriceScale::new(0).expect("digits 0");
        assert_eq!(
            five.to_decimal(108_543).expect("decimal").to_bits(),
            1.08543_f64.to_bits()
        );
        assert_eq!(five.from_decimal(1.08543).expect("price"), 108_543);
        assert_eq!(two.from_decimal(150.1).expect("price"), 15_010);
        assert_eq!(zero.from_decimal(42.0).expect("price"), 42);
        assert_eq!(five.from_decimal(0.00001).expect("price"), 1);
        for price in [1, 99_999, 100_000, 108_543, 999_999_999] {
            let decimal = five.to_decimal(price).expect("decimal");
            assert_eq!(five.from_decimal(decimal).expect("round trip"), price);
        }
        assert!(matches!(
            two.from_decimal(150.123),
            Err(MarketDecodeError::InexactPrice)
        ));
        assert!(matches!(
            five.from_decimal(1.085_431),
            Err(MarketDecodeError::InexactPrice)
        ));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(five.from_decimal(bad).is_err());
        }
        assert!(five.to_decimal(0).is_err());
    }

    #[test]
    fn symbol_prices_rescale_exactly_to_wire_units() {
        assert_eq!(
            PriceScale::new(5).expect("5").to_wire(108_543).expect("w"),
            108_543
        );
        assert_eq!(
            PriceScale::new(2).expect("2").to_wire(15_012).expect("w"),
            15_012_000
        );
        assert_eq!(
            PriceScale::new(7)
                .expect("7")
                .to_wire(10_854_300)
                .expect("w"),
            108_543
        );
        assert!(matches!(
            PriceScale::new(7).expect("7").to_wire(10_854_301),
            Err(MarketDecodeError::InexactPrice)
        ));
        assert!(PriceScale::new(0).expect("0").to_wire(i64::MAX).is_err());
    }

    #[test]
    fn invalid_digit_counts_are_rejected() {
        assert!(PriceScale::new(-1).is_err());
        assert!(PriceScale::new(11).is_err());
    }
}
