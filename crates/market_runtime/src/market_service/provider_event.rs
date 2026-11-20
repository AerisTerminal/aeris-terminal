use super::{
    DepthSnapshot, MarketBar, MarketTrade, OrderLevelEvent, TopOfBookQuote,
    lifecycle::RecoveryReason,
};
use aeris_market_engine::fence::{Epoch, ProviderTag};
pub(crate) use aeris_provider_spi::{ProviderFault, ProviderFaultKind};
use std::collections::BTreeMap;

pub(super) struct ProviderEvent {
    pub(super) provider: &'static str,
    pub(super) generation: Epoch<ProviderTag>,
    pub(super) wire_generation: u64,
    pub(super) kind: ProviderEventKind,
}

/// Untrusted adapter identity, retained only at adapter conversion boundaries.
pub(crate) struct WireProviderEvent {
    pub(crate) provider: &'static str,
    pub(crate) generation: u64,
    pub(crate) kind: ProviderEventKind,
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

pub(crate) enum ProviderTradeBatch {
    One(MarketTrade),
    Many(Vec<MarketTrade>),
}

#[derive(Clone)]
pub(crate) enum ProviderDisconnect {
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

impl ProviderDisconnect {
    pub(crate) const fn from_fault(fault: ProviderFault) -> Self {
        match fault.kind {
            ProviderFaultKind::SequenceGap => Self::SequenceGap {
                detail: fault.detail,
                auto_recover: fault.auto_recovers,
            },
            ProviderFaultKind::Transient => Self::Recover {
                detail: fault.detail,
                auto_recover: fault.auto_recovers,
                retry_after: None,
                provider_detail: None,
            },
            ProviderFaultKind::Transport
            | ProviderFaultKind::Authentication
            | ProviderFaultKind::AgreementRequired
            | ProviderFaultKind::UnsupportedSystem
            | ProviderFaultKind::SchemaMismatch
            | ProviderFaultKind::HeartbeatSilence
            | ProviderFaultKind::MessageSilence
            | ProviderFaultKind::QueueOverflow
            | ProviderFaultKind::MalformedMessage => Self::Fail {
                detail: fault.detail,
                auto_recover: fault.auto_recovers,
            },
        }
    }
}

pub(crate) enum ProviderEventKind {
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

impl ProviderEventKind {
    pub(crate) const fn recovering_from_fault(fault: ProviderFault) -> Self {
        let recovery_reason = match fault.kind {
            ProviderFaultKind::SequenceGap => Some(RecoveryReason::SequenceGap),
            ProviderFaultKind::Transport
            | ProviderFaultKind::Authentication
            | ProviderFaultKind::AgreementRequired
            | ProviderFaultKind::UnsupportedSystem
            | ProviderFaultKind::SchemaMismatch
            | ProviderFaultKind::HeartbeatSilence
            | ProviderFaultKind::MessageSilence
            | ProviderFaultKind::QueueOverflow
            | ProviderFaultKind::MalformedMessage
            | ProviderFaultKind::Transient => None,
        };
        Self::Recovering {
            detail: fault.detail,
            provider_detail: None,
            recovery_reason,
        }
    }
}

pub(crate) struct ProviderCandleSymbol {
    pub(crate) coin: String,
    pub(crate) interval: String,
}

impl ProviderCandleSymbol {
    pub(super) fn matches(&self, coin: &str, interval: &str) -> bool {
        self.coin == coin && self.interval == interval
    }
}
