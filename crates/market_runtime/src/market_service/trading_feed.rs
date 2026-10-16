//! Bounded, latest-wins delivery from the market coordinator to trading.
//! The coordinator never invokes an external callback or waits on trading.

#[cfg(test)]
use std::time::Duration;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Condvar, Mutex},
};

use aeris_contracts::{InstallProviderInstrument, ProviderInstrumentClass};
use aeris_instruments::{
    ContractDate, InstrumentClass, InstrumentDecimal, InstrumentId, PriceGrid, SessionHours,
};
use aeris_trading::market::MarketFeedEvent;
use aeris_trading::market::{InstrumentTerms, MarketRoute};

const MAXIMUM_PENDING_EVENTS: usize = 4 * aeris_trading::market::MAXIMUM_QUOTE_INTEREST;

/// Converts verified catalog terms once at the market ownership boundary.
/// A display increment is not assumed to be a constant exchange tick when
/// the provider supplies a significant-figure grid.
///
/// # Errors
/// Returns an error if required provider terms or explicit scales are invalid.
pub fn instrument_terms_from_install(
    instrument: &InstallProviderInstrument,
) -> Result<InstrumentTerms, String> {
    let unavailable = |detail: &str| {
        format!(
            "Practice trading unavailable for {}: {detail}",
            instrument.display_symbol
        )
    };
    let metadata = instrument
        .contract_metadata
        .as_deref()
        .ok_or_else(|| unavailable("contract metadata missing"))?;
    let currency = metadata
        .currency
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| unavailable("contract currency missing"))?;
    if metadata.point_value.is_none() || metadata.point_value_scale.is_none() {
        return Err(unavailable("contract point value missing"));
    }
    let price_scale = checked_scale(instrument.price_scale)?;
    let quantity_scale = checked_scale(instrument.quantity_scale)?;
    let price_grid = match (
        metadata.price_significant_figures,
        metadata.price_max_decimals,
    ) {
        (Some(figures), Some(max_decimals)) => PriceGrid::SignificantFigures {
            figures: checked_scale(figures)?,
            max_decimals: checked_scale(max_decimals)?,
        },
        (None, None) => instrument
            .price_increment
            .map(|units| InstrumentDecimal::try_new(units, price_scale))
            .transpose()
            .map_err(|error| error.to_string())?
            .filter(|increment| increment.units() > 0)
            .map_or(PriceGrid::Unknown, PriceGrid::Fixed),
        _ => return Err(unavailable("price grid is incomplete")),
    };
    let instrument_class = match metadata
        .instrument_class
        .and_then(|value| ProviderInstrumentClass::try_from(value).ok())
    {
        Some(ProviderInstrumentClass::Equity) => InstrumentClass::Equity,
        Some(ProviderInstrumentClass::Option) => InstrumentClass::Option,
        Some(ProviderInstrumentClass::Future) => InstrumentClass::Future,
        Some(ProviderInstrumentClass::ForeignExchange) => InstrumentClass::ForeignExchange,
        Some(ProviderInstrumentClass::CryptoSpot) => InstrumentClass::CryptoSpot,
        Some(ProviderInstrumentClass::CryptoPerpetual) => InstrumentClass::CryptoPerpetual,
        _ => InstrumentClass::Other,
    };
    let terms = InstrumentTerms {
        route: MarketRoute {
            provider_id: instrument.provider.clone(),
            instrument_id: InstrumentId::try_new(instrument.instrument_id.clone())
                .map_err(|error| error.to_string())?,
            provider_symbol: instrument.provider_symbol.clone(),
            display_symbol: instrument.display_symbol.clone(),
            venue_id: instrument.venue_id.clone(),
            entitlement_id: Some(instrument.entitlement_id.clone()),
            price_scale,
            quantity_scale,
        },
        instrument_class,
        price_grid,
        point_value: metadata
            .point_value
            .zip(metadata.point_value_scale)
            .map(|(units, scale)| {
                InstrumentDecimal::try_new(units, checked_scale(scale)?)
                    .map_err(|error| error.to_string())
            })
            .transpose()?,
        quantity_increment: metadata
            .order_quantity_increment
            .map(|units| InstrumentDecimal::try_new(units, quantity_scale))
            .transpose()
            .map_err(|error| error.to_string())?,
        currency,
        expiry: metadata
            .contract_expiry
            .as_deref()
            .map(parse_contract_date)
            .transpose()?,
        first_notice: metadata
            .first_notice_date
            .as_deref()
            .map(parse_contract_date)
            .transpose()?,
        last_trade: metadata
            .last_trade_date
            .as_deref()
            .map(parse_contract_date)
            .transpose()?,
        session_hours: verified_session_hours(metadata, &unavailable)?,
        session_generation: instrument.session_generation,
    };
    terms.validate().map_err(|error| error.to_string())?;
    Ok(terms)
}

fn verified_session_hours(
    metadata: &aeris_contracts::ProviderContractMetadata,
    unavailable: &impl Fn(&str) -> String,
) -> Result<Vec<SessionHours>, String> {
    metadata
        .session_hours
        .iter()
        .map(|hours| {
            let value = SessionHours {
                weekday: u8::try_from(hours.weekday)
                    .map_err(|_| unavailable("session weekday is invalid"))?,
                open_seconds: hours.open_seconds,
                close_seconds: hours.close_seconds,
                timezone: hours.timezone.clone(),
            };
            value
                .validate()
                .map_err(|_| unavailable("session hours are invalid"))?;
            Ok(value)
        })
        .collect()
}

fn parse_contract_date(value: &str) -> Result<ContractDate, String> {
    let (year, rest) = value
        .split_once('-')
        .ok_or_else(|| "provider contract date is invalid".to_string())?;
    let (month, day) = rest
        .split_once('-')
        .ok_or_else(|| "provider contract date is invalid".to_string())?;
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return Err("provider contract date is invalid".into());
    }
    let date = ContractDate {
        year: year
            .parse()
            .map_err(|_| "provider contract date is invalid")?,
        month: month
            .parse()
            .map_err(|_| "provider contract date is invalid")?,
        day: day
            .parse()
            .map_err(|_| "provider contract date is invalid")?,
    };
    date.validate().map_err(|error| error.to_string())?;
    Ok(date)
}

fn checked_scale(scale: u32) -> Result<u8, String> {
    let scale =
        u8::try_from(scale).map_err(|_| "provider instrument scale is invalid".to_string())?;
    if scale > 18 {
        return Err("provider instrument scale exceeds 18".into());
    }
    Ok(scale)
}

/// The trading owner receives events on a dedicated market worker, never on
/// the coordinator or UI thread.
pub struct TradingFeedLink {
    pub events: Box<dyn Fn(MarketFeedEvent) + Send + 'static>,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
enum EventKey {
    Terms(String),
    Session(String),
    Feed(String),
    Quote(String),
}

impl EventKey {
    fn for_event(event: &MarketFeedEvent) -> Self {
        match event {
            MarketFeedEvent::Quote(quote) => Self::Quote(quote.instrument_id.as_str().into()),
            MarketFeedEvent::Feed { instrument_id, .. } => {
                Self::Feed(instrument_id.as_str().into())
            }
            MarketFeedEvent::Session(session) => {
                Self::Session(session.instrument_id.as_str().into())
            }
            MarketFeedEvent::Terms(terms) => Self::Terms(terms.route.instrument_id.as_str().into()),
        }
    }
}

#[derive(Default)]
pub(super) struct TradingFeedOutbox {
    pending: Mutex<BTreeMap<EventKey, MarketFeedEvent>>,
    wake: Condvar,
    attached: AtomicBool,
}

impl TradingFeedOutbox {
    pub(super) fn attached(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }

    pub(super) fn attach(&self) {
        self.attached.store(true, Ordering::Release);
    }

    pub(super) fn publish(&self, event: MarketFeedEvent) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = EventKey::for_event(&event);
        if !pending.contains_key(&key) && pending.len() == MAXIMUM_PENDING_EVENTS {
            // Keep route and generation controls ahead of prices under load.
            // Never evict a control just to admit a lower-priority quote.
            if pending
                .last_key_value()
                .is_some_and(|(last, _)| key >= *last)
            {
                return;
            }
            pending.pop_last();
        }
        pending.insert(key, event);
        self.wake.notify_one();
    }

    #[cfg(test)]
    pub(super) fn receive(&self, timeout: Duration) -> Option<MarketFeedEvent> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut pending = self
            .wake
            .wait_timeout_while(pending, timeout, |events| events.is_empty())
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
        pending.pop_first().map(|(_, event)| event)
    }

    pub(super) fn receive_until_shutdown(&self, shutdown: &AtomicBool) -> Option<MarketFeedEvent> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut pending = self
            .wake
            .wait_while(pending, |events| {
                events.is_empty() && !shutdown.load(Ordering::Acquire)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.pop_first().map(|(_, event)| event)
    }

    pub(super) fn wake(&self) {
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_instruments::InstrumentId;
    use aeris_instruments::{CalendarSourceKind, InstrumentDecimal, PriceGrid, SessionPhase};
    use aeris_trading::market::{
        FeedReason, FeedState, InstrumentSession, MarketRoute, TradingQuote,
    };

    #[test]
    fn cached_snapshot_survives_a_drain_between_each_event() {
        let outbox = TradingFeedOutbox::default();
        let id = InstrumentId::try_new("instrument:BTC").unwrap();
        let route = MarketRoute {
            provider_id: "fixture".into(),
            instrument_id: id.clone(),
            provider_symbol: "BTC".into(),
            display_symbol: "BTC".into(),
            venue_id: "fixture".into(),
            entitlement_id: None,
            price_scale: 2,
            quantity_scale: 0,
        };
        // A restored, chart-free lease already has a current-generation quote.
        outbox.publish(MarketFeedEvent::Terms(InstrumentTerms {
            route,
            instrument_class: InstrumentClass::CryptoSpot,
            price_grid: PriceGrid::Unknown,
            point_value: Some(InstrumentDecimal::try_new(1, 0).unwrap()),
            quantity_increment: None,
            currency: "USD".into(),
            expiry: None,
            first_notice: None,
            last_trade: None,
            session_hours: Vec::new(),
            session_generation: 1,
        }));
        let quote = TradingQuote {
            instrument_id: id.clone(),
            bid: aeris_trading::FixedPoint::try_new(100, 2).unwrap(),
            ask: aeris_trading::FixedPoint::try_new(101, 2).unwrap(),
            bid_size: None,
            ask_size: None,
            last: None,
            provider_id: "fixture".into(),
            provider_generation: 2,
            source_sequence: Some(1),
            exchange_unix_nanos: None,
            received_unix_nanos: 10,
        };
        outbox.publish(MarketFeedEvent::Quote(quote.clone()));
        outbox.publish(MarketFeedEvent::Session(InstrumentSession {
            instrument_id: id.clone(),
            provider_generation: 2,
            phase: SessionPhase::AlwaysOpen,
            source: CalendarSourceKind::AlwaysOpen,
            session_end_unix_nanos: None,
            next_open_unix_nanos: None,
        }));
        outbox.publish(MarketFeedEvent::Feed {
            instrument_id: id,
            state: FeedState::Live,
            provider_generation: 2,
        });

        // Each receive represents an individual trading-owner drain. An early
        // quote is lost without Terms, and the first Live feed clears it.
        let (mut terms, mut session, mut live, mut current_quote) = (false, false, false, None);
        let mut delivered = Vec::new();
        while let Some(event) = outbox.receive(Duration::ZERO) {
            match &event {
                MarketFeedEvent::Terms(_) => terms = true,
                MarketFeedEvent::Session(_) if terms => session = true,
                MarketFeedEvent::Feed { state, .. } if terms => {
                    if !live || *state != FeedState::Live {
                        current_quote = None;
                    }
                    live = *state == FeedState::Live;
                }
                MarketFeedEvent::Quote(value) if terms => current_quote = Some(value.clone()),
                _ => {}
            }
            delivered.push(event);
        }
        assert_eq!(delivered.len(), 4, "each kind has one latest state");
        assert!(matches!(delivered[0], MarketFeedEvent::Terms(_)));
        assert!(matches!(delivered[1], MarketFeedEvent::Session(_)));
        assert!(matches!(delivered[2], MarketFeedEvent::Feed { .. }));
        assert!(matches!(delivered[3], MarketFeedEvent::Quote(_)));
        assert!(session && live);
        assert_eq!(
            current_quote,
            Some(quote),
            "a cached quote must remain current after separate owner drains"
        );
    }

    #[test]
    fn trading_feed_events_are_bounded_and_latest_wins() {
        let inbox = TradingFeedOutbox::default();
        for generation in 1..=20 {
            inbox.publish(MarketFeedEvent::Feed {
                instrument_id: InstrumentId::try_new("instrument:BTC").unwrap(),
                state: FeedState::Stale {
                    reason: FeedReason::Disconnected,
                },
                provider_generation: generation,
            });
        }
        assert!(matches!(
            inbox.receive(Duration::ZERO),
            Some(MarketFeedEvent::Feed {
                provider_generation: 20,
                ..
            })
        ));
        assert!(inbox.receive(Duration::ZERO).is_none());
    }

    #[test]
    fn full_outbox_prefers_controls_to_cached_quotes() {
        let outbox = TradingFeedOutbox::default();
        for number in 0..MAXIMUM_PENDING_EVENTS {
            outbox.publish(MarketFeedEvent::Quote(TradingQuote {
                instrument_id: InstrumentId::try_new(format!("instrument:{number}")).unwrap(),
                bid: aeris_trading::FixedPoint::try_new(100, 2).unwrap(),
                ask: aeris_trading::FixedPoint::try_new(101, 2).unwrap(),
                bid_size: None,
                ask_size: None,
                last: None,
                provider_id: "fixture".into(),
                provider_generation: 1,
                source_sequence: Some(1),
                exchange_unix_nanos: None,
                received_unix_nanos: 10,
            }));
        }
        let id = InstrumentId::try_new("instrument:control").unwrap();
        outbox.publish(MarketFeedEvent::Session(InstrumentSession {
            instrument_id: id.clone(),
            provider_generation: 1,
            phase: SessionPhase::AlwaysOpen,
            source: CalendarSourceKind::AlwaysOpen,
            session_end_unix_nanos: None,
            next_open_unix_nanos: None,
        }));
        outbox.publish(MarketFeedEvent::Feed {
            instrument_id: id,
            state: FeedState::Live,
            provider_generation: 1,
        });
        assert!(matches!(
            outbox.receive(Duration::ZERO),
            Some(MarketFeedEvent::Session(_))
        ));
        assert!(matches!(
            outbox.receive(Duration::ZERO),
            Some(MarketFeedEvent::Feed { .. })
        ));
        let mut quote_count = 0;
        while let Some(event) = outbox.receive(Duration::ZERO) {
            assert!(matches!(event, MarketFeedEvent::Quote(_)));
            quote_count += 1;
        }
        assert_eq!(quote_count, MAXIMUM_PENDING_EVENTS - 2);
    }

    #[test]
    fn full_control_outbox_does_not_evict_state_for_a_quote() {
        let outbox = TradingFeedOutbox::default();
        for number in 0..MAXIMUM_PENDING_EVENTS {
            outbox.publish(MarketFeedEvent::Session(InstrumentSession {
                instrument_id: InstrumentId::try_new(format!("instrument:{number}")).unwrap(),
                provider_generation: 1,
                phase: SessionPhase::AlwaysOpen,
                source: CalendarSourceKind::AlwaysOpen,
                session_end_unix_nanos: None,
                next_open_unix_nanos: None,
            }));
        }
        outbox.publish(MarketFeedEvent::Quote(TradingQuote {
            instrument_id: InstrumentId::try_new("instrument:overflow").unwrap(),
            bid: aeris_trading::FixedPoint::try_new(100, 2).unwrap(),
            ask: aeris_trading::FixedPoint::try_new(101, 2).unwrap(),
            bid_size: None,
            ask_size: None,
            last: None,
            provider_id: "fixture".into(),
            provider_generation: 1,
            source_sequence: Some(1),
            exchange_unix_nanos: None,
            received_unix_nanos: 10,
        }));
        for _ in 0..MAXIMUM_PENDING_EVENTS {
            assert!(matches!(
                outbox.receive(Duration::ZERO),
                Some(MarketFeedEvent::Session(_))
            ));
        }
        assert!(outbox.receive(Duration::ZERO).is_none());
    }

    #[test]
    fn trading_feed_terms_keep_verified_significant_figure_grid() {
        let instrument = InstallProviderInstrument {
            provider: "hyperliquid".into(),
            session_generation: 1,
            instrument_id: "hyperliquid:perp:BTC".into(),
            provider_symbol: "BTC".into(),
            display_symbol: "BTC-USDC".into(),
            venue_id: "Hyperliquid".into(),
            entitlement_id: "hyperliquid-public".into(),
            price_scale: 8,
            quantity_scale: 8,
            price_increment: Some(100_000),
            contract_metadata: Some(Box::new(aeris_contracts::ProviderContractMetadata {
                currency: Some("USD".into()),
                point_value: Some(1),
                point_value_scale: Some(0),
                price_significant_figures: Some(5),
                price_max_decimals: Some(1),
                instrument_class: Some(ProviderInstrumentClass::CryptoPerpetual as i32),
                ..Default::default()
            })),
            ..Default::default()
        };
        let terms = instrument_terms_from_install(&instrument).unwrap();
        assert_eq!(
            terms.price_grid,
            PriceGrid::SignificantFigures {
                figures: 5,
                max_decimals: 1
            }
        );
        assert_eq!(terms.instrument_class, InstrumentClass::CryptoPerpetual);
        assert_eq!(terms.route.price_scale, 8);
        let mut incomplete = instrument;
        incomplete
            .contract_metadata
            .as_mut()
            .unwrap()
            .price_max_decimals = None;
        assert!(instrument_terms_from_install(&incomplete).is_err());
    }

    #[test]
    fn trading_feed_terms_preserve_verified_contract_dates_hours_and_generation() {
        let instrument = InstallProviderInstrument {
            provider: "fixture".into(),
            instrument_id: "fixture:future".into(),
            provider_symbol: "FUT".into(),
            display_symbol: "Future".into(),
            venue_id: "EXCHANGE".into(),
            entitlement_id: "public".into(),
            session_generation: 27,
            selection_generation: 1,
            price_scale: 2,
            quantity_scale: 0,
            contract_metadata: Some(Box::new(aeris_contracts::ProviderContractMetadata {
                currency: Some("USD".into()),
                point_value: Some(50),
                point_value_scale: Some(0),
                contract_expiry: Some("2026-12-18".into()),
                first_notice_date: Some("2026-12-01".into()),
                last_trade_date: Some("2026-12-17".into()),
                session_hours: vec![aeris_contracts::ProviderSessionHours {
                    weekday: 1,
                    open_seconds: 3600,
                    close_seconds: 7200,
                    timezone: "America/Chicago".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        };
        let terms = instrument_terms_from_install(&instrument).unwrap();
        assert_eq!(terms.session_generation, 27);
        assert_eq!(
            terms.expiry,
            Some(aeris_instruments::ContractDate {
                year: 2026,
                month: 12,
                day: 18
            })
        );
        assert_eq!(terms.first_notice.unwrap().day, 1);
        assert_eq!(terms.last_trade.unwrap().day, 17);
        assert_eq!(terms.session_hours.len(), 1);
        assert_eq!(terms.session_hours[0].timezone, "America/Chicago");

        let mut invalid = instrument;
        invalid.contract_metadata.as_mut().unwrap().contract_expiry = Some("2026-02-30".into());
        assert!(instrument_terms_from_install(&invalid).is_err());
    }
}
