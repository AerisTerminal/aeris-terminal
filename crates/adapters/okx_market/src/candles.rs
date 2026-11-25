//! OKX V5 REST candle page and WebSocket live candle decoding.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
    bar_period_millis,
};
use aeris_market_data::{BarPeriod, MarketBar};
use serde::Deserialize;

use crate::identity::OkxMarketKind;

/// Maximum bars returned by one OKX V5 `/api/v5/market/candles` request.
pub const MAXIMUM_OKX_CANDLES: usize = 300;
const NANOS_PER_MILLI: i64 = 1_000_000;

/// Maps an Aeris [`BarPeriod`] to OKX V5's native bar string.
///
/// # Errors
/// Returns an error for unsupported periods.
pub fn okx_interval_for_period(period: BarPeriod) -> CryptoResult<&'static str> {
    match period {
        BarPeriod::Time { seconds } => match seconds {
            60 => Ok("1m"),
            180 => Ok("3m"),
            300 => Ok("5m"),
            900 => Ok("15m"),
            1_800 => Ok("30m"),
            3_600 => Ok("1H"),
            7_200 => Ok("2H"),
            14_400 => Ok("4H"),
            21_600 => Ok("6H"),
            43_200 => Ok("12H"),
            86_400 => Ok("1D"),
            _ => Err(CryptoError::Unsupported(format!(
                "okx does not support {seconds}s candles"
            ))),
        },
        BarPeriod::Session { days: 1 } => Ok("1D"),
        BarPeriod::Week { weeks: 1 } => Ok("1W"),
        BarPeriod::Month { months: 1 } => Ok("1M"),
        _ => Err(CryptoError::Unsupported(
            "okx does not support this bar interval".to_string(),
        )),
    }
}

/// Maps an OKX V5 bar string back to an Aeris [`BarPeriod`].
#[must_use]
pub fn period_for_okx_interval(interval: &str) -> Option<BarPeriod> {
    match interval {
        "1m" => BarPeriod::time(60).ok(),
        "3m" => BarPeriod::time(180).ok(),
        "5m" => BarPeriod::time(300).ok(),
        "15m" => BarPeriod::time(900).ok(),
        "30m" => BarPeriod::time(1_800).ok(),
        "1H" => BarPeriod::time(3_600).ok(),
        "2H" => BarPeriod::time(7_200).ok(),
        "4H" => BarPeriod::time(14_400).ok(),
        "6H" => BarPeriod::time(21_600).ok(),
        "12H" => BarPeriod::time(43_200).ok(),
        "1D" | "1Dutc" => BarPeriod::time(86_400).ok(),
        "1W" | "1Wutc" => BarPeriod::week(1).ok(),
        "1M" | "1Mutc" => BarPeriod::month(1).ok(),
        _ => None,
    }
}

/// Decoded REST candle page split into closed bars and optional forming bar.
#[derive(Clone, Debug)]
pub struct OkxCandlePage {
    pub bars: Vec<MarketBar>,
    pub forming: Option<MarketBar>,
    pub handoff_close_millis: Option<i64>,
}

/// Decoded WebSocket live candle update.
#[derive(Clone, Debug)]
pub struct OkxLiveCandle {
    pub bar: MarketBar,
    pub is_closed: bool,
    pub close_millis: i64,
}

#[derive(Deserialize)]
struct WireCandleEnvelope {
    code: String,
    data: Vec<Vec<RawDecimal>>,
}

/// Decodes OKX V5 `/api/v5/market/candles` (reversing newest-first rows into ascending bars).
///
/// Uses base-asset volume (`row[6]` = `volCcy` for SWAP, `row[5]` = `vol` for Spot).
///
/// # Errors
/// Returns an error if the JSON is malformed, `code != "0"`, or any bar field is invalid.
pub fn decode_candle_page(
    raw: &[u8],
    kind: OkxMarketKind,
    period: BarPeriod,
) -> CryptoResult<OkxCandlePage> {
    let env: WireCandleEnvelope = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("okx candles payload is malformed".to_string())
    })?;
    if env.code != "0" {
        return Err(CryptoError::Transport(format!(
            "okx candles returned code {}",
            env.code
        )));
    }
    let step_ms = bar_period_millis(period)?;
    let mut rows = env.data;
    rows.reverse();

    let mut bars = Vec::with_capacity(rows.len());
    let mut forming = None;
    let mut handoff_close_millis = None;

    for (index, row) in rows.into_iter().enumerate() {
        if row.len() < 9 {
            return Err(CryptoError::MalformedPayload(
                "okx candle row is truncated".to_string(),
            ));
        }
        let open_ms: i64 = row[0].as_str().parse().map_err(|_| {
            CryptoError::MalformedPayload("okx candle timestamp is malformed".to_string())
        })?;
        let close_ms = open_ms.checked_add(step_ms).ok_or_else(|| {
            CryptoError::MalformedPayload("okx candle close timestamp overflowed".to_string())
        })?;
        let open_unix_nanos = open_ms.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
            CryptoError::MalformedPayload("okx candle open timestamp overflowed".to_string())
        })?;
        let vol_idx = match kind {
            OkxMarketKind::Swap => 6,
            OkxMarketKind::Spot => 5,
        };
        let bar = MarketBar {
            source_sequence: u64::try_from(index + 1).map_err(|_| {
                CryptoError::MalformedPayload("okx candle sequence overflowed".to_string())
            })?,
            exchange_timestamp_seconds: open_unix_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_unix_nanos,
            open: row[1].to_fixed(NORMALIZED_PRICE_SCALE)?,
            high: row[2].to_fixed(NORMALIZED_PRICE_SCALE)?,
            low: row[3].to_fixed(NORMALIZED_PRICE_SCALE)?,
            close: row[4].to_fixed(NORMALIZED_PRICE_SCALE)?,
            volume: row[vol_idx].to_fixed_aggregate(NORMALIZED_QUANTITY_SCALE)?,
        };
        bar.validate()
            .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
        let confirmed = row[8].as_str() == "1";
        if confirmed {
            handoff_close_millis = Some(close_ms);
            bars.push(bar);
        } else {
            handoff_close_millis = Some(open_ms);
            forming = Some(bar);
        }
    }

    Ok(OkxCandlePage {
        bars,
        forming,
        handoff_close_millis,
    })
}

/// Decodes one OKX WS `candle*` `data` array into live candle updates.
///
/// # Errors
/// Returns an error if the JSON or bar fields are invalid.
pub fn decode_live_candle(
    data: &serde_json::value::RawValue,
    kind: OkxMarketKind,
    period: BarPeriod,
) -> CryptoResult<Vec<OkxLiveCandle>> {
    let rows: Vec<Vec<RawDecimal>> = serde_json::from_str(data.get()).map_err(|_| {
        CryptoError::MalformedPayload("okx ws candle payload is malformed".to_string())
    })?;
    let step_ms = bar_period_millis(period)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if row.len() < 9 {
            continue;
        }
        let open_ms: i64 = row[0].as_str().parse().map_err(|_| {
            CryptoError::MalformedPayload("okx ws candle timestamp is malformed".to_string())
        })?;
        let close_ms = open_ms.checked_add(step_ms).ok_or_else(|| {
            CryptoError::MalformedPayload("okx ws candle close timestamp overflowed".to_string())
        })?;
        let open_unix_nanos = open_ms.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
            CryptoError::MalformedPayload("okx ws candle open timestamp overflowed".to_string())
        })?;
        let vol_idx = match kind {
            OkxMarketKind::Swap => 6,
            OkxMarketKind::Spot => 5,
        };
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: open_unix_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_unix_nanos,
            open: row[1].to_fixed(NORMALIZED_PRICE_SCALE)?,
            high: row[2].to_fixed(NORMALIZED_PRICE_SCALE)?,
            low: row[3].to_fixed(NORMALIZED_PRICE_SCALE)?,
            close: row[4].to_fixed(NORMALIZED_PRICE_SCALE)?,
            volume: row[vol_idx].to_fixed_aggregate(NORMALIZED_QUANTITY_SCALE)?,
        };
        bar.validate()
            .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
        out.push(OkxLiveCandle {
            bar,
            is_closed: row[8].as_str() == "1",
            close_millis: close_ms,
        });
    }
    Ok(out)
}
