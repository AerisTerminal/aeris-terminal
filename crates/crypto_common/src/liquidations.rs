//! Bounded in-memory liquidation event ledger and bar-bucketing helpers shared across
//! cryptocurrency exchange adapters.

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::RwLock,
};

use aeris_market_data::BarPeriod;

const NANOS_PER_MILLI: i64 = 1_000_000;
const MAXIMUM_TRACKED_SYMBOLS: usize = 64;
const MAXIMUM_EVENTS_PER_SYMBOL: usize = 4_096;

/// One normalized forced liquidation fill observed on a perpetual venue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidationEvent {
    pub timestamp_ms: i64,
    /// `true` when a long position was liquidated (forced sell); `false` when a short position
    /// was liquidated (forced buy).
    pub is_long: bool,
    /// Liquidated base-asset quantity (non-negative).
    pub contracts: f64,
    /// Liquidated USD notional value (`contracts * price`, non-negative).
    pub usd: f64,
}

impl LiquidationEvent {
    /// Deterministic deduplication key rounded to micro-units so minor string-format differences
    /// between REST backfill and WebSocket frames do not double-count the same liquidation.
    #[must_use]
    pub fn dedup_key(&self) -> (i64, bool, u64, u64) {
        let contracts_key = ((self.contracts.max(0.0) * 1_000_000.0).round() + 0.0).to_bits();
        let usd_key = ((self.usd.max(0.0) * 100.0).round() + 0.0).to_bits();
        (self.timestamp_ms, self.is_long, contracts_key, usd_key)
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.timestamp_ms > 0
            && self.contracts.is_finite()
            && self.contracts >= 0.0
            && self.usd.is_finite()
            && self.usd >= 0.0
            && (self.contracts > 0.0 || self.usd > 0.0)
    }
}

/// One bar-aligned liquidation aggregate produced by bucketing [`LiquidationEvent`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiquidationBucket {
    pub timestamp_unix_nanos: i64,
    pub long_usd: f64,
    pub short_usd: f64,
    pub long_contracts: f64,
    pub short_contracts: f64,
}

#[derive(Default)]
struct SymbolBuffer {
    last_touch: u64,
    events: VecDeque<LiquidationEvent>,
    seen: HashSet<(i64, bool, u64, u64)>,
}

impl SymbolBuffer {
    fn push(&mut self, event: LiquidationEvent, touch: u64) {
        if !event.is_valid() {
            return;
        }
        self.last_touch = touch;
        let key = event.dedup_key();
        if !self.seen.insert(key) {
            return;
        }
        if self.events.len() >= MAXIMUM_EVENTS_PER_SYMBOL
            && let Some(evicted) = self.events.pop_front()
        {
            self.seen.remove(&evicted.dedup_key());
        }
        self.events.push_back(event);
    }
}

#[derive(Default)]
struct LedgerInner {
    touch_counter: u64,
    by_symbol: BTreeMap<String, SymbolBuffer>,
}

/// Thread-safe, strictly bounded per-symbol liquidation event store.
#[derive(Default)]
pub struct LiquidationLedger {
    inner: RwLock<LedgerInner>,
}

impl LiquidationLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one liquidation event under `symbol` (case-insensitive, stored uppercase).
    pub fn record(&self, symbol: &str, event: LiquidationEvent) {
        if !event.is_valid() {
            return;
        }
        let key = symbol.to_ascii_uppercase();
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.touch_counter = inner.touch_counter.saturating_add(1);
        let touch = inner.touch_counter;
        if !inner.by_symbol.contains_key(&key)
            && inner.by_symbol.len() >= MAXIMUM_TRACKED_SYMBOLS
            && let Some(oldest_symbol) = inner
                .by_symbol
                .iter()
                .min_by_key(|(_, buf)| buf.last_touch)
                .map(|(sym, _)| sym.clone())
        {
            inner.by_symbol.remove(&oldest_symbol);
        }
        inner.by_symbol.entry(key).or_default().push(event, touch);
    }

    /// Returns a snapshot of all retained raw liquidation events for `symbol`.
    #[must_use]
    pub fn events_for(&self, symbol: &str) -> Vec<LiquidationEvent> {
        let key = symbol.to_ascii_uppercase();
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .by_symbol
            .get(&key)
            .map(|buf| buf.events.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Buckets all retained liquidation events for `symbol` into bar-aligned aggregates.
    #[must_use]
    pub fn snapshot_buckets(
        &self,
        symbol: &str,
        period: BarPeriod,
        maximum_points: usize,
    ) -> Vec<LiquidationBucket> {
        let events = self.events_for(symbol);
        bucket_liquidation_events(&events, period, maximum_points)
    }
}

/// Deduplicates and buckets raw liquidation events into sorted bar-aligned totals.
#[must_use]
pub fn bucket_liquidation_events(
    events: &[LiquidationEvent],
    period: BarPeriod,
    maximum_points: usize,
) -> Vec<LiquidationBucket> {
    if events.is_empty() || maximum_points == 0 {
        return Vec::new();
    }
    let bucket_nanos = period
        .duration_nanos()
        .filter(|&nanos| nanos > 0)
        .unwrap_or(86_400 * 1_000_000_000);
    let mut seen = HashSet::with_capacity(events.len());
    let mut by_bucket: BTreeMap<i64, LiquidationBucket> = BTreeMap::new();
    for &event in events {
        if !event.is_valid() || !seen.insert(event.dedup_key()) {
            continue;
        }
        let Some(ts_nanos) = event.timestamp_ms.checked_mul(NANOS_PER_MILLI) else {
            continue;
        };
        let bucket_ts = ts_nanos.div_euclid(bucket_nanos) * bucket_nanos;
        let entry = by_bucket
            .entry(bucket_ts)
            .or_insert_with(|| LiquidationBucket {
                timestamp_unix_nanos: bucket_ts,
                ..LiquidationBucket::default()
            });
        if event.is_long {
            entry.long_usd += event.usd;
            entry.long_contracts += event.contracts;
        } else {
            entry.short_usd += event.usd;
            entry.short_contracts += event.contracts;
        }
    }
    let mut out: Vec<LiquidationBucket> = by_bucket.into_values().collect();
    if out.len() > maximum_points {
        let keep_from = out.len() - maximum_points;
        out.drain(..keep_from);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_deduplicates_and_buckets_long_and_short_liquidations() {
        let ledger = LiquidationLedger::new();
        let long_evt = LiquidationEvent {
            timestamp_ms: 1_700_000_010_000,
            is_long: true,
            contracts: 1.5,
            usd: 90_000.0,
        };
        let short_evt = LiquidationEvent {
            timestamp_ms: 1_700_000_025_000,
            is_long: false,
            contracts: 0.5,
            usd: 30_000.0,
        };
        ledger.record("btcusdt", long_evt);
        ledger.record("BTCUSDT", long_evt); // duplicate ignored
        ledger.record("BTCUSDT", short_evt);

        let buckets = ledger.snapshot_buckets("BTCUSDT", BarPeriod::Time { seconds: 60 }, 100);
        assert_eq!(buckets.len(), 1);
        assert!((buckets[0].long_usd - 90_000.0).abs() < 1e-6);
        assert!((buckets[0].short_usd - 30_000.0).abs() < 1e-6);
        assert!((buckets[0].long_contracts - 1.5).abs() < 1e-6);
        assert!((buckets[0].short_contracts - 0.5).abs() < 1e-6);
    }
}
