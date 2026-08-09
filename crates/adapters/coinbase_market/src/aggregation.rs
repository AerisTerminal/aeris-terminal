//! Bounded deterministic one-minute aggregation for Coinbase spot trades.

use crate::{CanonicalTrade, FixedPointValue};
use axiusflow_market_data::MarketBar;
use core::fmt;
use std::{collections::VecDeque, error::Error, num::NonZeroUsize};

const ONE_MINUTE_NANOS: i64 = 60_000_000_000;
const ONE_MINUTE_SECONDS: i64 = 60;
const MAXIMUM_DECIMAL_SCALE: u8 = 18;

/// Hard ceiling for retained completed bars in one product aggregator.
pub const MAXIMUM_AGGREGATED_HISTORY_BARS: usize = 4_096;

#[derive(Clone, Copy)]
struct InFlightBar {
    minute_unix_seconds: i64,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    volume: i64,
    provider_timestamp_unix_nanos: Option<i64>,
    provider_sequence_num: Option<u64>,
}

/// One completed bar plus the final provider message evidence that formed it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoinbaseAggregatedBar {
    pub bar: MarketBar,
    pub provider_timestamp_unix_nanos: Option<i64>,
    pub provider_sequence_num: Option<u64>,
}

/// Explicit identity, precision, and retention bounds for one product aggregator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoinbaseBarAggregatorConfig {
    product_id: String,
    instrument_id: String,
    price_scale: u8,
    quantity_scale: u8,
    maximum_history_bars: NonZeroUsize,
}

impl CoinbaseBarAggregatorConfig {
    /// Creates a validated Coinbase spot aggregation profile.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed product identity or unsupported scales.
    pub fn try_new(
        product_id: impl Into<String>,
        price_scale: u8,
        quantity_scale: u8,
        maximum_history_bars: NonZeroUsize,
    ) -> Result<Self, CoinbaseBarAggregationError> {
        let product_id = product_id.into();
        let Some((base, quote)) = product_id.split_once('-') else {
            return Err(CoinbaseBarAggregationError::InvalidConfiguration);
        };
        if base.is_empty()
            || quote.is_empty()
            || product_id.len() > 32
            || !product_id.chars().all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '-'
            })
            || price_scale > MAXIMUM_DECIMAL_SCALE
            || quantity_scale > MAXIMUM_DECIMAL_SCALE
            || maximum_history_bars.get() > MAXIMUM_AGGREGATED_HISTORY_BARS
        {
            return Err(CoinbaseBarAggregationError::InvalidConfiguration);
        }
        Ok(Self {
            instrument_id: crate::coinbase_instrument_id(&product_id)
                .map_err(|_| CoinbaseBarAggregationError::InvalidConfiguration)?,
            product_id,
            price_scale,
            quantity_scale,
            maximum_history_bars,
        })
    }

    /// Coinbase product identity accepted by this aggregator.
    #[must_use]
    pub fn product_id(&self) -> &str {
        &self.product_id
    }

    /// Provider-neutral instrument identity for matching retained history.
    #[must_use]
    pub fn instrument_id(&self) -> &str {
        &self.instrument_id
    }

    /// Decimal scale used for price mantissas.
    #[must_use]
    pub const fn price_scale(&self) -> u8 {
        self.price_scale
    }

    /// Decimal scale used for quantity and volume mantissas.
    #[must_use]
    pub const fn quantity_scale(&self) -> u8 {
        self.quantity_scale
    }
}

/// Deterministic, bounded one-minute bar aggregator for one Coinbase product.
pub struct CoinbaseBarAggregator {
    config: CoinbaseBarAggregatorConfig,
    in_flight: Option<InFlightBar>,
    history: VecDeque<MarketBar>,
    next_sequence: u64,
    late_trades: u64,
    activity_started: bool,
}

impl CoinbaseBarAggregator {
    /// Creates an empty product aggregator; the first completed bar gets sequence 1.
    #[must_use]
    pub fn new(config: CoinbaseBarAggregatorConfig) -> Self {
        Self {
            history: VecDeque::with_capacity(config.maximum_history_bars.get()),
            config,
            in_flight: None,
            next_sequence: 1,
            late_trades: 0,
            activity_started: false,
        }
    }

    /// Product identity accepted by this aggregator.
    #[must_use]
    pub fn product_id(&self) -> &str {
        self.config.product_id()
    }

    /// Provider-neutral instrument identity required for history seeding.
    #[must_use]
    pub fn instrument_id(&self) -> &str {
        self.config.instrument_id()
    }

    /// Decimal scale used for price mantissas.
    #[must_use]
    pub const fn price_scale(&self) -> u8 {
        self.config.price_scale()
    }

    /// Decimal scale used for quantity and volume mantissas.
    #[must_use]
    pub const fn quantity_scale(&self) -> u8 {
        self.config.quantity_scale()
    }

    /// Clears live and seeded state while retaining identity and memory bounds.
    pub fn reset(&mut self) {
        self.in_flight = None;
        self.history.clear();
        self.next_sequence = 1;
        self.late_trades = 0;
        self.activity_started = false;
    }

    /// Atomically seeds completed history and the newest still-open minute.
    ///
    /// Source sequences are reassigned contiguously for this local bar stream.
    /// Seeding is permitted exactly once and must precede every live trade.
    ///
    /// # Errors
    ///
    /// Returns an error for repeated seeding, invalid or unordered bars, or
    /// sequence exhaustion.
    pub fn seed_backfill(
        &mut self,
        bars: &[MarketBar],
    ) -> Result<usize, CoinbaseBarAggregationError> {
        self.seed(bars, true)
    }

    /// Atomically seeds completed history before a live current minute.
    ///
    /// # Errors
    ///
    /// Returns an error for repeated seeding, invalid or unordered bars, or
    /// sequence exhaustion.
    pub fn seed_completed_history(
        &mut self,
        bars: &[MarketBar],
    ) -> Result<usize, CoinbaseBarAggregationError> {
        self.seed(bars, false)
    }

    fn seed(
        &mut self,
        bars: &[MarketBar],
        newest_is_in_flight: bool,
    ) -> Result<usize, CoinbaseBarAggregationError> {
        if self.activity_started {
            return Err(CoinbaseBarAggregationError::AlreadyInitialized);
        }
        let mut previous_timestamp = None;
        for bar in bars {
            let mut validated = *bar;
            validated.source_sequence = 1;
            validated
                .validate()
                .map_err(|_| CoinbaseBarAggregationError::InvalidBar)?;
            if previous_timestamp.is_some_and(|previous| {
                bar.exchange_timestamp_seconds <= previous
                    || bar.exchange_timestamp_seconds % ONE_MINUTE_SECONDS != 0
            }) || previous_timestamp.is_none()
                && bar.exchange_timestamp_seconds % ONE_MINUTE_SECONDS != 0
            {
                return Err(CoinbaseBarAggregationError::InvalidBar);
            }
            previous_timestamp = Some(bar.exchange_timestamp_seconds);
        }
        let retained_start = bars
            .len()
            .saturating_sub(self.config.maximum_history_bars.get());
        let retained = &bars[retained_start..];
        let mut history = VecDeque::with_capacity(self.config.maximum_history_bars.get());
        let mut next_sequence = 1_u64;
        let completed_count = if newest_is_in_flight {
            retained.len().saturating_sub(1)
        } else {
            retained.len()
        };
        for source in retained.iter().take(completed_count) {
            let mut bar = *source;
            bar.source_sequence = next_sequence;
            next_sequence = next_sequence
                .checked_add(1)
                .ok_or(CoinbaseBarAggregationError::SequenceOverflow)?;
            bar.validate()
                .map_err(|_| CoinbaseBarAggregationError::InvalidBar)?;
            history.push_back(bar);
        }
        let in_flight = newest_is_in_flight
            .then(|| {
                retained.last().map(|bar| InFlightBar {
                    minute_unix_seconds: bar.exchange_timestamp_seconds,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    volume: bar.volume,
                    provider_timestamp_unix_nanos: None,
                    provider_sequence_num: None,
                })
            })
            .flatten();
        let seeded = history.len();
        self.history = history;
        self.in_flight = in_flight;
        self.next_sequence = next_sequence;
        self.late_trades = 0;
        self.activity_started = true;
        Ok(seeded)
    }

    /// Applies one canonical trade and emits the completed bar on minute roll.
    ///
    /// # Errors
    ///
    /// Returns an error for product mismatch, precision loss, numeric overflow,
    /// invalid bar state, or sequence exhaustion.
    pub fn apply_trade(
        &mut self,
        trade: &CanonicalTrade,
    ) -> Result<Option<MarketBar>, CoinbaseBarAggregationError> {
        self.apply_trade_with_evidence(trade)
            .map(|completed| completed.map(|completed| completed.bar))
    }

    /// Applies one canonical trade and retains the final provider timestamp and sequence.
    ///
    /// # Errors
    ///
    /// Returns the same bounded validation failures as [`Self::apply_trade`].
    pub fn apply_trade_with_evidence(
        &mut self,
        trade: &CanonicalTrade,
    ) -> Result<Option<CoinbaseAggregatedBar>, CoinbaseBarAggregationError> {
        if trade.product_id != self.config.product_id {
            return Err(CoinbaseBarAggregationError::ProductMismatch);
        }
        let price = mantissa_at_scale(trade.price, self.config.price_scale)?;
        let size = mantissa_at_scale(trade.size, self.config.quantity_scale)?;
        if price <= 0 || size <= 0 {
            return Err(CoinbaseBarAggregationError::InvalidBar);
        }
        let minute = trade.trade_time_unix_nanos.div_euclid(ONE_MINUTE_NANOS) * ONE_MINUTE_SECONDS;
        self.activity_started = true;
        let mut completed = None;
        match &mut self.in_flight {
            Some(bar) if bar.minute_unix_seconds == minute => {
                bar.high = bar.high.max(price);
                bar.low = bar.low.min(price);
                bar.close = price;
                bar.volume = bar
                    .volume
                    .checked_add(size)
                    .ok_or(CoinbaseBarAggregationError::NumericOverflow)?;
                bar.provider_timestamp_unix_nanos = Some(trade.provider_timestamp_unix_nanos);
                bar.provider_sequence_num = Some(trade.sequence_num);
            }
            Some(_) => {
                let finished = self
                    .in_flight
                    .take()
                    .ok_or(CoinbaseBarAggregationError::InvalidBar)?;
                if minute < finished.minute_unix_seconds {
                    self.late_trades = self.late_trades.saturating_add(1);
                    self.in_flight = Some(finished);
                    return Ok(None);
                }
                completed = Some(self.complete(finished)?);
                self.open_bar(minute, price, size, trade);
            }
            None => {
                if self
                    .history
                    .back()
                    .is_some_and(|bar| minute <= bar.exchange_timestamp_seconds)
                {
                    self.late_trades = self.late_trades.saturating_add(1);
                    return Ok(None);
                }
                self.open_bar(minute, price, size, trade);
            }
        }
        Ok(completed)
    }

    /// Retained completed bars in local sequence order.
    #[must_use]
    pub fn history(&self) -> Vec<MarketBar> {
        self.history.iter().copied().collect()
    }

    /// Count of late trades ignored without rewriting completed or in-flight bars.
    #[must_use]
    pub const fn late_trades(&self) -> u64 {
        self.late_trades
    }

    /// The in-flight minute without completing it.
    #[must_use]
    pub fn in_flight(&self) -> Option<MarketBar> {
        self.in_flight.as_ref().map(|bar| MarketBar {
            source_sequence: self.next_sequence,
            exchange_timestamp_seconds: bar.minute_unix_seconds,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        })
    }

    fn open_bar(&mut self, minute: i64, price: i64, size: i64, trade: &CanonicalTrade) {
        self.in_flight = Some(InFlightBar {
            minute_unix_seconds: minute,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: size,
            provider_timestamp_unix_nanos: Some(trade.provider_timestamp_unix_nanos),
            provider_sequence_num: Some(trade.sequence_num),
        });
    }

    fn complete(
        &mut self,
        bar: InFlightBar,
    ) -> Result<CoinbaseAggregatedBar, CoinbaseBarAggregationError> {
        let completed = MarketBar {
            source_sequence: self.next_sequence,
            exchange_timestamp_seconds: bar.minute_unix_seconds,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        };
        completed
            .validate()
            .map_err(|_| CoinbaseBarAggregationError::InvalidBar)?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(CoinbaseBarAggregationError::SequenceOverflow)?;
        self.history.push_back(completed);
        if self.history.len() > self.config.maximum_history_bars.get() {
            self.history.pop_front();
        }
        Ok(CoinbaseAggregatedBar {
            bar: completed,
            provider_timestamp_unix_nanos: bar.provider_timestamp_unix_nanos,
            provider_sequence_num: bar.provider_sequence_num,
        })
    }
}

/// Converts an exact provider decimal to an instrument-scale mantissa.
///
/// # Errors
///
/// Returns an error when conversion loses precision or overflows.
pub fn mantissa_at_scale(
    value: FixedPointValue,
    target_scale: u8,
) -> Result<i64, CoinbaseBarAggregationError> {
    if target_scale > MAXIMUM_DECIMAL_SCALE {
        return Err(CoinbaseBarAggregationError::InvalidConfiguration);
    }
    let source = i128::from(value.mantissa);
    let scaled = if value.scale <= u32::from(target_scale) {
        let shift = u32::from(target_scale) - value.scale;
        source
            .checked_mul(
                10_i128
                    .checked_pow(shift)
                    .ok_or(CoinbaseBarAggregationError::NumericOverflow)?,
            )
            .ok_or(CoinbaseBarAggregationError::NumericOverflow)?
    } else {
        let shift = value.scale - u32::from(target_scale);
        let divisor = 10_i128
            .checked_pow(shift)
            .ok_or(CoinbaseBarAggregationError::NumericOverflow)?;
        if source % divisor != 0 {
            return Err(CoinbaseBarAggregationError::PrecisionLoss);
        }
        source / divisor
    };
    i64::try_from(scaled).map_err(|_| CoinbaseBarAggregationError::NumericOverflow)
}

/// Coarse deterministic aggregation failures without provider payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoinbaseBarAggregationError {
    InvalidConfiguration,
    AlreadyInitialized,
    ProductMismatch,
    PrecisionLoss,
    NumericOverflow,
    SequenceOverflow,
    InvalidBar,
}

impl fmt::Display for CoinbaseBarAggregationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Coinbase bar aggregation failed: {self:?}")
    }
}

impl Error for CoinbaseBarAggregationError {}

#[cfg(test)]
mod tests {
    use super::{
        CoinbaseBarAggregationError, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig,
        ONE_MINUTE_NANOS, ONE_MINUTE_SECONDS,
    };
    use crate::{CanonicalTrade, FixedPointValue};
    use axiusflow_market_data::MarketBar;
    use std::num::NonZeroUsize;

    fn aggregator() -> CoinbaseBarAggregator {
        CoinbaseBarAggregator::new(
            CoinbaseBarAggregatorConfig::try_new(
                "BTC-USD",
                2,
                8,
                NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
            )
            .expect("aggregation config validates"),
        )
    }

    fn trade(minute: i64, price: &str, size: &str) -> CanonicalTrade {
        CanonicalTrade {
            product_id: "BTC-USD".to_string(),
            trade_id: format!("t-{minute}-{price}"),
            price: FixedPointValue::parse(price).expect("price parses"),
            size: FixedPointValue::parse(size).expect("size parses"),
            maker_side_buy: true,
            trade_time_unix_nanos: minute * ONE_MINUTE_NANOS + 1_000_000_000,
            provider_timestamp_unix_nanos: 0,
            sequence_num: 0,
            canonical_sequence: 1,
        }
    }

    fn bar(minute: i64, price: i64) -> MarketBar {
        MarketBar {
            source_sequence: u64::try_from(minute + 1).expect("positive fixture sequence"),
            exchange_timestamp_seconds: minute * ONE_MINUTE_SECONDS,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 100_000_000,
        }
    }

    #[test]
    fn aggregation_emits_exact_bar_only_on_minute_roll() {
        let mut aggregator = aggregator();
        assert!(
            aggregator
                .apply_trade(&trade(100, "100.00", "0.5"))
                .expect("first trade applies")
                .is_none()
        );
        aggregator
            .apply_trade(&trade(100, "101.50", "0.5"))
            .expect("same-minute trade applies");
        let completed = aggregator
            .apply_trade(&trade(101, "102.00", "1.0"))
            .expect("new-minute trade applies")
            .expect("minute roll completes the prior bar");
        assert_eq!(completed.open, 10_000);
        assert_eq!(completed.high, 10_150);
        assert_eq!(completed.close, 10_150);
        assert_eq!(completed.volume, 100_000_000);
        assert_eq!(completed.source_sequence, 1);
    }

    #[test]
    fn backfill_seeds_completed_history_and_merges_the_open_minute() {
        let mut aggregator = aggregator();
        assert_eq!(
            aggregator
                .seed_backfill(&[bar(100, 10_000), bar(101, 10_100)])
                .expect("backfill seeds"),
            1
        );
        aggregator
            .apply_trade(&trade(101, "102.00", "0.5"))
            .expect("live trade merges open minute");
        let completed = aggregator
            .apply_trade(&trade(102, "103.00", "0.5"))
            .expect("minute rolls")
            .expect("seeded open minute completes");
        assert_eq!(completed.source_sequence, 2);
        assert_eq!(completed.open, 10_100);
        assert_eq!(completed.high, 10_200);
        assert_eq!(completed.close, 10_200);
        assert_eq!(completed.volume, 150_000_000);
    }

    #[test]
    fn seeded_in_flight_bar_reports_missing_live_provider_evidence() {
        let mut aggregator = aggregator();
        aggregator
            .seed_backfill(&[bar(100, 10_000), bar(101, 10_100)])
            .expect("backfill seeds");
        let completed = aggregator
            .apply_trade_with_evidence(&trade(102, "103.00", "0.5"))
            .expect("minute rolls")
            .expect("seeded open minute completes");
        assert_eq!(completed.provider_timestamp_unix_nanos, None);
        assert_eq!(completed.provider_sequence_num, None);
    }

    #[test]
    fn completed_history_opens_only_the_following_live_minute() {
        let mut aggregator = aggregator();
        assert_eq!(
            aggregator
                .seed_completed_history(&[bar(100, 10_000), bar(101, 10_100)])
                .expect("completed history seeds"),
            2
        );
        assert!(
            aggregator
                .apply_trade(&trade(101, "102.00", "0.5"))
                .expect("overlapping trade is ignored")
                .is_none()
        );
        aggregator
            .apply_trade(&trade(102, "103.00", "0.5"))
            .expect("next live minute opens");
        let completed = aggregator
            .apply_trade(&trade(103, "104.00", "0.5"))
            .expect("minute rolls")
            .expect("live minute completes");
        assert_eq!(completed.source_sequence, 3);
        assert_eq!(
            completed.exchange_timestamp_seconds,
            102 * ONE_MINUTE_SECONDS
        );
    }

    #[test]
    fn late_mismatched_and_inexact_trades_fail_closed() {
        let mut aggregator = aggregator();
        aggregator
            .apply_trade(&trade(101, "100.00", "0.5"))
            .expect("first trade applies");
        assert!(
            aggregator
                .apply_trade(&trade(100, "99.00", "0.5"))
                .expect("late trade is counted")
                .is_none()
        );
        assert_eq!(aggregator.late_trades(), 1);
        let mut wrong_product = trade(102, "100.00", "0.5");
        wrong_product.product_id = "ETH-USD".to_string();
        assert_eq!(
            aggregator.apply_trade(&wrong_product),
            Err(CoinbaseBarAggregationError::ProductMismatch)
        );
        assert_eq!(
            aggregator.apply_trade(&trade(102, "100.001", "0.5")),
            Err(CoinbaseBarAggregationError::PrecisionLoss)
        );
    }
}
