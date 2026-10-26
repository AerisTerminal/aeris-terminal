use super::{
    DepthSnapshot, MarketBar, MarketTrade, OrderLevelEvent, TopOfBookQuote,
    lifecycle::RecoveryReason,
};
use crate::hyperliquid_realtime::HyperliquidRealtimeEvent;
use crate::rithmic_realtime::RithmicRealtimeEvent;
use aeris_market_engine::fence::{Epoch, ProviderTag};
use aeris_rithmic_protocol_adapter::ProviderInvalidationReason;
use std::collections::BTreeMap;

pub(super) const fn rithmic_invalidation_detail(
    reason: Option<ProviderInvalidationReason>,
) -> &'static str {
    match reason {
        Some(ProviderInvalidationReason::Transport) => "Rithmic transport connection failed",
        Some(ProviderInvalidationReason::Authentication) => {
            "Rithmic refused the saved login; check the user ID, password and system, and that the account has market data and Aeris enabled"
        }
        Some(ProviderInvalidationReason::AgreementRequired) => {
            "Rithmic account requires an agreement before market data can stream"
        }
        Some(ProviderInvalidationReason::UnsupportedSystem) => {
            "The selected Rithmic system is not offered on this server"
        }
        Some(ProviderInvalidationReason::SchemaMismatch) => {
            "Rithmic protocol schema does not match the installed provider kit"
        }
        Some(ProviderInvalidationReason::HeartbeatSilence) => {
            "Rithmic heartbeat response timed out"
        }
        Some(ProviderInvalidationReason::MessageSilence) => {
            "Rithmic market-data stream stopped producing messages"
        }
        Some(ProviderInvalidationReason::SequenceGap) => {
            "Rithmic market-data sequence continuity was lost"
        }
        Some(ProviderInvalidationReason::QueueOverflow) => {
            "Rithmic callback queue overflowed before market data could be consumed"
        }
        Some(ProviderInvalidationReason::MalformedMessage) => {
            "Rithmic sent market data that failed protocol validation"
        }
        None => "Rithmic live session is recovering",
    }
}

pub(super) const fn rithmic_auto_recovers(reason: Option<ProviderInvalidationReason>) -> bool {
    matches!(
        reason,
        None | Some(
            ProviderInvalidationReason::Transport
                | ProviderInvalidationReason::HeartbeatSilence
                | ProviderInvalidationReason::MessageSilence
                | ProviderInvalidationReason::SequenceGap
                | ProviderInvalidationReason::QueueOverflow
        )
    )
}

pub(super) struct ProviderEvent {
    pub(super) provider: &'static str,
    pub(super) generation: Epoch<ProviderTag>,
    pub(super) wire_generation: u64,
    pub(super) kind: ProviderEventKind,
}

/// Untrusted adapter identity, retained only at adapter conversion boundaries.
pub(super) struct WireProviderEvent {
    pub(super) provider: &'static str,
    pub(super) generation: u64,
    pub(super) kind: ProviderEventKind,
}

/// Tracks accepted adapter generations; epoch minting belongs to `ProviderRegistry`.
#[derive(Default)]
pub(super) struct ProviderEpochRegistry {
    active: BTreeMap<String, (u64, Epoch<ProviderTag>)>,
    stale_events: u64,
    stale_history: u64,
}

impl ProviderEpochRegistry {
    #[cfg(test)]
    pub(super) fn stamp(
        &mut self,
        provider: &str,
        wire: u64,
        registry: &mut super::provider_session::ProviderRegistry,
    ) -> Option<Epoch<ProviderTag>> {
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
        let epoch = registry.reserve();
        self.active.insert(provider.to_owned(), (wire, epoch));
        Some(epoch)
    }

    pub(super) fn bind(&mut self, provider: &str, epoch: Epoch<ProviderTag>) {
        self.bind_wire(provider, epoch, epoch.get().get());
    }

    pub(super) fn bind_wire(&mut self, provider: &str, epoch: Epoch<ProviderTag>, wire: u64) {
        self.active.insert(provider.to_owned(), (wire, epoch));
    }

    pub(super) fn accepts(&self, provider: &str, epoch: Epoch<ProviderTag>) -> bool {
        self.active
            .get(provider)
            .is_some_and(|(_, current)| current == &epoch)
    }
    pub(super) fn current(&self, provider: &str, wire: u64) -> Option<Epoch<ProviderTag>> {
        self.active
            .get(provider)
            .and_then(|(number, epoch)| (*number == wire).then_some(*epoch))
    }

    pub(super) fn drop_event(&mut self) {
        self.stale_events = self.stale_events.saturating_add(1);
    }

    pub(super) fn drop_history(&mut self) {
        self.stale_history = self.stale_history.saturating_add(1);
    }

    #[cfg(any(test, feature = "probe"))]
    pub(super) fn stale_counts(&self) -> (u64, u64) {
        (self.stale_events, self.stale_history)
    }
}

pub(super) enum ProviderTradeBatch {
    One(MarketTrade),
    Many(Vec<MarketTrade>),
}

#[derive(Clone)]
pub(super) enum ProviderDisconnect {
    SequenceGap {
        detail: &'static str,
        auto_recover: bool,
    },
    Recover {
        detail: &'static str,
        auto_recover: bool,
        retry_after: Option<std::time::Duration>,
        provider_detail: Option<String>,
    },
    Fail {
        detail: &'static str,
        auto_recover: bool,
    },
}

pub(super) enum ProviderEventKind {
    /// The adapter replaced its transport; the registry must reserve a new epoch and rebind.
    TransportReplaced,
    Connecting,
    Connected,
    Heartbeat(Option<u64>),
    Recovering {
        detail: &'static str,
        provider_detail: Option<String>,
        recovery_reason: Option<RecoveryReason>,
    },
    Failed(String),
    Disconnected(ProviderDisconnect),
    Trades(ProviderTradeBatch),
    Quote(TopOfBookQuote),
    Depth(DepthSnapshot),
    OrderLevels(OrderLevelEvent),
    Candle {
        symbol: ProviderCandleSymbol,
        bar: MarketBar,
    },
}

pub(super) struct ProviderCandleSymbol {
    pub(super) coin: String,
    pub(super) interval: String,
}

impl ProviderCandleSymbol {
    pub(super) fn matches(&self, coin: &str, interval: &str) -> bool {
        self.coin == coin && self.interval == interval
    }
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
                detail: rithmic_invalidation_detail(reason),
                provider_detail: None,
                recovery_reason: (reason == Some(ProviderInvalidationReason::SequenceGap))
                    .then_some(RecoveryReason::SequenceGap),
            },
            Wire::Disconnected(_, reason) => {
                let detail = rithmic_invalidation_detail(reason);
                let auto_recover = rithmic_auto_recovers(reason);
                let disposition = if reason == Some(ProviderInvalidationReason::SequenceGap) {
                    ProviderDisconnect::SequenceGap {
                        detail,
                        auto_recover,
                    }
                } else if reason.is_some() {
                    ProviderDisconnect::Fail {
                        detail,
                        auto_recover,
                    }
                } else {
                    ProviderDisconnect::Recover {
                        detail,
                        auto_recover,
                        retry_after: None,
                        provider_detail: None,
                    }
                };
                ProviderEventKind::Disconnected(disposition)
            }
            Wire::Trade(_, trade) => ProviderEventKind::Trades(ProviderTradeBatch::One(trade)),
            Wire::Quote(_, quote) => ProviderEventKind::Quote(quote),
            Wire::Depth(_, depth) => ProviderEventKind::Depth(depth),
            Wire::OrderLevels(_, event) => ProviderEventKind::OrderLevels(event),
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
                recovery_reason: None,
            },
            Wire::Disconnected(_) => ProviderEventKind::Disconnected(ProviderDisconnect::Recover {
                detail: "Hyperliquid live session is recovering",
                auto_recover: false,
                retry_after: None,
                provider_detail: None,
            }),
            Wire::Candle(_, coin, interval, candle) => ProviderEventKind::Candle {
                symbol: ProviderCandleSymbol { coin, interval },
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
}
