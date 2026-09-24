//! Native candle history and live candle mapping.
//!
//! Provider candles are the authority for every supported time-based
//! interval. REST history and live candle replacements merge by instrument,
//! interval, and candle-open timestamp with exactly one forming candle.
//! Trade volume is never added into provider candles.

use aeris_market_data::{BarPeriod, MarketBar};
use serde::Deserialize;

use crate::decimal::RawDecimal;

/// The API exposes only the latest 5,000 candles per request window.
pub const MAXIMUM_HYPERLIQUID_CANDLES: usize = 5_000;

/// One decoded candle page: closed history plus the still-open period.
#[derive(Clone, Debug, PartialEq)]
pub struct HyperliquidCandlePage {
    /// Provider-closed periods in open-time order (canonical history).
    pub bars: Vec<MarketBar>,
    /// The period the provider caught open, held live rather than in history.
    pub forming: Option<MarketBar>,
    /// Latest close timestamp (millis) covered by the provider response.
    pub handoff_close_millis: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct WireCandle {
    #[serde(default)]
    t: i64,
    #[serde(default)]
    T: i64,
    #[serde(default)]
    o: Option<RawDecimal>,
    #[serde(default)]
    h: Option<RawDecimal>,
    #[serde(default)]
    l: Option<RawDecimal>,
    #[serde(default)]
    c: Option<RawDecimal>,
    #[serde(default)]
    v: Option<RawDecimal>,
}

/// Maps a canonical time period to a Hyperliquid interval string.
///
/// Tick, session, week, and month periods have no native Hyperliquid candle
/// and fail with an explicit unsupported-history error instead of silent
/// reinterpretation.
///
/// # Errors
///
/// Returns an error when the period has no native Hyperliquid interval.
pub fn hyperliquid_interval_for_period(period: BarPeriod) -> Result<&'static str, String> {
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
            28_800 => Ok("8h"),
            43_200 => Ok("12h"),
            86_400 => Ok("1d"),
            _ => Err("hyperliquid history is unavailable for this interval".to_string()),
        },
        BarPeriod::Session { days: 3 } => Ok("3d"),
        BarPeriod::Week { weeks: 1 } => Ok("1w"),
        BarPeriod::Month { months: 1 } => Ok("1M"),
        _ => Err("hyperliquid history is unavailable for this interval".to_string()),
    }
}

/// Maps a Hyperliquid interval string back to a canonical period.
///
/// # Errors
///
/// Returns an error for an unknown interval string.
pub fn period_for_hyperliquid_interval(interval: &str) -> Result<BarPeriod, String> {
    match interval {
        "1m" => BarPeriod::time(60).map_err(|error| error.to_string()),
        "3m" => BarPeriod::time(180).map_err(|error| error.to_string()),
        "5m" => BarPeriod::time(300).map_err(|error| error.to_string()),
        "15m" => BarPeriod::time(900).map_err(|error| error.to_string()),
        "30m" => BarPeriod::time(1_800).map_err(|error| error.to_string()),
        "1h" => BarPeriod::time(3_600).map_err(|error| error.to_string()),
        "2h" => BarPeriod::time(7_200).map_err(|error| error.to_string()),
        "4h" => BarPeriod::time(14_400).map_err(|error| error.to_string()),
        "8h" => BarPeriod::time(28_800).map_err(|error| error.to_string()),
        "12h" => BarPeriod::time(43_200).map_err(|error| error.to_string()),
        "1d" => BarPeriod::time(86_400).map_err(|error| error.to_string()),
        "3d" => BarPeriod::session(3).map_err(|error| error.to_string()),
        "1w" => BarPeriod::week(1).map_err(|error| error.to_string()),
        "1M" => BarPeriod::month(1).map_err(|error| error.to_string()),
        _ => Err("hyperliquid interval is unsupported".to_string()),
    }
}

/// Decodes one `candleSnapshot` array into closed bars plus the open period.
///
/// The payload arrives as raw JSON text so fractional numbers never pass
/// through `f64`: REST snapshots emit decimal strings while the live candle
/// channel emits JSON numbers, and both stay exact. The live channel
/// delivers a single `Candle` object per update; history delivers `Candle[]`.
/// Both shapes decode here so a live replacement flows into the same merge
/// path without re-serialization.
///
/// `now_millis` decides which trailing candle is still forming: a candle
/// whose close time is after now stays out of `bars` and is returned as
/// `forming`. Malformed rows, non-monotonic opens, and OHLC violations fail
/// the page rather than fabricating history.
///
/// # Errors
///
/// Returns an error for malformed rows, out-of-order or invalid timestamps,
/// missing fields, invalid OHLCV, over-long pages, or sequence overflow.
#[allow(clippy::too_many_lines)]
pub fn decode_candle_page(
    payload: &serde_json::value::RawValue,
    period: BarPeriod,
    price_scale: u32,
    quantity_scale: u32,
    now_millis: i64,
) -> Result<HyperliquidCandlePage, String> {
    let rows: Vec<WireCandle> = serde_json::from_str(payload.get())
        .or_else(|_| serde_json::from_str::<WireCandle>(payload.get()).map(|row| vec![row]))
        .map_err(|_| "hyperliquid candles are malformed".to_string())?;
    if rows.len() > MAXIMUM_HYPERLIQUID_CANDLES {
        return Err("hyperliquid candle page exceeds the provider limit".to_string());
    }
    period.validate().map_err(|error| error.to_string())?;
    // Provider snapshots can contain an adjacent corrected row for the same
    // candle key. The later row is authoritative, matching the replacement
    // semantics of the live candle channel. Only an exact open+close key may
    // replace; backward time or a duplicate open with a different close still
    // fails closed.
    let mut normalized: Vec<WireCandle> = Vec::with_capacity(rows.len());
    for row in rows {
        if row.t < 0 || row.T <= row.t {
            return Err("hyperliquid candle timestamp is invalid".to_string());
        }
        if let Some(previous) = normalized.last() {
            if row.t < previous.t {
                return Err("hyperliquid candles are not in open-time order".to_string());
            }
            if row.t == previous.t {
                if row.T != previous.T {
                    return Err("hyperliquid duplicate candle interval is inconsistent".to_string());
                }
                if let Some(tail) = normalized.last_mut() {
                    *tail = row;
                } else {
                    return Err("hyperliquid candle normalization lost its tail".to_string());
                }
                continue;
            }
        }
        normalized.push(row);
    }
    let mut bars = Vec::with_capacity(normalized.len());
    let mut previous_open: Option<i64> = None;
    for (index, row) in normalized.iter().enumerate() {
        if let Some(previous) = previous_open
            && period
                .duration_nanos()
                .and_then(|duration| duration.checked_div(1_000_000))
                .is_some_and(|duration| row.t - previous != duration)
        {
            return Err("hyperliquid candle history has a time gap".to_string());
        }
        previous_open = Some(row.t);
        let (Some(open), Some(high), Some(low), Some(close), Some(volume)) =
            (&row.o, &row.h, &row.l, &row.c, &row.v)
        else {
            return Err("hyperliquid candle field is missing".to_string());
        };
        let open = open.to_fixed(price_scale)?;
        let high = high.to_fixed(price_scale)?;
        let low = low.to_fixed(price_scale)?;
        let close = close.to_fixed(price_scale)?;
        let volume = volume.to_fixed_aggregate(quantity_scale)?;
        if volume < 0 {
            return Err("hyperliquid candle volume is invalid".to_string());
        }
        if high < open.max(close) || low > open.min(close) || low > high {
            return Err("hyperliquid candle OHLC is invalid".to_string());
        }
        let open_nanos = row
            .t
            .checked_mul(1_000_000)
            .ok_or_else(|| "hyperliquid candle timestamp overflowed".to_string())?;
        let bar = MarketBar {
            source_sequence: u64::try_from(index + 1)
                .map_err(|_| "hyperliquid candle sequence overflowed".to_string())?,
            exchange_timestamp_seconds: open_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_nanos,
            open,
            high,
            low,
            close,
            volume,
        };
        bar.validate().map_err(|error| error.to_string())?;
        bars.push((row.T, bar));
    }
    let handoff_close_millis = normalized.last().map(|row| row.T);
    // Split closed history from the still-open period by close time.
    let mut closed = Vec::new();
    let mut forming = None;
    for (close_millis, bar) in bars {
        if close_millis > now_millis {
            if forming.is_some() {
                return Err("hyperliquid candle page has two open periods".to_string());
            }
            forming = Some(bar);
        } else {
            closed.push(bar);
        }
    }
    // Resequence: closed history keeps 1..N, forming (when present) is N+1.
    let mut sequenced = Vec::with_capacity(closed.len());
    for (index, mut bar) in closed.into_iter().enumerate() {
        bar.source_sequence = u64::try_from(index + 1)
            .map_err(|_| "hyperliquid candle sequence overflowed".to_string())?;
        sequenced.push(bar);
    }
    if let Some(mut open) = forming {
        open.source_sequence = u64::try_from(sequenced.len() + 1)
            .map_err(|_| "hyperliquid candle sequence overflowed".to_string())?;
        forming = Some(open);
    }
    Ok(HyperliquidCandlePage {
        bars: sequenced,
        forming,
        handoff_close_millis,
    })
}

/// One live candle update without canonical sequencing.
///
/// Source sequences are owned by the engine handoff: the same open
/// timestamp keeps the forming sequence while a newer period extends it.
/// Sequencing here would make a redelivered update look like a new bar, so
/// the wire body carries identity and values only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HyperliquidLiveCandle {
    /// Candle-open timestamp in unix nanos.
    pub open_nanos: i64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
}

/// Decodes one live `Candle` object into an unsequenced update.
///
/// Accepts the single-object update the live channel emits per frame. Array
/// pages decode through [`decode_candle_page`] instead; mixing the two
/// would let a history-shaped payload masquerade as one live replacement.
///
/// # Errors
///
/// Returns an error for malformed payloads, missing fields, invalid
/// timestamps, or invalid OHLCV.
pub fn decode_live_candle(
    payload: &serde_json::value::RawValue,
    price_scale: u32,
    quantity_scale: u32,
) -> Result<HyperliquidLiveCandle, String> {
    let row: WireCandle = serde_json::from_str(payload.get())
        .map_err(|_| "hyperliquid live candle is malformed".to_string())?;
    if row.t < 0 || row.T <= row.t {
        return Err("hyperliquid live candle timestamp is invalid".to_string());
    }
    let (Some(open), Some(high), Some(low), Some(close), Some(volume)) =
        (&row.o, &row.h, &row.l, &row.c, &row.v)
    else {
        return Err("hyperliquid candle field is missing".to_string());
    };
    let candle = HyperliquidLiveCandle {
        open_nanos: row
            .t
            .checked_mul(1_000_000)
            .ok_or_else(|| "hyperliquid candle timestamp overflowed".to_string())?,
        open: open.to_fixed(price_scale)?,
        high: high.to_fixed(price_scale)?,
        low: low.to_fixed(price_scale)?,
        close: close.to_fixed(price_scale)?,
        volume: volume.to_fixed_aggregate(quantity_scale)?,
    };
    if candle.volume < 0 {
        return Err("hyperliquid candle volume is invalid".to_string());
    }
    if candle.high < candle.open.max(candle.close)
        || candle.low > candle.open.min(candle.close)
        || candle.low > candle.high
    {
        return Err("hyperliquid candle OHLC is invalid".to_string());
    }
    Ok(candle)
}

/// Merges a live candle replacement into closed history by open timestamp.
///
/// Returns the updated closed tail plus whether the update replaced the
/// forming candle in place (`false`) or completed it and opened the next
/// period (`true`). Duplicate updates with identical OHLCV are idempotent.
///
/// # Errors
///
/// Returns an error for invalid bars, stale updates, non-canonical
/// rollovers, or sequence overflow.
pub fn merge_live_candle(
    closed: &mut Vec<MarketBar>,
    forming: &mut Option<MarketBar>,
    update: MarketBar,
) -> Result<bool, String> {
    update.validate().map_err(|error| error.to_string())?;
    if let Some(open) = forming {
        if open.exchange_timestamp_unix_nanos == update.exchange_timestamp_unix_nanos {
            if *open == update {
                return Ok(false);
            }
            *open = update;
            return Ok(false);
        }
        if update.exchange_timestamp_unix_nanos < open.exchange_timestamp_unix_nanos {
            return Err("hyperliquid live candle is stale".to_string());
        }
        // The open period completed: it joins closed history, the update
        // becomes the new forming candle.
        let mut completed = *open;
        completed.source_sequence = closed
            .last()
            .map_or(0, |bar| bar.source_sequence)
            .checked_add(1)
            .ok_or_else(|| "hyperliquid candle sequence overflowed".to_string())?;
        if closed.last().is_some_and(|last| {
            last.exchange_timestamp_unix_nanos >= completed.exchange_timestamp_unix_nanos
        }) {
            return Err("hyperliquid candle rollover is not canonical".to_string());
        }
        closed.push(completed);
        *open = update;
        return Ok(true);
    }
    // No forming candle yet: the update must extend history, never rewrite it.
    if closed.last().is_some_and(|last| {
        update.exchange_timestamp_unix_nanos <= last.exchange_timestamp_unix_nanos
    }) {
        // Exact duplicate of the tail is idempotent; anything older is stale.
        if closed.last().is_some_and(|last| *last == update) {
            return Ok(false);
        }
        return Err("hyperliquid live candle is stale".to_string());
    }
    *forming = Some(update);
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(_now: i64) -> serde_json::Value {
        json!([
            {"t": 60_000, "T": 120_000, "s": "1m", "i": "BTC",
             "o": "10.0", "h": "11.0", "l": "9.0", "c": "10.5", "v": "7.0", "n": 3},
            {"t": 120_000, "T": 180_000, "s": "1m", "i": "BTC",
             "o": "10.5", "h": "12.0", "l": "10.0", "c": "11.0", "v": "2.0", "n": 1},
        ])
    }

    /// Re-encodes a fixture exactly as the socket delivers it: raw text the
    /// decoder parses without any intermediate `Value` float conversion.
    fn raw(value: &serde_json::Value) -> Box<serde_json::value::RawValue> {
        serde_json::value::RawValue::from_string(value.to_string()).expect("fixture encodes")
    }

    #[test]
    fn history_splits_closed_bars_from_the_forming_candle() {
        // Now is inside the second candle: first is closed, second is open.
        let decoded = decode_candle_page(
            &raw(&page(150_000)),
            BarPeriod::time(60).unwrap(),
            6,
            6,
            150_000,
        )
        .expect("page");
        assert_eq!(decoded.bars.len(), 1);
        assert!(decoded.forming.is_some());
        assert_eq!(decoded.bars[0].source_sequence, 1);
        assert_eq!(decoded.forming.unwrap().source_sequence, 2);
        // Now past both closes: everything is closed history.
        let closed = decode_candle_page(
            &raw(&page(200_000)),
            BarPeriod::time(60).unwrap(),
            6,
            6,
            200_000,
        )
        .expect("closed");
        assert_eq!(closed.bars.len(), 2);
        assert!(closed.forming.is_none());
    }

    #[test]
    fn fixed_time_history_rejects_missing_buckets() {
        let payload = serde_json::value::to_raw_value(&json!([
            {"t": 60_000, "T": 119_999, "o": "10", "h": "10", "l": "10", "c": "10", "v": "1"},
            {"t": 180_000, "T": 239_999, "o": "10", "h": "10", "l": "10", "c": "10", "v": "1"}
        ]))
        .expect("fixture");
        assert_eq!(
            decode_candle_page(
                &payload,
                BarPeriod::time(60).expect("period"),
                8,
                8,
                300_000
            ),
            Err("hyperliquid candle history has a time gap".to_string())
        );
    }

    #[test]
    fn snapshot_duplicate_key_uses_the_later_provider_correction() {
        let payload = serde_json::value::to_raw_value(&json!([
            {"t": 60_000, "T": 119_999, "o": "10", "h": "11", "l": "9", "c": "10", "v": "0"},
            {"t": 60_000, "T": 119_999, "o": "12", "h": "13", "l": "11", "c": "12", "v": "0"},
            {"t": 120_000, "T": 179_999, "o": "12", "h": "12", "l": "12", "c": "12", "v": "1"}
        ]))
        .expect("fixture");
        let decoded = decode_candle_page(
            &payload,
            BarPeriod::time(60).expect("period"),
            8,
            8,
            200_000,
        )
        .expect("corrected duplicate decodes");
        assert_eq!(decoded.bars.len(), 2);
        assert_eq!(decoded.bars[0].open, 1_200_000_000);
        assert_eq!(decoded.bars[0].close, 1_200_000_000);

        let inconsistent = serde_json::value::to_raw_value(&json!([
            {"t": 60_000, "T": 119_999, "o": "10", "h": "10", "l": "10", "c": "10", "v": "1"},
            {"t": 60_000, "T": 120_000, "o": "10", "h": "10", "l": "10", "c": "10", "v": "1"}
        ]))
        .expect("fixture");
        assert_eq!(
            decode_candle_page(
                &inconsistent,
                BarPeriod::time(60).expect("period"),
                8,
                8,
                200_000,
            ),
            Err("hyperliquid duplicate candle interval is inconsistent".to_string())
        );
    }
    #[test]
    fn malformed_ohlc_timestamps_and_order_fail_closed() {
        let bad_ohlc = json!([
            {"t": 60_000, "T": 120_000, "o": "10.0", "h": "9.0",
             "l": "9.0", "c": "10.5", "v": "1.0", "n": 1},
        ]);
        assert!(
            decode_candle_page(&raw(&bad_ohlc), BarPeriod::time(60).unwrap(), 6, 6, 200_000)
                .is_err()
        );
        let unordered = json!([
            {"t": 120_000, "T": 180_000, "o": "10.0", "h": "11.0",
             "l": "9.0", "c": "10.5", "v": "1.0", "n": 1},
            {"t": 60_000, "T": 120_000, "o": "10.0", "h": "11.0",
             "l": "9.0", "c": "10.5", "v": "1.0", "n": 1},
        ]);
        assert!(
            decode_candle_page(
                &raw(&unordered),
                BarPeriod::time(60).unwrap(),
                6,
                6,
                200_000
            )
            .is_err()
        );
        let over_limit = serde_json::Value::Array(vec![
            json!({"t": 0, "T": 1, "o": "1.0", "h": "1.0", "l": "1.0",
                   "c": "1.0", "v": "1.0", "n": 1});
            MAXIMUM_HYPERLIQUID_CANDLES + 1
        ]);
        assert!(
            decode_candle_page(&raw(&over_limit), BarPeriod::time(60).unwrap(), 6, 6, 0).is_err()
        );
    }

    #[test]
    fn numeric_live_candles_decode_at_full_precision() {
        // The live channel emits numbers where the snapshot emits strings.
        let numeric = json!([
            {"t": 60_000, "T": 120_000, "s": "BTC", "i": "1m",
             "o": 29295.0, "h": 29309.0, "l": 29250.0, "c": 29258.0,
             "v": 0.98639, "n": 189},
        ]);
        let decoded =
            decode_candle_page(&raw(&numeric), BarPeriod::time(60).unwrap(), 8, 8, 200_000)
                .expect("numeric page");
        assert_eq!(decoded.bars.len(), 1);
        assert_eq!(decoded.bars[0].open, 2_929_500_000_000);
        assert_eq!(decoded.bars[0].volume, 98_639_000);
    }

    #[test]
    fn single_object_candles_decode_like_one_row_pages() {
        // Observed live update shape: one bare `Candle` object.
        let wire = serde_json::value::RawValue::from_string(
            r#"{"t":60000,"T":120000,"s":"BTC","i":"1m","o":"10.0","h":"11.0","l":"9.0","c":10.5,"v":"7.0","n":3}"#.to_string(),
        )
        .expect("wire encodes");
        let decoded = decode_candle_page(&wire, BarPeriod::time(60).unwrap(), 6, 6, 150_000)
            .expect("single object decodes");
        assert_eq!(
            decoded.bars.len() + usize::from(decoded.forming.is_some()),
            1
        );
        // Non-object, non-array payloads still fail closed.
        let bad = serde_json::value::RawValue::from_string("42".to_string()).expect("wire encodes");
        assert!(decode_candle_page(&bad, BarPeriod::time(60).unwrap(), 6, 6, 150_000).is_err());
    }

    #[test]
    fn live_candle_bodies_carry_values_without_sequences() {
        // Observed live update shape: one bare `Candle` object.
        let wire = serde_json::value::RawValue::from_string(
            r#"{"t":60000,"T":120000,"s":"BTC","i":"1m","o":"10.0","h":"11.0","l":"9.0","c":10.5,"v":"7.0","n":3}"#.to_string(),
        )
        .expect("wire encodes");
        let live = decode_live_candle(&wire, 6, 6).expect("live decodes");
        assert_eq!(live.open_nanos, 60_000_000_000);
        assert_eq!(live.close, 10_500_000);
        // Array pages are history-shaped, not live replacements.
        let page =
            serde_json::value::RawValue::from_string("[]".to_string()).expect("wire encodes");
        assert!(decode_live_candle(&page, 6, 6).is_err());
    }

    #[test]
    fn live_aggregate_volume_uses_the_same_precision_as_history() {
        let wire = serde_json::value::RawValue::from_string(
            r#"{"t":60000,"T":120000,"s":"BTC","i":"1m","o":"10.0","h":"11.0","l":"9.0","c":"10.5","v":"939217.2893600001","n":3}"#.to_string(),
        )
        .expect("wire encodes");
        let live = decode_live_candle(&wire, 8, 8).expect("aggregate volume decodes");
        let history = decode_candle_page(&wire, BarPeriod::time(60).unwrap(), 8, 8, 90_000)
            .expect("same candle decodes as history");
        assert_eq!(live.volume, 93_921_728_936_000);
        assert_eq!(history.forming.expect("forming candle").volume, live.volume);
    }

    #[test]
    fn unsupported_intervals_fail_with_an_explicit_boundary() {
        assert!(hyperliquid_interval_for_period(BarPeriod::tick(100).unwrap()).is_err());
        assert!(hyperliquid_interval_for_period(BarPeriod::Time { seconds: 30 }).is_err());
        assert_eq!(
            hyperliquid_interval_for_period(BarPeriod::time(60).unwrap()),
            Ok("1m")
        );
        assert_eq!(
            period_for_hyperliquid_interval("1h").unwrap(),
            BarPeriod::time(3_600).unwrap()
        );
    }

    #[test]
    fn live_replacements_roll_the_forming_candle_exactly_once() {
        let decoded = decode_candle_page(
            &raw(&page(150_000)),
            BarPeriod::time(60).unwrap(),
            6,
            6,
            150_000,
        )
        .expect("page");
        let mut closed = decoded.bars;
        let mut forming = decoded.forming;
        let first = forming.unwrap();
        // Duplicate replacement is idempotent.
        assert!(!merge_live_candle(&mut closed, &mut forming, first).unwrap());
        // A newer open timestamp completes the old forming candle.
        let mut next = first;
        next.exchange_timestamp_unix_nanos = 180_000_000_000;
        next.exchange_timestamp_seconds = 180;
        next.source_sequence = 3;
        assert!(merge_live_candle(&mut closed, &mut forming, next).unwrap());
        assert_eq!(closed.len(), 2);
        // Stale updates fail instead of rewriting closed history.
        assert!(merge_live_candle(&mut closed, &mut forming, first).is_err());
    }
}
