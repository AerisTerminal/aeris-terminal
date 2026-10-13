use super::{
    DepthSnapshot, IndexedTradeMutation, InstallProviderInstrument, MarketBar, MarketTrade,
    TopOfBookQuote,
};
use crate::hyperliquid_realtime::HyperliquidRealtimeEvent;
use crate::rithmic_realtime::RithmicRealtimeEvent;
use aeris_contracts::MarketSessionStatus;
use aeris_market_engine::fence::{Epoch, EpochSequence, ProviderTag};
use std::collections::BTreeMap;

pub(super) struct ProviderEvent {
    pub(super) provider: &'static str,
    pub(super) generation: Epoch<ProviderTag>,
    pub(super) wire_generation: u64,
    pub(super) kind: ProviderEventKind,
}

/// Untrusted adapter identity. The coordinator registry stamps it before dispatch.
pub(super) struct WireProviderEvent {
    pub(super) provider: &'static str,
    pub(super) generation: u64,
    pub(super) kind: ProviderEventKind,
}

/// Owns the only mint for adapter generations. Never retains retired sessions.
#[derive(Default)]
pub(super) struct ProviderEpochRegistry {
    sequence: EpochSequence<ProviderTag>,
    active: BTreeMap<String, (u64, Epoch<ProviderTag>)>,
    stale_events: u64,
    stale_history: u64,
}

impl ProviderEpochRegistry {
    pub(super) fn stamp(&mut self, provider: &str, wire: u64) -> Option<Epoch<ProviderTag>> {
        if wire == 0
            || self
                .active
                .get(provider)
                .is_some_and(|(current, _)| wire < *current)
        {
            return None;
        }
        if let Some((current, epoch)) = self.active.get(provider)
            && *current == wire
        {
            return Some(*epoch);
        }
        let epoch = self.sequence.next();
        self.active.insert(provider.to_owned(), (wire, epoch));
        Some(epoch)
    }

    pub(super) fn accepts(&self, provider: &str, epoch: Epoch<ProviderTag>) -> bool {
        self.active
            .get(provider)
            .is_some_and(|(_, current)| current == &epoch)
    }

    pub(super) fn drop_event(&mut self) {
        self.stale_events = self.stale_events.saturating_add(1);
    }

    pub(super) fn drop_history(&mut self) {
        self.stale_history = self.stale_history.saturating_add(1);
    }

    #[cfg(test)]
    pub(super) fn stale_counts(&self) -> (u64, u64) {
        (self.stale_events, self.stale_history)
    }
}

pub(super) enum ProviderTradeBatch {
    One(MarketTrade),
    Many(Vec<MarketTrade>),
}

#[derive(Clone, Copy)]
pub(super) struct ProviderCandle {
    pub(super) bar: MarketBar,
    pub(super) trade_count: Option<u64>,
    pub(super) trade_watermark: Option<u64>,
}

#[derive(Clone, Copy)]
pub(super) enum ProviderDisconnect {
    Recover {
        detail: &'static str,
        auto_recover: bool,
    },
    Fail {
        detail: &'static str,
        auto_recover: bool,
    },
    End,
}

pub(super) enum ProviderEventKind {
    Connecting,
    Connected,
    Heartbeat(Option<u64>),
    Recovering {
        detail: &'static str,
        provider_detail: Option<String>,
    },
    Failed(String),
    Disconnected(ProviderDisconnect),
    Trades(ProviderTradeBatch),
    IndexedTrades {
        instrument: InstallProviderInstrument,
        changes: Vec<IndexedTradeMutation>,
    },
    TapeBackfill {
        instrument: InstallProviderInstrument,
        trades: Vec<MarketTrade>,
        truncated: bool,
    },
    Quote(TopOfBookQuote),
    Depth(DepthSnapshot),
    Candle {
        symbol: String,
        candle: ProviderCandle,
    },
    CandleRecovery(String),
    TradeRecovery(InstallProviderInstrument),
    Calendar {
        instrument_id: String,
        status: MarketSessionStatus,
        valid_from_utc_ms: i64,
        valid_until_utc_ms: i64,
    },
}

impl ProviderEvent {
    pub(super) fn rithmic(event: RithmicRealtimeEvent) -> WireProviderEvent {
        use RithmicRealtimeEvent as Wire;
        let generation = event.generation();
        let kind = match event {
            Wire::Connecting(_) => ProviderEventKind::Connecting,
            Wire::Connected(_) => ProviderEventKind::Connected,
            Wire::Heartbeat(_, rtt) => ProviderEventKind::Heartbeat(rtt),
            Wire::Failed(_, _error) => ProviderEventKind::Failed(
                "Rithmic reconnect could not start; check provider configuration".into(),
            ),
            Wire::Recovering(_, reason) => ProviderEventKind::Recovering {
                detail: super::realtime::rithmic_invalidation_detail(reason),
                provider_detail: None,
            },
            Wire::Disconnected(_, reason) => {
                let detail = super::realtime::rithmic_invalidation_detail(reason);
                let auto_recover = super::realtime::rithmic_auto_recovers(reason);
                let disposition = if reason.is_some() {
                    ProviderDisconnect::Fail {
                        detail,
                        auto_recover,
                    }
                } else {
                    ProviderDisconnect::Recover {
                        detail,
                        auto_recover,
                    }
                };
                ProviderEventKind::Disconnected(disposition)
            }
            Wire::Trade(_, trade) => ProviderEventKind::Trades(ProviderTradeBatch::One(trade)),
            Wire::Quote(_, quote) => ProviderEventKind::Quote(quote),
            Wire::Depth(_, depth) => ProviderEventKind::Depth(depth),
        };
        WireProviderEvent {
            provider: "rithmic",
            generation,
            kind,
        }
    }

    pub(super) fn hyperliquid(event: HyperliquidRealtimeEvent) -> WireProviderEvent {
        use HyperliquidRealtimeEvent as Wire;
        let generation = match &event {
            Wire::Connecting(g)
            | Wire::Connected(g)
            | Wire::Candle(g, ..)
            | Wire::Trades(g, _)
            | Wire::Quote(g, _)
            | Wire::Depth(g, _)
            | Wire::Heartbeat(g, _)
            | Wire::Recovering(g)
            | Wire::Disconnected(g) => *g,
        };
        let kind = match event {
            Wire::Connecting(_) => ProviderEventKind::Connecting,
            Wire::Connected(_) => ProviderEventKind::Connected,
            Wire::Heartbeat(_, rtt) => ProviderEventKind::Heartbeat(rtt),
            Wire::Recovering(_) => ProviderEventKind::Recovering {
                detail: "Hyperliquid live session is recovering",
                provider_detail: None,
            },
            Wire::Disconnected(_) => ProviderEventKind::Disconnected(ProviderDisconnect::Recover {
                detail: "Hyperliquid live session is recovering",
                auto_recover: false,
            }),
            Wire::Candle(_, coin, interval, candle) => ProviderEventKind::Candle {
                symbol: format!("{coin}{{={interval}}}"),
                candle: ProviderCandle {
                    bar: MarketBar {
                        source_sequence: 1,
                        exchange_timestamp_seconds: candle.open_nanos.div_euclid(1_000_000_000),
                        exchange_timestamp_unix_nanos: candle.open_nanos,
                        open: candle.open,
                        high: candle.high,
                        low: candle.low,
                        close: candle.close,
                        volume: candle.volume,
                    },
                    trade_count: None,
                    trade_watermark: None,
                },
            },
            Wire::Trades(_, trades) => ProviderEventKind::Trades(ProviderTradeBatch::Many(trades)),
            Wire::Quote(_, quote) => ProviderEventKind::Quote(quote),
            Wire::Depth(_, depth) => ProviderEventKind::Depth(depth),
        };
        WireProviderEvent {
            provider: "hyperliquid",
            generation,
            kind,
        }
    }

    pub(super) fn ctrader(event: super::ctrader::RealtimeEvent) -> WireProviderEvent {
        use super::ctrader::RealtimeEvent as Wire;
        let generation = event.generation();
        let kind = match event {
            Wire::Connecting(_) => ProviderEventKind::Connecting,
            Wire::Connected(_) => ProviderEventKind::Connected,
            Wire::Recovering(_, detail) => ProviderEventKind::Recovering {
                detail: "cTrader feed requires recovery",
                provider_detail: Some(detail),
            },
            Wire::Failed(_, detail) => ProviderEventKind::Failed(detail),
            Wire::Disconnected(_) => ProviderEventKind::Disconnected(ProviderDisconnect::End),
            Wire::Candle(_, symbol, bar) => ProviderEventKind::Candle {
                symbol,
                candle: ProviderCandle {
                    bar,
                    trade_count: None,
                    trade_watermark: None,
                },
            },
            Wire::Quote(_, quote) => ProviderEventKind::Quote(quote),
            Wire::Depth(_, depth) => ProviderEventKind::Depth(depth),
        };
        WireProviderEvent {
            provider: "ctrader",
            generation,
            kind,
        }
    }

    pub(super) fn tastytrade(event: super::tastytrade::RealtimeEvent) -> WireProviderEvent {
        use super::tastytrade::RealtimeEvent as Wire;
        let generation = event.generation();
        let kind = match event {
            Wire::Connecting(_) => ProviderEventKind::Connecting,
            Wire::Connected(_) => ProviderEventKind::Connected,
            Wire::Heartbeat(_, rtt) => ProviderEventKind::Heartbeat(Some(rtt)),
            Wire::Recovering(_, detail) => ProviderEventKind::Recovering {
                detail: "Tastytrade feed requires recovery",
                provider_detail: Some(detail),
            },
            Wire::Disconnected(_) => ProviderEventKind::Disconnected(ProviderDisconnect::End),
            Wire::Candle(_, symbol, bar, count, trade_watermark) => ProviderEventKind::Candle {
                symbol,
                candle: ProviderCandle {
                    bar,
                    trade_count: Some(count),
                    trade_watermark: Some(trade_watermark),
                },
            },
            Wire::CandleRecovery(_, symbol) => ProviderEventKind::CandleRecovery(symbol),
            Wire::TradeRecovery(_, instrument) => ProviderEventKind::TradeRecovery(instrument),
            Wire::Quote(_, quote) => ProviderEventKind::Quote(quote),
            Wire::Trades(_, instrument, changes) => ProviderEventKind::IndexedTrades {
                instrument,
                changes,
            },
            Wire::Tape(_, instrument, trades, truncated) => ProviderEventKind::TapeBackfill {
                instrument,
                trades,
                truncated,
            },
        };
        WireProviderEvent {
            provider: "tastytrade",
            generation,
            kind,
        }
    }
}
