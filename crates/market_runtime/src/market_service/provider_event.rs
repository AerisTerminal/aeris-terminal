use super::{
    DepthSnapshot, IndexedTradeMutation, InstallProviderInstrument, MarketBar, MarketTrade,
    TopOfBookQuote,
};
use crate::hyperliquid_realtime::HyperliquidRealtimeEvent;
use crate::rithmic_realtime::RithmicRealtimeEvent;

pub(super) struct ProviderEvent {
    pub(super) provider: &'static str,
    pub(super) generation: u64,
    pub(super) kind: ProviderEventKind,
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
}

impl ProviderEvent {
    pub(super) fn rithmic(event: RithmicRealtimeEvent) -> Self {
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
        Self {
            provider: "rithmic",
            generation,
            kind,
        }
    }

    pub(super) fn hyperliquid(event: HyperliquidRealtimeEvent) -> Self {
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
        Self {
            provider: "hyperliquid",
            generation,
            kind,
        }
    }

    pub(super) fn ctrader(event: super::ctrader::RealtimeEvent) -> Self {
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
        Self {
            provider: "ctrader",
            generation,
            kind,
        }
    }

    pub(super) fn tastytrade(event: super::tastytrade::RealtimeEvent) -> Self {
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
        Self {
            provider: "tastytrade",
            generation,
            kind,
        }
    }
}
