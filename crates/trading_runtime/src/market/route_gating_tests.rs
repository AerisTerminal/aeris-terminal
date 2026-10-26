use super::placement_atomicity_tests::open_core;
use super::*;
use crate::tests::market_feed::{feed_state, live_session, null_persisted_route, quote, terms};
use crate::tests::{
    TestDirectory, config, instrument, market_order, observation, start_service_with_config,
};
use crate::{
    FixedPoint, OrderSide, OrderStatus, OrderType, TimeInForce, TradingServiceConfig,
    clock::ManualClock,
};
use aeris_trading::RejectReason;
use aeris_trading::market::{FeedReason, FeedState, MarketFeedEvent};
use std::sync::{Arc, mpsc};

const CLOCK_START: i64 = 1_000_000_000_000;

/// A store holding the fixture instrument without a verified market route, as a V20-migrated
/// legacy instrument is before its Terms arrive.
fn route_less_store(directory: &TestDirectory) -> TradingServiceConfig {
    let mut configuration = config(directory);
    configuration.clock = Arc::new(ManualClock::new(CLOCK_START));
    let service = start_service_with_config(configuration.clone());
    service
        .register_instrument(instrument())
        .expect("instrument");
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
    configuration
}

/// Delivers an open session, a Live feed and a quote through the owner feed path, optionally
/// preceded by the verified Terms that carry the instrument's route.
fn deliver_feed(core: &mut TradingCore, with_terms: bool, sequence: u64, bid: i64, ask: i64) {
    let id = instrument().instrument_id;
    let mut controls = Vec::new();
    if with_terms {
        controls.push(MarketFeedEvent::Terms(terms()));
    }
    controls.push(live_session(id.clone(), 1));
    controls.push(feed_state(id, 1, FeedState::Live));
    core.apply_market_batch(feed::MarketFeedBatch {
        controls,
        quotes: vec![quote(1, sequence, bid, ask)],
        resync: false,
    });
}

fn owner_feed_state(core: &TradingCore) -> FeedState {
    core.quote_book
        .feed_state(&instrument().instrument_id, &core.state.market_routes)
}

fn route_unknown() -> FeedState {
    FeedState::Unavailable {
        reason: FeedReason::RouteUnknown,
    }
}

/// A direct crossing observation received after the order's `eligible_after` and the
/// recovery floor, so only route gating can stop it from filling. It shares the owner book's
/// first revision with different prices, so the later verified quote is not fenced as stale.
fn observe_eligible_cross(core: &mut TradingCore) -> Vec<crate::Fill> {
    let (reply, response) = mpsc::sync_channel(1);
    core.dispatch_market_command(Command::Observe(
        observation(9_850, 9_875, 1, CLOCK_START + 3_000_000),
        reply,
    ));
    response.recv().expect("observe reply").expect("observe")
}

fn resting_buy_limit(client_id: &str) -> crate::PlaceOrder {
    let mut request = market_order(client_id, OrderSide::Buy, 1, CLOCK_START);
    request.order_type = OrderType::Limit;
    request.time_in_force = TimeInForce::GoodTillCancelled;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).expect("limit"));
    request
}

#[test]
fn route_less_instrument_rejects_admission_with_quote_unavailable() {
    let directory = TestDirectory::new("route-less-admission");
    let configuration = route_less_store(&directory);
    let mut core = open_core(&configuration);
    // Session, Live feed and quote without Terms are ignored: only a verified route makes an
    // instrument's feed executable.
    deliver_feed(&mut core, false, 1, 10_000, 10_025);
    assert_eq!(owner_feed_state(&core), route_unknown());
    let revision = core.state.revision;

    for request in [
        market_order("route-less-market", OrderSide::Buy, 1, CLOCK_START),
        resting_buy_limit("route-less-limit"),
    ] {
        assert_eq!(
            core.place_order(&request).unwrap_err(),
            RejectReason::QuoteUnavailable.to_string(),
            "{}",
            request.client_order_id.as_str()
        );
    }
    assert!(core.state.orders.is_empty(), "a rejection writes no order");
    assert!(core.state.order_events.is_empty());
    assert_eq!(core.state.revision, revision);
    assert_eq!(owner_feed_state(&core), route_unknown());
}

#[test]
fn route_less_resting_order_never_fills_until_verified_terms_resolve_the_route() {
    let directory = TestDirectory::new("route-less-resting");
    let configuration = route_less_store(&directory);
    let mut core = open_core(&configuration);
    deliver_feed(&mut core, true, 1, 10_000, 10_025);
    let placed = core
        .place_order(&resting_buy_limit("route-less-resting"))
        .expect("routed resting limit");
    assert_eq!(placed.status, OrderStatus::Working);
    drop(core);
    null_persisted_route(&directory, &instrument());

    let mut core = open_core(&configuration);
    assert_eq!(owner_feed_state(&core), route_unknown());
    deliver_feed(&mut core, false, 2, 9_875, 9_900);
    assert_eq!(
        observe_eligible_cross(&mut core),
        Vec::new(),
        "a crossing observation cannot fill a route-less order"
    );
    assert_eq!(owner_feed_state(&core), route_unknown());
    let resting = core.state.orders.get(&placed.id).expect("resting order");
    assert_eq!(resting.status, OrderStatus::Working);
    assert!(core.state.fills.is_empty());

    deliver_feed(&mut core, true, 4, 9_875, 9_900);
    assert_eq!(owner_feed_state(&core), FeedState::Live);
    let filled = core.state.orders.get(&placed.id).expect("filled order");
    assert_eq!(filled.status, OrderStatus::Filled);
    assert_eq!(core.state.fills.len(), 1);
    let fill = core.state.fills.iter().next().expect("fill");
    assert_eq!(fill.order_id, placed.id);
    assert_eq!(fill.price, FixedPoint::try_new(9_900, 2).expect("price"));
}
