//! Trade decoding to canonical events with sequence and dedup policy.
//!
//! `market_trades` sequence numbers must advance by exactly one for the
//! subscribed product. Administrative acknowledgements and heartbeat messages
//! do not share that sequence domain. Trade identifiers deduplicate the
//! snapshot/update overlap inside one bounded window per product.

use crate::errors::CoinbaseError;
use crate::fixed_point::FixedPointValue;
use crate::messages::ChannelMessage;
use axiusflow_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};

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
    pub canonical_sequence: u64,
}

impl CanonicalTrade {
    /// Projects the exact Coinbase value into the provider-neutral fixed-point contract.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when configured scales lose precision, overflow,
    /// or produce an invalid canonical trade.
    pub fn to_market_trade(
        &self,
        price_scale: u8,
        quantity_scale: u8,
        session_generation: u64,
        received_unix_nanos: i64,
    ) -> Result<MarketTrade, CoinbaseError> {
        let instrument_id = instrument_id_for_product(&self.product_id)?;
        let trade = MarketTrade {
            metadata: EventMetadata {
                provider_id: crate::PROVIDER.to_string(),
                instrument_id,
                entitlement_id: crate::ENTITLEMENT_CLASS.to_string(),
                source_sequence: self.canonical_sequence,
                session_generation,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(self.trade_time_unix_nanos),
                    provider_unix_nanos: Some(self.provider_timestamp_unix_nanos),
                    received_unix_nanos,
                },
            },
            trade_id: self.trade_id.clone(),
            price: crate::mantissa_at_scale(self.price, price_scale)
                .map_err(|_| CoinbaseError::InvalidMessage)?,
            quantity: crate::mantissa_at_scale(self.size, quantity_scale)
                .map_err(|_| CoinbaseError::InvalidMessage)?,
            aggressor: if self.maker_side_buy {
                AggressorSide::Sell
            } else {
                AggressorSide::Buy
            },
        };
        trade
            .validate()
            .map_err(|_| CoinbaseError::InvalidMessage)?;
        Ok(trade)
    }
}

fn instrument_id_for_product(product_id: &str) -> Result<String, CoinbaseError> {
    crate::coinbase_instrument_id(product_id)
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
    next_trade_sequence_num: Option<u64>,
    dedup: std::collections::VecDeque<(String, String)>,
    dedup_set: std::collections::HashSet<(String, String)>,
    metrics: DecoderMetrics,
    next_trade_sequence: u64,
}

impl CoinbaseDecoder {
    /// Creates a decoder with no sequence baseline.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_trade_sequence_num: None,
            dedup: std::collections::VecDeque::new(),
            dedup_set: std::collections::HashSet::new(),
            metrics: DecoderMetrics::default(),
            next_trade_sequence: 1,
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
        self.decode_with_liveness(bytes)
            .map(|(trades, _, _, _)| trades)
    }

    pub(crate) fn decode_with_liveness(
        &mut self,
        bytes: &[u8],
    ) -> Result<(Vec<CanonicalTrade>, bool, bool, bool), CoinbaseError> {
        let message: ChannelMessage = serde_json::from_slice(bytes).map_err(|error| {
            let channel = serde_json::from_slice::<serde_json::Value>(bytes)
                .ok()
                .and_then(|value| value.get("channel")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_string());
            eprintln!("Coinbase {channel} message schema mismatch: {error}");
            CoinbaseError::InvalidMessage
        })?;
        self.metrics.messages += 1;
        if message.channel == "heartbeats" {
            self.metrics.heartbeats += 1;
            return Ok((Vec::new(), true, true, false));
        }
        if message.channel == "l2_data" {
            return Ok((Vec::new(), true, false, true));
        }
        if message.channel != "market_trades" {
            return Ok((Vec::new(), false, false, false));
        }
        if let Some(next) = self.next_trade_sequence_num {
            if message.sequence_num < next {
                return Ok((Vec::new(), true, false, false));
            }
            if message.sequence_num > next {
                self.metrics.sequence_gaps += 1;
            }
        }
        self.next_trade_sequence_num = message.sequence_num.checked_add(1);
        let provider_timestamp = parse_rfc3339_nanos(&message.timestamp)?;
        let mut trades = Vec::new();
        for event in &message.events {
            for trade in &event.trades {
                let maker_side_buy = match trade.side.as_str() {
                    "BUY" => true,
                    "SELL" => false,
                    _ => return Err(CoinbaseError::InvalidMessage),
                };
                let price = FixedPointValue::parse(&trade.price)?;
                let size = FixedPointValue::parse(&trade.size)?;
                let trade_time_unix_nanos = parse_rfc3339_nanos(&trade.time)?;
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
                    price,
                    size,
                    maker_side_buy,
                    trade_time_unix_nanos,
                    provider_timestamp_unix_nanos: provider_timestamp,
                    sequence_num: message.sequence_num,
                    canonical_sequence: self.next_trade_sequence,
                });
                self.next_trade_sequence = self
                    .next_trade_sequence
                    .checked_add(1)
                    .ok_or(CoinbaseError::InvalidMessage)?;
                self.metrics.trades += 1;
            }
        }
        Ok((trades, true, false, false))
    }

    /// Resets sequence and dedup state for a reconnect.
    pub fn reset(&mut self) {
        self.next_trade_sequence_num = None;
        self.dedup.clear();
        self.dedup_set.clear();
        self.next_trade_sequence = 1;
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
    use axiusflow_market_data::AggressorSide;

    fn trade_message(sequence: u64, trade_id: &str) -> String {
        format!(
            r#"{{"channel":"market_trades","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":{sequence},"events":[{{"type":"update","trades":[{{"trade_id":"{trade_id}","product_id":"BTC-USD","price":"67001.25","size":"0.0042","side":"SELL","time":"2023-02-09T20:19:34.265Z"}}]}}]}}"#
        )
    }

    fn batched_trade_message(sequence: u64) -> String {
        format!(
            r#"{{"channel":"market_trades","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":{sequence},"events":[{{"type":"update","trades":[{{"trade_id":"t-1","product_id":"BTC-USD","price":"67001.25","size":"0.0042","side":"SELL","time":"2023-02-09T20:19:34.265Z"}},{{"trade_id":"t-2","product_id":"BTC-USD","price":"67001.50","size":"0.0043","side":"BUY","time":"2023-02-09T20:19:34.266Z"}}]}}]}}"#
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
    fn decoded_trade_projects_into_provider_neutral_contract() {
        let mut decoder = CoinbaseDecoder::new();
        let trade = decoder
            .decode(trade_message(1, "t-1").as_bytes())
            .expect("valid message decodes")
            .pop()
            .expect("message contains one trade")
            .to_market_trade(2, 8, 9, 1_675_977_576_000_000_000)
            .expect("trade projects without precision loss");
        assert_eq!(trade.metadata.provider_id, crate::PROVIDER);
        assert_eq!(trade.metadata.instrument_id, "instrument:coinbase:btc:usd");
        assert_eq!(trade.metadata.entitlement_id, crate::ENTITLEMENT_CLASS);
        assert_eq!(trade.metadata.source_sequence, 1);
        assert_eq!(trade.metadata.session_generation, 9);
        assert_eq!(trade.price, 6_700_125);
        assert_eq!(trade.quantity, 420_000);
        assert_eq!(trade.aggressor, AggressorSide::Buy);
    }

    #[test]
    fn sequence_zero_projects_to_the_first_canonical_sequence() {
        let mut decoder = CoinbaseDecoder::new();
        let trade = decoder
            .decode(trade_message(0, "t-0").as_bytes())
            .expect("zero-based provider sequence decodes")
            .pop()
            .expect("message contains one trade")
            .to_market_trade(2, 8, 1, 1)
            .expect("zero-based provider sequence normalizes");
        assert_eq!(trade.metadata.source_sequence, 1);
    }

    #[test]
    fn batched_trades_receive_distinct_canonical_sequences() {
        let mut decoder = CoinbaseDecoder::new();
        let trades = decoder
            .decode(batched_trade_message(0).as_bytes())
            .expect("batched trades decode");
        assert_eq!(
            trades
                .iter()
                .map(|trade| trade.canonical_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn projection_uses_the_catalog_compatible_product_identity() {
        let mut decoder = CoinbaseDecoder::new();
        let mut trade = decoder
            .decode(trade_message(1, "t-1").as_bytes())
            .expect("trade decodes")
            .pop()
            .expect("message contains one trade");
        trade.product_id = "SOL-USD".to_string();
        assert_eq!(
            trade
                .to_market_trade(2, 8, 1, 1)
                .expect("catalog product projects")
                .metadata
                .instrument_id,
            "instrument:coinbase:sol:usd"
        );
    }

    #[test]
    fn unknown_trade_sides_are_rejected_before_canonical_projection() {
        let mut decoder = CoinbaseDecoder::new();
        let message = trade_message(0, "t-1").replace("\"SELL\"", "\"UNKNOWN\"");
        assert!(matches!(
            decoder.decode(message.as_bytes()),
            Err(crate::CoinbaseError::InvalidMessage)
        ));
    }

    #[test]
    fn sequence_gap_is_recorded_without_tearing_down_the_public_stream() {
        let mut decoder = CoinbaseDecoder::new();
        decoder
            .decode(trade_message(0, "t-1").as_bytes())
            .expect("baseline accepted");
        assert_eq!(
            decoder
                .decode(trade_message(2, "t-2").as_bytes())
                .expect("later batch remains usable")
                .len(),
            1
        );
        assert_eq!(decoder.metrics().sequence_gaps, 1);
        assert!(
            decoder
                .decode(trade_message(1, "t-old").as_bytes())
                .expect("out-of-order batch is ignored")
                .is_empty()
        );
    }

    #[test]
    fn administrative_and_heartbeat_sequences_do_not_contaminate_trades() {
        let mut decoder = CoinbaseDecoder::new();
        decoder
            .decode(subscriptions_message(0).as_bytes())
            .expect("first acknowledgement accepted");
        decoder
            .decode(trade_message(0, "t-1").as_bytes())
            .expect("trade baseline accepted");
        decoder
            .decode(subscriptions_message(0).as_bytes())
            .expect("repeated acknowledgement sequence accepted");
        decoder
            .decode(heartbeat_message(47).as_bytes())
            .expect("independent heartbeat sequence accepted");
        decoder
            .decode(heartbeat_message(900).as_bytes())
            .expect("heartbeat counter jumps do not invalidate trades");
        decoder
            .decode(trade_message(1, "t-2").as_bytes())
            .expect("next trade accepted");
        assert_eq!(decoder.metrics().sequence_gaps, 0);
        assert_eq!(decoder.metrics().heartbeats, 2);
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
