//! Coinbase Advanced Trade REST candle page decoding.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
    bar_period_millis,
};
use aeris_market_data::{BarPeriod, MarketBar};
use serde::Deserialize;

/// Maximum bars returned by one Coinbase `/market/products/{id}/candles` request.
pub const MAXIMUM_COINBASE_CANDLES: usize = 350;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const MILLIS_PER_SECOND: i64 = 1_000;

/// Maps an Aeris [`BarPeriod`] to Coinbase Advanced Trade's `granularity` query value.
///
/// # Errors
/// Returns an error for unsupported periods.
pub fn coinbase_interval_for_period(period: BarPeriod) -> CryptoResult<&'static str> {
    match period {
        BarPeriod::Time { seconds } => match seconds {
            60 => Ok("ONE_MINUTE"),
            300 => Ok("FIVE_MINUTE"),
            900 => Ok("FIFTEEN_MINUTE"),
            1_800 => Ok("THIRTY_MINUTE"),
            3_600 => Ok("ONE_HOUR"),
            7_200 => Ok("TWO_HOUR"),
            21_600 => Ok("SIX_HOUR"),
            86_400 => Ok("ONE_DAY"),
            _ => Err(CryptoError::Unsupported(format!(
                "coinbase does not support {seconds}s candles"
            ))),
        },
        BarPeriod::Session { days: 1 } => Ok("ONE_DAY"),
        _ => Err(CryptoError::Unsupported(
            "coinbase does not support this bar interval".to_string(),
        )),
    }
}

/// Maps a Coinbase `granularity` string back to an Aeris [`BarPeriod`].
#[must_use]
pub fn period_for_coinbase_interval(interval: &str) -> Option<BarPeriod> {
    match interval {
        "ONE_MINUTE" => BarPeriod::time(60).ok(),
        "FIVE_MINUTE" => BarPeriod::time(300).ok(),
        "FIFTEEN_MINUTE" => BarPeriod::time(900).ok(),
        "THIRTY_MINUTE" => BarPeriod::time(1_800).ok(),
        "ONE_HOUR" => BarPeriod::time(3_600).ok(),
        "TWO_HOUR" => BarPeriod::time(7_200).ok(),
        "SIX_HOUR" => BarPeriod::time(21_600).ok(),
        "ONE_DAY" => BarPeriod::time(86_400).ok(),
        _ => None,
    }
}

/// Decoded REST candle page split into closed bars and optional forming bar.
#[derive(Clone, Debug)]
pub struct CoinbaseCandlePage {
    pub bars: Vec<MarketBar>,
    pub forming: Option<MarketBar>,
    pub handoff_boundary_millis: Option<i64>,
}

#[derive(Deserialize)]
struct WireCandlesResponse {
    #[serde(default)]
    candles: Vec<WireCandle>,
}

#[derive(Deserialize)]
struct WireCandle {
    start: RawDecimal,
    low: RawDecimal,
    high: RawDecimal,
    open: RawDecimal,
    close: RawDecimal,
    volume: RawDecimal,
}

/// Decodes Coinbase `/api/v3/brokerage/market/products/{id}/candles` into ascending closed bars
/// plus an optional open forming bar.
///
/// # Errors
/// Returns an error if the JSON or any bar field is malformed.
pub fn decode_candle_page(
    raw: &[u8],
    period: BarPeriod,
    now_millis: i64,
) -> CryptoResult<CoinbaseCandlePage> {
    let response: WireCandlesResponse = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("coinbase candles payload is malformed".to_string())
    })?;
    let step_ms = bar_period_millis(period)?;
    let mut parsed = Vec::with_capacity(response.candles.len());
    for row in response.candles {
        let open_secs: i64 = row.start.as_str().parse().map_err(|_| {
            CryptoError::MalformedPayload("coinbase candle timestamp is malformed".to_string())
        })?;
        let open_ms = open_secs.checked_mul(MILLIS_PER_SECOND).ok_or_else(|| {
            CryptoError::MalformedPayload("coinbase candle timestamp overflowed".to_string())
        })?;
        parsed.push((open_ms, open_secs, row));
    }
    parsed.sort_by_key(|(open_ms, ..)| *open_ms);
    parsed.dedup_by_key(|(open_ms, ..)| *open_ms);

    let mut bars = Vec::with_capacity(parsed.len());
    let mut forming = None;
    let mut handoff_boundary_millis = None;
    let mut sequence: u64 = 0;

    for (open_ms, open_secs, row) in parsed {
        if open_ms > now_millis {
            continue;
        }
        let close_ms = open_ms.checked_add(step_ms).ok_or_else(|| {
            CryptoError::MalformedPayload("coinbase candle close timestamp overflowed".to_string())
        })?;
        let open_unix_nanos = open_secs.checked_mul(NANOS_PER_SECOND).ok_or_else(|| {
            CryptoError::MalformedPayload("coinbase candle open timestamp overflowed".to_string())
        })?;
        sequence = sequence.saturating_add(1);
        let bar = MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: open_secs,
            exchange_timestamp_unix_nanos: open_unix_nanos,
            open: row.open.to_fixed(NORMALIZED_PRICE_SCALE)?,
            high: row.high.to_fixed(NORMALIZED_PRICE_SCALE)?,
            low: row.low.to_fixed(NORMALIZED_PRICE_SCALE)?,
            close: row.close.to_fixed(NORMALIZED_PRICE_SCALE)?,
            volume: row.volume.to_fixed_aggregate(NORMALIZED_QUANTITY_SCALE)?,
        };
        bar.validate()
            .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
        if close_ms <= now_millis {
            handoff_boundary_millis = Some(close_ms);
            bars.push(bar);
        } else {
            // The REST candle already reflects trades up to `now_millis`, so live
            // trade-built updates resume strictly after `now_millis` without double counting.
            handoff_boundary_millis = Some(now_millis.max(open_ms));
            forming = Some(bar);
        }
    }

    Ok(CoinbaseCandlePage {
        bars,
        forming,
        handoff_boundary_millis,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_intervals_and_rejects_unsupported() {
        assert_eq!(
            coinbase_interval_for_period(BarPeriod::time(60).unwrap()).unwrap(),
            "ONE_MINUTE"
        );
        assert_eq!(
            coinbase_interval_for_period(BarPeriod::time(300).unwrap()).unwrap(),
            "FIVE_MINUTE"
        );
        assert_eq!(
            coinbase_interval_for_period(BarPeriod::time(3_600).unwrap()).unwrap(),
            "ONE_HOUR"
        );
        assert_eq!(
            coinbase_interval_for_period(BarPeriod::session(1).unwrap()).unwrap(),
            "ONE_DAY"
        );
        assert!(coinbase_interval_for_period(BarPeriod::time(180).unwrap()).is_err());
        assert_eq!(
            period_for_coinbase_interval("FIFTEEN_MINUTE"),
            BarPeriod::time(900).ok()
        );
    }

    #[test]
    fn decodes_descending_candles_into_ascending_closed_and_forming() {
        let raw = br#"{
            "candles": [
                {"start": "1700000120", "low": "100.0", "high": "105.0", "open": "101.0", "close": "104.0", "volume": "12.5"},
                {"start": "1700000060", "low": "99.0", "high": "102.0", "open": "100.0", "close": "101.0", "volume": "8.0"}
            ]
        }"#;
        let period = BarPeriod::time(60).unwrap();
        let now_millis = 1_700_000_150_000;
        let page = decode_candle_page(raw, period, now_millis).expect("decode candles");
        assert_eq!(page.bars.len(), 1);
        assert_eq!(page.bars[0].exchange_timestamp_seconds, 1_700_000_060);
        assert_eq!(page.bars[0].source_sequence, 1);
        let forming = page.forming.expect("open forming bar");
        assert_eq!(forming.exchange_timestamp_seconds, 1_700_000_120);
        assert_eq!(forming.source_sequence, 2);
        assert_eq!(page.handoff_boundary_millis, Some(now_millis));
    }
}
