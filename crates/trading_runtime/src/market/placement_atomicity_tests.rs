use super::*;
use crate::tests::{TestDirectory, config, instrument, market_order, start_service_with_config};
use crate::{OrderSide, OrderStatus, TradingServiceConfig, clock::ManualClock};
use aeris_instruments::{CalendarSourceKind, SessionPhase};
use aeris_trading::market::{
    FeedState, InstrumentSession, MarketFeedEvent, MarketRoute, TradingQuote,
};
use std::sync::{Arc, atomic::AtomicBool, mpsc};
use std::time::Duration;

fn open_core(configuration: &TradingServiceConfig) -> TradingCore {
    let (sender, _receiver) = mpsc::sync_channel(1);
    TradingCore::open(
        &configuration.database_path,
        configuration.retention,
        configuration.clock.clone(),
        configuration.execution,
        std::sync::Weak::new(),
        Arc::new(feed::MarketFeedInbox::new(
            sender,
            Arc::new(AtomicBool::new(false)),
        )),
    )
    .expect("core opens")
}

fn install_live_quote(core: &mut TradingCore) {
    let id = instrument().instrument_id;
    core.state.market_routes.insert(
        id.clone(),
        MarketRoute {
            provider_id: "fixture".into(),
            instrument_id: id.clone(),
            provider_symbol: "ESZ6".into(),
            display_symbol: "ESZ6".into(),
            venue_id: "CME".into(),
            entitlement_id: None,
            price_scale: 2,
            quantity_scale: 0,
        },
    );
    for event in [
        MarketFeedEvent::Session(InstrumentSession {
            instrument_id: id.clone(),
            provider_generation: 1,
            phase: SessionPhase::Open,
            source: CalendarSourceKind::ProviderCalendar,
            session_end_unix_nanos: Some(2_000_000_000_000),
            next_open_unix_nanos: None,
        }),
        MarketFeedEvent::Feed {
            instrument_id: id.clone(),
            state: FeedState::Live,
            provider_generation: 1,
        },
        MarketFeedEvent::Quote(TradingQuote {
            instrument_id: id,
            bid: FixedPoint::try_new(10_000, 2).unwrap(),
            ask: FixedPoint::try_new(10_025, 2).unwrap(),
            bid_size: None,
            ask_size: None,
            last: None,
            provider_id: "fixture".into(),
            provider_generation: 1,
            source_sequence: Some(1),
            exchange_unix_nanos: None,
            received_unix_nanos: 1_000_000_000_000,
        }),
    ] {
        core.quote_book
            .accept(event, &core.state.market_routes)
            .expect("feed event");
    }
}

#[test]
fn failed_placement_fill_commit_keeps_acceptance_fill_and_position_absent() {
    let directory = TestDirectory::new("placement-commit-rollback");
    let mut configuration = config(&directory);
    configuration.clock = Arc::new(ManualClock::new(1_000_000_000_000));
    let service = start_service_with_config(configuration.clone());
    service
        .register_instrument(instrument())
        .expect("instrument");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let mut core = open_core(&configuration);
    install_live_quote(&mut core);
    let request = market_order(
        "failed-placement-fill",
        OrderSide::Buy,
        1,
        1_000_000_000_000,
    );
    let baseline_sequence = core.state.next_sequence;
    let baseline_revision = core.state.revision;
    core.store.crash_during_commit(1);
    assert!(
        core.place_order(&request).is_err(),
        "SQLite rejected the composed step"
    );
    assert_eq!(core.state.next_sequence, baseline_sequence);
    assert_eq!(core.state.revision, baseline_revision);
    assert!(core.state.orders.is_empty());
    assert!(core.state.order_events.is_empty());
    assert!(core.state.fills.is_empty());
    assert!(core.state.positions.is_empty());
    core.store.crash_during_commit(2);
    let placed = core.place_order(&request).expect("retry");
    assert_eq!(placed.status, OrderStatus::Filled);
    assert_eq!(core.state.fills.len(), 1);
    drop(core);
    let recovered = open_core(&configuration);
    assert_eq!(recovered.state.orders.get(&placed.id), Some(&placed));
    assert_eq!(recovered.state.fills.len(), 1);
    assert_eq!(recovered.state.positions.len(), 1);
}
