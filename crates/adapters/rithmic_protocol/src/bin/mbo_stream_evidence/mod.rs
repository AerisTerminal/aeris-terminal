//! Sanitized stream evidence for `--mbo-only`.
//!
//! Only counts and clock differences leave this module. Prices, sizes and
//! exchange order ids are inspected for presence and never printed.

use aeris_rithmic_protocol_adapter::{
    DepthByOrderUpdate, JopTimestamp, ProviderTimestamp, QuoteSideUpdate, QuoteUpdate,
    SourceTimestamp, TradeUpdate,
};
use std::fmt;

#[derive(Default)]
pub(super) struct MboStreamEvidence {
    depth: DepthEvidence,
    trades: TradeEvidence,
    quotes: QuoteEvidence,
}

impl MboStreamEvidence {
    pub(super) fn record_depth(&mut self, update: &DepthByOrderUpdate) {
        self.depth.frames = self.depth.frames.saturating_add(1);
        self.depth.mutations = self.depth.mutations.saturating_add(update.mutations.len());
        self.depth.clocks.record(
            update.timestamp,
            update.source_timestamp,
            update.jop_timestamp,
        );
    }

    pub(super) fn record_trade(&mut self, trade: &TradeUpdate) {
        let trades = &mut self.trades;
        trades.frames = trades.frames.saturating_add(1);
        if trade.exchange_order_id.is_some() {
            trades.exchange_order_ids = trades.exchange_order_ids.saturating_add(1);
        }
        if trade.aggressor_exchange_order_id.is_some() {
            trades.aggressor_order_ids = trades.aggressor_order_ids.saturating_add(1);
        }
        trades.clocks.record(
            Some(trade.timestamp),
            trade.source_timestamp,
            trade.jop_timestamp,
        );
    }

    pub(super) fn record_quote(&mut self, quote: &QuoteUpdate) {
        let quotes = &mut self.quotes;
        quotes.frames = quotes.frames.saturating_add(1);
        quotes.bid.record(&quote.bid);
        quotes.ask.record(&quote.ask);
    }

    pub(super) const fn depth_frames(&self) -> usize {
        self.depth.frames
    }

    pub(super) const fn trade_frames(&self) -> usize {
        self.trades.frames
    }

    pub(super) const fn quote_frames(&self) -> usize {
        self.quotes.frames
    }

    pub(super) fn print(&self) {
        println!(
            "rithmic_mbo_depth_evidence frames={} mutations={} {}",
            self.depth.frames, self.depth.mutations, self.depth.clocks
        );
        println!(
            "rithmic_mbo_trade_evidence frames={} exchange_order_ids={} aggressor_order_ids={} {}",
            self.trades.frames,
            self.trades.exchange_order_ids,
            self.trades.aggressor_order_ids,
            self.trades.clocks
        );
        println!(
            "rithmic_mbo_quote_evidence frames={} bid_levels={} bid_implied_sizes={} ask_levels={} ask_implied_sizes={}",
            self.quotes.frames,
            self.quotes.bid.levels,
            self.quotes.bid.implied_sizes,
            self.quotes.ask.levels,
            self.quotes.ask.implied_sizes
        );
    }
}

#[derive(Default)]
struct DepthEvidence {
    frames: usize,
    mutations: usize,
    clocks: ClockEvidence,
}

#[derive(Default)]
struct TradeEvidence {
    frames: usize,
    exchange_order_ids: usize,
    aggressor_order_ids: usize,
    clocks: ClockEvidence,
}

#[derive(Default)]
struct QuoteEvidence {
    frames: usize,
    bid: QuoteSideEvidence,
    ask: QuoteSideEvidence,
}

#[derive(Default)]
struct QuoteSideEvidence {
    levels: usize,
    implied_sizes: usize,
}

impl QuoteSideEvidence {
    fn record(&mut self, side: &QuoteSideUpdate) {
        if let QuoteSideUpdate::Value(level) = side {
            self.levels = self.levels.saturating_add(1);
            if level.implied_size.is_some() {
                self.implied_sizes = self.implied_sizes.saturating_add(1);
            }
        }
    }
}

/// Presence of each Rithmic clock and how the exchange clock's parts relate,
/// so the meaning of `source_nsecs` can be read from live frames.
#[derive(Default)]
struct ClockEvidence {
    gateway: usize,
    source: usize,
    source_microseconds: usize,
    source_nanoseconds: usize,
    nanoseconds_whole_sub_second: usize,
    nanoseconds_match_microseconds: usize,
    nanoseconds_below_microsecond: usize,
    exchange_instants: usize,
    jop: usize,
    jop_nanoseconds: usize,
    gateway_minus_exchange_us: Span,
    jop_minus_exchange_ns: Span,
}

impl ClockEvidence {
    fn record(
        &mut self,
        gateway: Option<ProviderTimestamp>,
        source: Option<SourceTimestamp>,
        jop: Option<JopTimestamp>,
    ) {
        if gateway.is_some() {
            self.gateway = self.gateway.saturating_add(1);
        }
        let exchange = source.and_then(|source| self.record_source(source));
        if let (Some(gateway), Some(exchange)) = (gateway, exchange) {
            let gateway_us =
                i64::from(gateway.seconds) * 1_000_000 + i64::from(gateway.microseconds);
            self.gateway_minus_exchange_us
                .record(gateway_us - exchange.div_euclid(1_000));
        }
        let Some(jop) = jop else { return };
        self.jop = self.jop.saturating_add(1);
        let Some(nanoseconds) = jop.nanoseconds else {
            return;
        };
        self.jop_nanoseconds = self.jop_nanoseconds.saturating_add(1);
        if let Some(exchange) = exchange {
            let jop_ns = i64::from(jop.seconds) * 1_000_000_000 + i64::from(nanoseconds);
            self.jop_minus_exchange_ns.record(jop_ns - exchange);
        }
    }

    fn record_source(&mut self, source: SourceTimestamp) -> Option<i64> {
        self.source = self.source.saturating_add(1);
        if source.microseconds.is_some() {
            self.source_microseconds = self.source_microseconds.saturating_add(1);
        }
        if let Some(nanoseconds) = source.nanoseconds {
            self.source_nanoseconds = self.source_nanoseconds.saturating_add(1);
            if nanoseconds >= 1_000 {
                self.nanoseconds_whole_sub_second =
                    self.nanoseconds_whole_sub_second.saturating_add(1);
                if source.microseconds == Some(nanoseconds / 1_000) {
                    self.nanoseconds_match_microseconds =
                        self.nanoseconds_match_microseconds.saturating_add(1);
                }
            } else {
                self.nanoseconds_below_microsecond =
                    self.nanoseconds_below_microsecond.saturating_add(1);
            }
        }
        let exchange = source.unix_nanos();
        if exchange.is_some() {
            self.exchange_instants = self.exchange_instants.saturating_add(1);
        }
        exchange
    }
}

impl fmt::Display for ClockEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "gateway={} source={} source_usecs={} source_nsecs={} nsecs_ge_1000={} nsecs_ge_1000_match_usecs={} nsecs_lt_1000={} exchange_ns={} jop={} jop_nsecs={} gateway_minus_exchange_us={} jop_minus_exchange_ns={}",
            self.gateway,
            self.source,
            self.source_microseconds,
            self.source_nanoseconds,
            self.nanoseconds_whole_sub_second,
            self.nanoseconds_match_microseconds,
            self.nanoseconds_below_microsecond,
            self.exchange_instants,
            self.jop,
            self.jop_nanoseconds,
            self.gateway_minus_exchange_us,
            self.jop_minus_exchange_ns
        )
    }
}

#[derive(Clone, Copy, Default)]
struct Span(Option<(i64, i64)>);

impl Span {
    fn record(&mut self, value: i64) {
        self.0 = Some(match self.0 {
            Some((low, high)) => (low.min(value), high.max(value)),
            None => (value, value),
        });
    }
}

impl fmt::Display for Span {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some((low, high)) => write!(formatter, "{low}..{high}"),
            None => formatter.write_str("-"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClockEvidence;
    use aeris_rithmic_protocol_adapter::{JopTimestamp, ProviderTimestamp, SourceTimestamp};

    #[test]
    fn clock_evidence_reports_part_relations_and_differences_without_values() {
        let mut clocks = ClockEvidence::default();
        clocks.record(
            Some(ProviderTimestamp {
                seconds: 1_761_400_000,
                microseconds: 125_000,
            }),
            Some(SourceTimestamp {
                seconds: 1_761_400_000,
                microseconds: Some(123_456),
                nanoseconds: Some(123_456_789),
            }),
            Some(JopTimestamp {
                seconds: 1_761_400_000,
                nanoseconds: Some(123_457_000),
            }),
        );
        clocks.record(
            None,
            Some(SourceTimestamp {
                seconds: 1_761_400_001,
                microseconds: Some(7),
                nanoseconds: Some(89),
            }),
            None,
        );

        assert_eq!(
            clocks.to_string(),
            "gateway=1 source=2 source_usecs=2 source_nsecs=2 nsecs_ge_1000=1 nsecs_ge_1000_match_usecs=1 nsecs_lt_1000=1 exchange_ns=2 jop=1 jop_nsecs=1 gateway_minus_exchange_us=1544..1544 jop_minus_exchange_ns=211..211"
        );
    }
}
