//! Binance REST kline page and WebSocket live kline decoding.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
};
use aeris_market_data::{BarPeriod, MarketBar};
use serde::Deserialize;

/// Maximum bars returned by one Binance USDT-M Futures `/fapi/v1/klines` request.
pub const MAXIMUM_BINANCE_FUTURES_CANDLES: usize = 1_500;
/// Maximum bars returned by one Binance Spot `/api/v3/klines` request.
pub const MAXIMUM_BINANCE_SPOT_CANDLES: usize = 1_000;

const NANOS_PER_MILLI: i64 = 1_000_000;

/// Maps an Aeris [`BarPeriod`] to Binance's native kline interval string.
///
/// # Errors
/// Returns an error for unsupported periods (such as tick bars).
pub fn binance_interval_for_period(period: BarPeriod) -> CryptoResult<&'static str> {
    match period {
        BarPeriod::Time { seconds } => match seconds {
            60 => Ok("1m"),
            180 => Ok("3m"),
            300 => Ok("5m"),
            900 => Ok("15m"),
            1_800 => Ok("30m"),
            3_600 => Ok("1h"),
            7_200 => Ok("2h"),
            14_400 => Ok("4h"),
            21_600 => Ok("6h"),
            28_800 => Ok("8h"),
            43_200 => Ok("12h"),
            _ => Err(CryptoError::Unsupported(format!(
                "binance does not support {seconds}s candles"
            ))),
        },
        BarPeriod::Session { days: 1 } => Ok("1d"),
        BarPeriod::Session { days: 3 } => Ok("3d"),
        BarPeriod::Week { weeks: 1 } => Ok("1w"),
        BarPeriod::Month { months: 1 } => Ok("1M"),
        _ => Err(CryptoError::Unsupported(
            "binance does not support this bar interval".to_string(),
        )),
    }
}

/// Maps a Binance wire interval string back to an Aeris [`BarPeriod`].
#[must_use]
pub fn period_for_binance_interval(interval: &str) -> Option<BarPeriod> {
    match interval {
        "1m" => BarPeriod::time(60).ok(),
        "3m" => BarPeriod::time(180).ok(),
        "5m" => BarPeriod::time(300).ok(),
        "15m" => BarPeriod::time(900).ok(),
        "30m" => BarPeriod::time(1_800).ok(),
        "1h" => BarPeriod::time(3_600).ok(),
        "2h" => BarPeriod::time(7_200).ok(),
        "4h" => BarPeriod::time(14_400).ok(),
        "6h" => BarPeriod::time(21_600).ok(),
        "8h" => BarPeriod::time(28_800).ok(),
        "12h" => BarPeriod::time(43_200).ok(),
        "1d" => BarPeriod::session(1).ok(),
        "3d" => BarPeriod::session(3).ok(),
        "1w" => BarPeriod::week(1).ok(),
        "1M" => BarPeriod::month(1).ok(),
        _ => None,
    }
}

/// Decoded REST candle page split into closed bars and an optional forming bar.
#[derive(Clone, Debug)]
pub struct BinanceCandlePage {
    pub bars: Vec<MarketBar>,
    pub forming: Option<MarketBar>,
    pub handoff_close_millis: Option<i64>,
}

/// Decoded WebSocket live kline update.
#[derive(Clone, Debug)]
pub struct BinanceLiveCandle {
    pub bar: MarketBar,
    pub is_closed: bool,
    pub close_millis: i64,
}

#[derive(Deserialize)]
struct WireKlineRow(
    i64,        // 0: open time ms
    RawDecimal, // 1: open
    RawDecimal, // 2: high
    RawDecimal, // 3: low
    RawDecimal, // 4: close
    RawDecimal, // 5: volume
    i64,        // 6: close time ms
    serde::de::IgnoredAny,
    serde::de::IgnoredAny,
    serde::de::IgnoredAny,
    serde::de::IgnoredAny,
    serde::de::IgnoredAny,
);

/// Decodes a Binance REST `/klines` JSON array into closed and forming bars.
///
/// # Errors
/// Returns an error if the JSON payload or any bar field is malformed.
pub fn decode_candle_page(raw: &[u8], now_millis: i64) -> CryptoResult<BinanceCandlePage> {
    let rows: Vec<WireKlineRow> = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("binance klines payload is malformed".to_string())
    })?;
    let mut bars = Vec::with_capacity(rows.len());
    let mut forming = None;
    let mut handoff_close_millis = None;

    for (index, row) in rows.into_iter().enumerate() {
        let open_unix_nanos = row.0.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
            CryptoError::MalformedPayload("binance kline open timestamp overflowed".to_string())
        })?;
        let bar = MarketBar {
            source_sequence: u64::try_from(index + 1).map_err(|_| {
                CryptoError::MalformedPayload("binance kline sequence overflowed".to_string())
            })?,
            exchange_timestamp_seconds: open_unix_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_unix_nanos,
            open: row.1.to_fixed(NORMALIZED_PRICE_SCALE)?,
            high: row.2.to_fixed(NORMALIZED_PRICE_SCALE)?,
            low: row.3.to_fixed(NORMALIZED_PRICE_SCALE)?,
            close: row.4.to_fixed(NORMALIZED_PRICE_SCALE)?,
            volume: row.5.to_fixed_aggregate(NORMALIZED_QUANTITY_SCALE)?,
        };
        bar.validate()
            .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
        if row.6 < now_millis {
            handoff_close_millis = Some(row.6.saturating_add(1));
            bars.push(bar);
        } else {
            handoff_close_millis = Some(row.0);
            forming = Some(bar);
        }
    }

    Ok(BinanceCandlePage {
        bars,
        forming,
        handoff_close_millis,
    })
}

#[derive(Deserialize)]
struct WireWsKlineEvent {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "k")]
    kline: WireWsKlineBody,
}

#[derive(Deserialize)]
struct WireWsKlineBody {
    #[serde(rename = "t")]
    open_ms: i64,
    #[serde(rename = "T")]
    close_ms: i64,
    #[serde(rename = "i")]
    interval: String,
    #[serde(rename = "o")]
    open: RawDecimal,
    #[serde(rename = "h")]
    high: RawDecimal,
    #[serde(rename = "l")]
    low: RawDecimal,
    #[serde(rename = "c")]
    close: RawDecimal,
    #[serde(rename = "v")]
    volume: RawDecimal,
    #[serde(rename = "x")]
    is_closed: bool,
}

/// Decodes one Binance WebSocket `kline` event payload into `(symbol, interval, BinanceLiveCandle)`.
///
/// # Errors
/// Returns an error if the JSON payload or any bar field is malformed.
pub fn decode_live_candle(
    payload: &serde_json::value::RawValue,
) -> CryptoResult<(String, String, BinanceLiveCandle)> {
    let event: WireWsKlineEvent = serde_json::from_str(payload.get()).map_err(|_| {
        CryptoError::MalformedPayload("binance ws kline payload is malformed".to_string())
    })?;
    let k = event.kline;
    let open_unix_nanos = k.open_ms.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
        CryptoError::MalformedPayload("binance ws kline open timestamp overflowed".to_string())
    })?;
    let bar = MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: open_unix_nanos.div_euclid(1_000_000_000),
        exchange_timestamp_unix_nanos: open_unix_nanos,
        open: k.open.to_fixed(NORMALIZED_PRICE_SCALE)?,
        high: k.high.to_fixed(NORMALIZED_PRICE_SCALE)?,
        low: k.low.to_fixed(NORMALIZED_PRICE_SCALE)?,
        close: k.close.to_fixed(NORMALIZED_PRICE_SCALE)?,
        volume: k.volume.to_fixed_aggregate(NORMALIZED_QUANTITY_SCALE)?,
    };
    bar.validate()
        .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
    Ok((
        event.symbol.to_ascii_uppercase(),
        k.interval,
        BinanceLiveCandle {
            bar,
            is_closed: k.is_closed,
            close_millis: k.close_ms.saturating_add(1),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rest_kline_page_splitting_closed_and_forming() {
        let json = br#"[
            [1700000000000,"50000.0","50100.0","49900.0","50050.0","12.34",1700000059999,"0",100,"0","0","0"],
            [1700000060000,"50050.0","50200.0","50000.0","50150.0","5.5",1700000119999,"0",42,"0","0","0"]
        ]"#;
        let page = decode_candle_page(json, 1_700_000_080_000).unwrap();
        assert_eq!(page.bars.len(), 1);
        assert!(page.forming.is_some());
        assert_eq!(page.bars[0].open, 5_000_000_000_000);
        assert_eq!(page.handoff_close_millis, Some(1_700_000_060_000));
    }
}
