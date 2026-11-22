//! Bybit V5 REST kline page and WebSocket live kline decoding.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
    bar_period_millis,
};
use aeris_market_data::{BarPeriod, MarketBar};
use serde::Deserialize;

/// Maximum bars returned by one Bybit V5 `/v5/market/kline` request.
pub const MAXIMUM_BYBIT_CANDLES: usize = 1_000;
const NANOS_PER_MILLI: i64 = 1_000_000;

/// Maps an Aeris [`BarPeriod`] to Bybit V5's native kline interval string.
///
/// # Errors
/// Returns an error for unsupported periods.
pub fn bybit_interval_for_period(period: BarPeriod) -> CryptoResult<&'static str> {
    match period {
        BarPeriod::Time { seconds } => match seconds {
            60 => Ok("1"),
            180 => Ok("3"),
            300 => Ok("5"),
            900 => Ok("15"),
            1_800 => Ok("30"),
            3_600 => Ok("60"),
            7_200 => Ok("120"),
            14_400 => Ok("240"),
            21_600 => Ok("360"),
            43_200 => Ok("720"),
            _ => Err(CryptoError::Unsupported(format!(
                "bybit does not support {seconds}s candles"
            ))),
        },
        BarPeriod::Session { days: 1 } => Ok("D"),
        BarPeriod::Week { weeks: 1 } => Ok("W"),
        BarPeriod::Month { months: 1 } => Ok("M"),
        _ => Err(CryptoError::Unsupported(
            "bybit does not support this bar interval".to_string(),
        )),
    }
}

/// Maps a Bybit V5 wire interval string back to an Aeris [`BarPeriod`].
#[must_use]
pub fn period_for_bybit_interval(interval: &str) -> Option<BarPeriod> {
    match interval {
        "1" => BarPeriod::time(60).ok(),
        "3" => BarPeriod::time(180).ok(),
        "5" => BarPeriod::time(300).ok(),
        "15" => BarPeriod::time(900).ok(),
        "30" => BarPeriod::time(1_800).ok(),
        "60" => BarPeriod::time(3_600).ok(),
        "120" => BarPeriod::time(7_200).ok(),
        "240" => BarPeriod::time(14_400).ok(),
        "360" => BarPeriod::time(21_600).ok(),
        "720" => BarPeriod::time(43_200).ok(),
        "D" => BarPeriod::session(1).ok(),
        "W" => BarPeriod::week(1).ok(),
        "M" => BarPeriod::month(1).ok(),
        _ => None,
    }
}

/// Decoded REST candle page split into closed bars and optional forming bar.
#[derive(Clone, Debug)]
pub struct BybitCandlePage {
    pub bars: Vec<MarketBar>,
    pub forming: Option<MarketBar>,
    pub handoff_close_millis: Option<i64>,
}

/// Decoded WebSocket live kline update.
#[derive(Clone, Debug)]
pub struct BybitLiveCandle {
    pub bar: MarketBar,
    pub is_closed: bool,
    pub close_millis: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireKlineEnvelope {
    ret_code: i64,
    result: WireKlineResult,
}

#[derive(Deserialize)]
struct WireKlineResult {
    list: Vec<[RawDecimal; 7]>,
}

/// Decodes Bybit V5 `/v5/market/kline` (reversing newest-first rows into ascending bars).
///
/// # Errors
/// Returns an error if the JSON is malformed, `retCode != 0`, or any bar field is invalid.
pub fn decode_candle_page(
    raw: &[u8],
    period: BarPeriod,
    now_millis: i64,
) -> CryptoResult<BybitCandlePage> {
    let envelope: WireKlineEnvelope = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("bybit kline payload is malformed".to_string())
    })?;
    if envelope.ret_code != 0 {
        return Err(CryptoError::Transport(format!(
            "bybit kline returned retCode {}",
            envelope.ret_code
        )));
    }
    let step_ms = bar_period_millis(period)?;
    let mut rows = envelope.result.list;
    rows.reverse();

    let mut bars = Vec::with_capacity(rows.len());
    let mut forming = None;
    let mut handoff_close_millis = None;

    for (index, row) in rows.into_iter().enumerate() {
        let open_ms: i64 = row[0].as_str().parse().map_err(|_| {
            CryptoError::MalformedPayload("bybit kline timestamp is malformed".to_string())
        })?;
        let close_ms = open_ms.checked_add(step_ms).ok_or_else(|| {
            CryptoError::MalformedPayload("bybit kline close timestamp overflowed".to_string())
        })?;
        let open_unix_nanos = open_ms.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
            CryptoError::MalformedPayload("bybit kline open timestamp overflowed".to_string())
        })?;
        let bar = MarketBar {
            source_sequence: u64::try_from(index + 1).map_err(|_| {
                CryptoError::MalformedPayload("bybit kline sequence overflowed".to_string())
            })?,
            exchange_timestamp_seconds: open_unix_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_unix_nanos,
            open: row[1].to_fixed(NORMALIZED_PRICE_SCALE)?,
            high: row[2].to_fixed(NORMALIZED_PRICE_SCALE)?,
            low: row[3].to_fixed(NORMALIZED_PRICE_SCALE)?,
            close: row[4].to_fixed(NORMALIZED_PRICE_SCALE)?,
            volume: row[5].to_fixed_aggregate(NORMALIZED_QUANTITY_SCALE)?,
        };
        bar.validate()
            .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
        if close_ms <= now_millis {
            handoff_close_millis = Some(close_ms);
            bars.push(bar);
        } else {
            handoff_close_millis = Some(open_ms);
            forming = Some(bar);
        }
    }

    Ok(BybitCandlePage {
        bars,
        forming,
        handoff_close_millis,
    })
}

#[derive(Deserialize)]
struct WireWsKlineItem {
    start: i64,
    end: i64,
    interval: String,
    open: RawDecimal,
    close: RawDecimal,
    high: RawDecimal,
    low: RawDecimal,
    volume: RawDecimal,
    confirm: bool,
}

/// Decodes one Bybit WS `kline.{iv}.{SYM}` `data` array into live candle updates.
///
/// # Errors
/// Returns an error if the JSON or bar values are malformed.
pub fn decode_live_candle(
    data: &serde_json::value::RawValue,
) -> CryptoResult<Vec<(String, BybitLiveCandle)>> {
    let items: Vec<WireWsKlineItem> = serde_json::from_str(data.get()).map_err(|_| {
        CryptoError::MalformedPayload("bybit ws kline payload is malformed".to_string())
    })?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let open_unix_nanos = item.start.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
            CryptoError::MalformedPayload("bybit ws kline start overflowed".to_string())
        })?;
        let close_ms = item.end.saturating_add(1);
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: open_unix_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_unix_nanos,
            open: item.open.to_fixed(NORMALIZED_PRICE_SCALE)?,
            high: item.high.to_fixed(NORMALIZED_PRICE_SCALE)?,
            low: item.low.to_fixed(NORMALIZED_PRICE_SCALE)?,
            close: item.close.to_fixed(NORMALIZED_PRICE_SCALE)?,
            volume: item.volume.to_fixed_aggregate(NORMALIZED_QUANTITY_SCALE)?,
        };
        bar.validate()
            .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
        out.push((
            item.interval,
            BybitLiveCandle {
                bar,
                is_closed: item.confirm,
                close_millis: close_ms,
            },
        ));
    }
    Ok(out)
}
