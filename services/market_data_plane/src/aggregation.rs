//! Deterministic trade-to-bar aggregation per product.
//!
//! One-minute bars aggregate from canonical trades in arrival order: the first
//! trade of a minute opens the bar, the last closes it, and volume sums exact
//! mantissas. A completed bar is emitted only when a trade from a newer minute
//! arrives; the trailing in-flight bar stays un-emitted until then. History is
//! bounded and feeds client snapshots.

use crate::instruments::mantissa_at_scale;
use axiusflow_coinbase_market_adapter::CanonicalTrade;
use axiusflow_market_data::MarketBar;
use std::collections::VecDeque;

/// Maximum retained completed bars per product.
const MAXIMUM_HISTORY_BARS: usize = 1_024;

#[derive(Clone, Copy)]
struct InFlightBar {
    minute_unix_seconds: i64,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    volume: i64,
}

/// Per-product deterministic bar aggregator.
pub struct BarAggregator {
    in_flight: Option<InFlightBar>,
    history: VecDeque<MarketBar>,
    next_sequence: u64,
    late_trades: u64,
}

impl BarAggregator {
    /// Creates an empty aggregator; the first bar gets sequence 1.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            in_flight: None,
            history: VecDeque::new(),
            next_sequence: 1,
            late_trades: 0,
        }
    }

    /// Seeds completed history from backfill, assigning contiguous sequences.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bars or sequence overflow.
    pub fn seed_backfill(&mut self, mut bars: Vec<MarketBar>) -> Result<usize, String> {
        if bars.len() > MAXIMUM_HISTORY_BARS {
            let excess = bars.len() - MAXIMUM_HISTORY_BARS;
            bars.drain(..excess);
        }
        // The newest backfilled candle is the still-open minute: live trades
        // must merge into it rather than complete a duplicate bar.
        let in_flight_seed = bars.pop();
        for bar in &mut bars {
            bar.source_sequence = self.next_sequence;
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .ok_or_else(|| "bar sequence overflow".to_string())?;
            bar.validate().map_err(|error| format!("{error:?}"))?;
            self.history.push_back(*bar);
        }
        if let Some(seed) = in_flight_seed {
            self.in_flight = Some(InFlightBar {
                minute_unix_seconds: seed.exchange_timestamp_seconds,
                open: seed.open,
                high: seed.high,
                low: seed.low,
                close: seed.close,
                volume: seed.volume,
            });
        }
        Ok(bars.len())
    }

    /// Applies one canonical trade, emitting a completed bar on minute roll.
    ///
    /// # Errors
    ///
    /// Returns an error for fixed-point conversion or sequence failures.
    pub fn apply_trade(&mut self, trade: &CanonicalTrade) -> Result<Option<MarketBar>, String> {
        let price = mantissa_at_scale(trade.price, 2)?;
        let size = mantissa_at_scale(trade.size, 8)?;
        let minute = trade.trade_time_unix_nanos.div_euclid(60_000_000_000) * 60;
        let mut completed = None;
        match &mut self.in_flight {
            Some(bar) if bar.minute_unix_seconds == minute => {
                bar.high = bar.high.max(price);
                bar.low = bar.low.min(price);
                bar.close = price;
                bar.volume = bar
                    .volume
                    .checked_add(size)
                    .ok_or_else(|| "bar volume overflow".to_string())?;
            }
            Some(_) => {
                let finished = self.in_flight.take().expect("in-flight bar exists");
                if minute < finished.minute_unix_seconds {
                    self.late_trades += 1;
                    self.in_flight = Some(finished);
                    return Ok(None);
                }
                completed = Some(self.complete(finished)?);
                self.open_bar(minute, price, size);
            }
            None => self.open_bar(minute, price, size),
        }
        Ok(completed)
    }

    /// Retained completed bars in sequence order.
    #[must_use]
    pub fn history(&self) -> Vec<MarketBar> {
        self.history.iter().copied().collect()
    }

    /// Count of trades dropped for arriving behind the in-flight minute.
    #[allow(dead_code)]
    #[must_use]
    pub const fn late_trades(&self) -> u64 {
        self.late_trades
    }

    /// The in-flight minute's bar, when one exists, without emitting it.
    #[allow(dead_code)]
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

    fn open_bar(&mut self, minute: i64, price: i64, size: i64) {
        self.in_flight = Some(InFlightBar {
            minute_unix_seconds: minute,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: size,
        });
    }

    fn complete(&mut self, bar: InFlightBar) -> Result<MarketBar, String> {
        let completed = MarketBar {
            source_sequence: self.next_sequence,
            exchange_timestamp_seconds: bar.minute_unix_seconds,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        };
        completed.validate().map_err(|error| format!("{error:?}"))?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| "bar sequence overflow".to_string())?;
        self.history.push_back(completed);
        if self.history.len() > MAXIMUM_HISTORY_BARS {
            self.history.pop_front();
        }
        Ok(completed)
    }
}

impl Default for BarAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::BarAggregator;
    use axiusflow_coinbase_market_adapter::{CanonicalTrade, FixedPointValue};

    fn trade(minute: i64, price: &str, size: &str) -> CanonicalTrade {
        CanonicalTrade {
            product_id: "BTC-USD".to_string(),
            trade_id: format!("t-{minute}-{price}"),
            price: FixedPointValue::parse(price).expect("valid"),
            size: FixedPointValue::parse(size).expect("valid"),
            maker_side_buy: true,
            trade_time_unix_nanos: minute * 3_600_000_000_000 + 1_000_000_000,
            provider_timestamp_unix_nanos: 0,
            sequence_num: 0,
        }
    }

    #[test]
    fn aggregation_emits_only_on_minute_roll() {
        let mut aggregator = BarAggregator::new();
        assert!(
            aggregator
                .apply_trade(&trade(100, "100.00", "0.5"))
                .expect("trade")
                .is_none()
        );
        assert!(
            aggregator
                .apply_trade(&trade(100, "101.50", "0.5"))
                .expect("trade")
                .is_none()
        );
        let completed = aggregator
            .apply_trade(&trade(101, "102.00", "1.0"))
            .expect("trade")
            .expect("roll emits the finished bar");
        assert_eq!(completed.open, 10_000);
        assert_eq!(completed.high, 10_150);
        assert_eq!(completed.low, 10_000);
        assert_eq!(completed.close, 10_150);
        assert_eq!(completed.volume, 100_000_000);
        assert_eq!(completed.source_sequence, 1);
    }

    #[test]
    fn late_trades_do_not_rewrite_history() {
        let mut aggregator = BarAggregator::new();
        aggregator
            .apply_trade(&trade(101, "100.00", "0.5"))
            .expect("trade");
        aggregator
            .apply_trade(&trade(100, "99.00", "0.5"))
            .expect("trade");
        assert_eq!(aggregator.late_trades(), 1);
    }
}
