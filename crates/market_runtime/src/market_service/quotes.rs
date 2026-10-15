//! Latest validated top-of-book state for the shared instrument demand owner.

use std::collections::{BTreeMap, BTreeSet};

use aeris_instruments::SessionPhase;
use aeris_market_data::TopOfBookQuote;
use aeris_market_engine::InstrumentStreamKey;
use aeris_trading::{
    FixedPoint,
    market::{MarketRoute, TradingQuote},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuoteSourceKind {
    TopOfBook,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuoteState {
    pub quote: TradingQuote,
    pub phase: SessionPhase,
    pub source: QuoteSourceKind,
}

/// A bounded read projection. Only the coordinator writes; guards read the
/// same current projection without creating a second demand owner.
#[derive(Default)]
pub(super) struct QuoteBook {
    latest: BTreeMap<InstrumentStreamKey, QuoteState>,
}

impl QuoteBook {
    pub(super) fn retain(&mut self, required: &BTreeSet<InstrumentStreamKey>) {
        self.latest.retain(|key, _| required.contains(key));
    }

    pub(super) fn clear_provider(&mut self, provider: &str) {
        self.latest.retain(|key, _| key.provider != provider);
    }

    pub(super) fn latest(&self, key: &InstrumentStreamKey) -> Option<QuoteState> {
        self.latest.get(key).cloned()
    }

    pub(super) fn update(
        &mut self,
        key: InstrumentStreamKey,
        route: &MarketRoute,
        wire: &TopOfBookQuote,
        received_unix_nanos: i64,
        phase: SessionPhase,
    ) -> Option<QuoteState> {
        let (Some(bid), Some(ask)) = (wire.bid, wire.ask) else {
            self.latest.remove(&key);
            return None;
        };
        let (Ok(bid), Ok(ask)) = (
            FixedPoint::try_new(bid.price, route.price_scale),
            FixedPoint::try_new(ask.price, route.price_scale),
        ) else {
            return None;
        };
        let quote = TradingQuote {
            instrument_id: route.instrument_id.clone(),
            bid,
            ask,
            bid_size: wire
                .bid
                .and_then(|level| level.quantity)
                .and_then(|units| FixedPoint::try_new(units, route.quantity_scale).ok()),
            ask_size: wire
                .ask
                .and_then(|level| level.quantity)
                .and_then(|units| FixedPoint::try_new(units, route.quantity_scale).ok()),
            last: None,
            provider_id: route.provider_id.clone(),
            provider_generation: wire.metadata.session_generation,
            source_sequence: Some(wire.metadata.source_sequence),
            exchange_unix_nanos: wire.metadata.timestamps.exchange_unix_nanos,
            received_unix_nanos,
        };
        if quote.validate(route).is_err() {
            return None;
        }
        let state = QuoteState {
            quote,
            phase,
            source: QuoteSourceKind::TopOfBook,
        };
        self.latest.insert(key, state.clone());
        Some(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_instruments::InstrumentId;
    use aeris_market_data::{EventMetadata, QualifiedTimestamp, QuoteLevel};

    #[test]
    fn quotes_latest_validates_touch_replaces_and_retires_on_epoch() {
        let instrument = InstrumentId::try_new("instrument:hyperliquid:BTC").unwrap();
        let key = InstrumentStreamKey {
            provider: "hyperliquid".into(),
            instrument: instrument.clone(),
            entitlement: Some("public".into()),
        };
        let route = MarketRoute {
            provider_id: key.provider.clone(),
            instrument_id: instrument,
            provider_symbol: "BTC".into(),
            display_symbol: "BTC-USDC".into(),
            venue_id: "Hyperliquid".into(),
            entitlement_id: key.entitlement.clone(),
            price_scale: 2,
            quantity_scale: 8,
        };
        let mut wire = TopOfBookQuote {
            metadata: EventMetadata {
                provider_id: key.provider.clone(),
                instrument_id: key.instrument.as_str().into(),
                entitlement_id: "public".into(),
                session_generation: 2,
                source_sequence: 1,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(1),
                    provider_unix_nanos: None,
                    received_unix_nanos: 1,
                },
            },
            bid: Some(QuoteLevel {
                price: 100,
                quantity: Some(100_000_000),
                order_count: None,
            }),
            ask: Some(QuoteLevel {
                price: 101,
                quantity: Some(200_000_000),
                order_count: None,
            }),
        };
        let mut book = QuoteBook::default();
        let state = book
            .update(key.clone(), &route, &wire, 7, SessionPhase::AlwaysOpen)
            .expect("valid quote");
        assert_eq!(state.quote.received_unix_nanos, 7);
        assert_eq!(book.latest(&key), Some(state));
        wire.ask.as_mut().unwrap().price = 100;
        assert!(
            book.update(key.clone(), &route, &wire, 8, SessionPhase::AlwaysOpen)
                .is_none()
        );
        assert_eq!(book.latest(&key).unwrap().quote.received_unix_nanos, 7);
        book.clear_provider("hyperliquid");
        assert!(book.latest(&key).is_none());
    }
}
