//! Trade decoding to canonical events with sequence and dedup policy.
//!
//! `sequence_num` must advance by exactly one per message across the
//! connection; a gap is surfaced so the session reconnects and resnapshots
//! instead of silently continuing. Trade identifiers deduplicate the
//! snapshot/update overlap inside one bounded window per product.

use crate::errors::CoinbaseError;
use crate::fixed_point::FixedPointValue;
use crate::messages::ChannelMessage;

/// Maximum retained trade identifiers in the dedup window.
const MAXIMUM_DEDUP_ENTRIES: usize = 4_096;

/// One canonical trade with the full timestamp vocabulary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalTrade {
    pub product_id: String,
    pub trade_id: String,
    pub price: FixedPointValue,
    pub size: FixedPointValue,
    pub maker_side_buy: bool,
    pub trade_time_unix_nanos: i64,
    pub provider_timestamp_unix_nanos: i64,
    pub sequence_num: u64,
}

/// Aggregate decode counters for evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecoderMetrics {
    pub messages: u64,
    pub trades: u64,
    pub heartbeats: u64,
    pub duplicates_dropped: u64,
    pub sequence_gaps: u64,
}

/// Strict decoder over one connection's message stream.
pub struct CoinbaseDecoder {
    next_sequence: Option<u64>,
    dedup: std::collections::VecDeque<(String, String)>,
    dedup_set: std::collections::HashSet<(String, String)>,
    metrics: DecoderMetrics,
}

impl CoinbaseDecoder {
    /// Creates a decoder with no sequence baseline.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_sequence: None,
            dedup: std::collections::VecDeque::new(),
            dedup_set: std::collections::HashSet::new(),
            metrics: DecoderMetrics::default(),
        }
    }

    /// Current decode counters.
    #[must_use]
    pub const fn metrics(&self) -> DecoderMetrics {
        self.metrics
    }

    /// Decodes one channel message, returning only new trades.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed messages or a sequence gap.
    pub fn decode(&mut self, bytes: &[u8]) -> Result<Vec<CanonicalTrade>, CoinbaseError> {
        self.decode_with_liveness(bytes).map(|(trades, _)| trades)
    }

    pub(crate) fn decode_with_liveness(
        &mut self,
        bytes: &[u8],
    ) -> Result<(Vec<CanonicalTrade>, bool), CoinbaseError> {
        let message: ChannelMessage =
            serde_json::from_slice(bytes).map_err(|_| CoinbaseError::InvalidMessage)?;
        self.metrics.messages += 1;
        if let Some(next) = self.next_sequence
            && message.sequence_num != next
        {
            self.metrics.sequence_gaps += 1;
            return Err(CoinbaseError::SequenceGap {
                expected: next,
                actual: message.sequence_num,
            });
        }
        self.next_sequence = Some(message.sequence_num.saturating_add(1));

        if message.channel == "heartbeats" {
            self.metrics.heartbeats += 1;
            return Ok((Vec::new(), true));
        }
        if message.channel != "market_trades" {
            return Ok((Vec::new(), false));
        }
        let provider_timestamp = parse_rfc3339_nanos(&message.timestamp)?;
        let mut trades = Vec::new();
        for event in &message.events {
            for trade in &event.trades {
                let dedup_key = (trade.product_id.clone(), trade.trade_id.clone());
                if !self.dedup_set.insert(dedup_key.clone()) {
                    self.metrics.duplicates_dropped += 1;
                    continue;
                }
                self.dedup.push_back(dedup_key);
                if self.dedup.len() > MAXIMUM_DEDUP_ENTRIES
                    && let Some(oldest) = self.dedup.pop_front()
                {
                    self.dedup_set.remove(&oldest);
                }
                trades.push(CanonicalTrade {
                    product_id: trade.product_id.clone(),
                    trade_id: trade.trade_id.clone(),
                    price: FixedPointValue::parse(&trade.price)?,
                    size: FixedPointValue::parse(&trade.size)?,
                    maker_side_buy: trade.side == "BUY",
                    trade_time_unix_nanos: parse_rfc3339_nanos(&trade.time)?,
                    provider_timestamp_unix_nanos: provider_timestamp,
                    sequence_num: message.sequence_num,
                });
                self.metrics.trades += 1;
            }
        }
        Ok((trades, true))
    }

    /// Resets sequence and dedup state for a reconnect.
    pub fn reset(&mut self) {
        self.next_sequence = None;
        self.dedup.clear();
        self.dedup_set.clear();
    }
}

impl Default for CoinbaseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses an RFC 3339 timestamp with fractional seconds into Unix nanoseconds.
///
/// # Errors
///
/// Returns an error for malformed or out-of-range timestamps.
pub fn parse_rfc3339_nanos(source: &str) -> Result<i64, CoinbaseError> {
    let invalid = || CoinbaseError::InvalidTimestamp(source.to_string());
    let date = source.get(..10).ok_or_else(invalid)?;
    let time = source
        .get(11..)
        .and_then(|rest| rest.strip_suffix('Z'))
        .ok_or_else(invalid)?;
    let year: i64 = date
        .get(0..4)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if date.get(4..5) != Some("-") || date.get(7..8) != Some("-") {
        return Err(invalid());
    }
    let month: i64 = date
        .get(5..7)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let day: i64 = date
        .get(8..10)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let hour: i64 = time
        .get(0..2)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if time.get(2..3) != Some(":") || time.get(5..6) != Some(":") {
        return Err(invalid());
    }
    let minute: i64 = time
        .get(3..5)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let second: i64 = time
        .get(6..8)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let fraction = time.get(8..).unwrap_or("");
    let fraction = fraction.strip_prefix('.').unwrap_or("");
    let mut nanos: i64 = 0;
    let mut scale: i64 = 100_000_000;
    if fraction.len() > 9 {
        return Err(invalid());
    }
    for character in fraction.chars() {
        if !character.is_ascii_digit() {
            return Err(invalid());
        }
        nanos += i64::from(character as u8 - b'0') * scale;
        scale /= 10;
    }
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(invalid());
    }
    let days = days_from_civil(
        year,
        u32::try_from(month).map_err(|_| invalid())?,
        u32::try_from(day).map_err(|_| invalid())?,
    );
    days.checked_mul(86_400)
        .and_then(|days| days.checked_add(hour * 3_600 + minute * 60 + second))
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .and_then(|total| total.checked_add(nanos))
        .ok_or_else(invalid)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month_i = i64::from(month);
    let doy =
        (153 * (if month_i > 2 {
            month_i - 3
        } else {
            month_i + 9
        }) + 2)
            / 5
            + i64::from(day - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::{CoinbaseDecoder, parse_rfc3339_nanos};

    fn trade_message(sequence: u64, trade_id: &str) -> String {
        format!(
            r#"{{"channel":"market_trades","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":{sequence},"events":[{{"type":"update","trades":[{{"trade_id":"{trade_id}","product_id":"BTC-USD","price":"67001.25","size":"0.0042","side":"SELL","time":"2023-02-09T20:19:34.265Z"}}]}}]}}"#
        )
    }

    fn heartbeat_message(sequence: u64) -> String {
        format!(
            r#"{{"channel":"heartbeats","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":{sequence},"events":[{{"heartbeat_counter":{sequence}}}]}}"#
        )
    }

    fn subscriptions_message(sequence: u64) -> String {
        format!(
            r#"{{"channel":"subscriptions","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":{sequence},"events":[]}}"#
        )
    }

    #[test]
    fn decodes_exact_fixed_point_trades() {
        let mut decoder = CoinbaseDecoder::new();
        let trades = decoder
            .decode(trade_message(0, "t-1").as_bytes())
            .expect("valid message decodes");
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price.mantissa, 6_700_125);
        assert_eq!(trades[0].price.scale, 2);
        assert_eq!(trades[0].size.mantissa, 42);
        assert_eq!(trades[0].size.scale, 4);
        assert!(!trades[0].maker_side_buy);
    }

    #[test]
    fn sequence_gap_is_rejected_not_hidden() {
        let mut decoder = CoinbaseDecoder::new();
        decoder
            .decode(trade_message(0, "t-1").as_bytes())
            .expect("baseline accepted");
        assert!(decoder.decode(trade_message(2, "t-2").as_bytes()).is_err());
        assert_eq!(decoder.metrics().sequence_gaps, 1);
    }

    #[test]
    fn every_provider_channel_advances_the_connection_sequence() {
        let mut decoder = CoinbaseDecoder::new();
        decoder
            .decode(subscriptions_message(0).as_bytes())
            .expect("first acknowledgement accepted");
        decoder
            .decode(trade_message(1, "t-1").as_bytes())
            .expect("trade baseline accepted");
        decoder
            .decode(subscriptions_message(2).as_bytes())
            .expect("second acknowledgement accepted");
        decoder
            .decode(heartbeat_message(3).as_bytes())
            .expect("heartbeat accepted");
        decoder
            .decode(trade_message(4, "t-2").as_bytes())
            .expect("next trade accepted");
        assert_eq!(decoder.metrics().sequence_gaps, 0);
        assert_eq!(decoder.metrics().heartbeats, 1);
        assert_eq!(decoder.metrics().trades, 2);
    }

    #[test]
    fn duplicate_trade_ids_drop_once() {
        let mut decoder = CoinbaseDecoder::new();
        decoder
            .decode(trade_message(0, "t-1").as_bytes())
            .expect("first accepted");
        let second = decoder
            .decode(trade_message(1, "t-1").as_bytes())
            .expect("second decodes");
        assert!(second.is_empty());
        assert_eq!(decoder.metrics().duplicates_dropped, 1);
    }

    #[test]
    fn rfc3339_parses_documented_timestamps() {
        assert_eq!(
            parse_rfc3339_nanos("1970-01-01T00:00:00Z").expect("epoch"),
            0
        );
        assert_eq!(
            parse_rfc3339_nanos("2019-08-14T20:42:27.265Z").expect("documented"),
            1_565_815_347_265_000_000
        );
        assert!(parse_rfc3339_nanos("2023-02-09T25:19:35Z").is_err());
        assert!(parse_rfc3339_nanos("not a date").is_err());
    }
}
