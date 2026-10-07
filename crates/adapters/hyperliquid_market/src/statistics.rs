//! Listed-market statistics for screening, decoded from public asset contexts.
//!
//! Statistics are display context only, like [`crate::HyperliquidMarketContext`], and never
//! enter candle construction. They use scales chosen for whole-market screening: day notional
//! volume regularly exceeds what ten fractional places can hold in `i64`, so notionals are
//! quantized to cents. A field that is absent or fails to decode stays `None`, so one bad
//! value never hides the rest of a market's row.

use crate::decimal::{
    FUNDING_RATE_SCALE, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE,
    parse_aggregate_decimal_to_fixed,
};

/// Scale for screening notionals (day volume and open interest value): cents.
pub const STATISTICS_NOTIONAL_SCALE: u32 = 2;
/// Hyperliquid perpetual funding accrues hourly; the context reports the hourly rate.
pub const FUNDING_INTERVAL_SECONDS: u32 = 3_600;

/// Screening statistics for one market.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HyperliquidMarketStatistics {
    /// Mark price at [`NORMALIZED_PRICE_SCALE`].
    pub mark_price: Option<i64>,
    /// Price one day earlier at [`NORMALIZED_PRICE_SCALE`].
    pub previous_day_price: Option<i64>,
    /// Day notional volume at [`STATISTICS_NOTIONAL_SCALE`].
    pub day_notional_volume: Option<i64>,
    /// Open interest valued at the mark price, at [`STATISTICS_NOTIONAL_SCALE`].
    /// Spot markets report no open interest.
    pub open_interest_notional: Option<i64>,
    /// Hourly funding rate at [`FUNDING_RATE_SCALE`]. Spot markets report no funding.
    pub funding_rate: Option<i64>,
}

/// Decodes one `metaAndAssetCtxs`/`spotMetaAndAssetCtxs` context object.
#[must_use]
pub fn decode_market_statistics(context: &serde_json::Value) -> HyperliquidMarketStatistics {
    let fixed = |key: &str, scale: u32| {
        context
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(|text| parse_aggregate_decimal_to_fixed(text, scale).ok())
    };
    let mark_price = fixed("markPx", NORMALIZED_PRICE_SCALE);
    let open_interest = fixed("openInterest", NORMALIZED_QUANTITY_SCALE);
    HyperliquidMarketStatistics {
        mark_price,
        previous_day_price: fixed("prevDayPx", NORMALIZED_PRICE_SCALE),
        day_notional_volume: fixed("dayNtlVlm", STATISTICS_NOTIONAL_SCALE),
        open_interest_notional: open_interest
            .zip(mark_price)
            .and_then(|(quantity, price)| notional(quantity, price)),
        funding_rate: fixed("funding", FUNDING_RATE_SCALE),
    }
}

/// Quantity × price, rescaled from the two normalized scales to cents.
fn notional(quantity: i64, price: i64) -> Option<i64> {
    let product_scale = NORMALIZED_QUANTITY_SCALE + NORMALIZED_PRICE_SCALE;
    let divisor = 10_i128.pow(product_scale - STATISTICS_NOTIONAL_SCALE);
    i64::try_from(i128::from(quantity) * i128::from(price) / divisor).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn perp_context_decodes_every_statistic_at_its_scale() {
        // Observed public BTC context shape.
        let statistics = decode_market_statistics(&json!({
            "funding": "0.0000125", "openInterest": "35241.53662", "prevDayPx": "79521.0",
            "dayNtlVlm": "1793093225.5723297596", "oraclePx": "79916.0", "markPx": "79884.0",
        }));
        assert_eq!(statistics.mark_price, Some(7_988_400_000_000));
        assert_eq!(statistics.previous_day_price, Some(7_952_100_000_000));
        // Above the ten-place notional range of `i64`, still exact to the cent.
        assert_eq!(statistics.day_notional_volume, Some(179_309_322_557));
        // 35241.53662 × 79884.0 = 2815234911.35208 → cents, truncated.
        assert_eq!(statistics.open_interest_notional, Some(281_523_491_135));
        assert_eq!(statistics.funding_rate, Some(125_000));
    }

    #[test]
    fn spot_context_reports_no_funding_or_open_interest() {
        let statistics = decode_market_statistics(&json!({
            "prevDayPx": "4.5", "dayNtlVlm": "1200.25", "markPx": "4.75", "midPx": "4.751",
            "circulatingSupply": "1000000",
        }));
        assert_eq!(statistics.mark_price, Some(475_000_000));
        assert_eq!(statistics.day_notional_volume, Some(120_025));
        assert!(statistics.funding_rate.is_none());
        assert!(statistics.open_interest_notional.is_none());
    }

    #[test]
    fn one_malformed_field_leaves_the_others_intact() {
        let statistics = decode_market_statistics(&json!({
            "markPx": "abc", "prevDayPx": "2.0", "dayNtlVlm": 15.0,
        }));
        assert!(statistics.mark_price.is_none());
        assert_eq!(statistics.previous_day_price, Some(200_000_000));
        // Contexts carry decimals as strings; a bare number is not trusted.
        assert!(statistics.day_notional_volume.is_none());
    }
}
