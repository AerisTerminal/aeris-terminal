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
    fn invalid_digit_counts_are_rejected() {
        assert!(PriceScale::new(-1).is_err());
        assert!(PriceScale::new(11).is_err());
    }
}
