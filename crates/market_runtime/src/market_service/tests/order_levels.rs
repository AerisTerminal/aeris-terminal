// Order-level (market-by-order) delivery through the coordinator. Provider
// boundary events reach only consumers that demand order levels, with exact
// order identity, queue priority and continuity evidence; gaps and retired
// sessions fail closed; and the provider's bounded order-level subscriptions
// are granted deterministically under multi-instrument demand.
use super::boundary::Boundary;
use super::*;
use crate::rithmic_realtime::{RithmicRealtimeControl, RithmicRealtimeDemand};
use crate::{
    MAXIMUM_ORDER_LEVEL_VIEW_PRICES, MAXIMUM_ORDER_LEVEL_VIEW_QUEUE, MarketOrderBookSnapshot,
    MarketOrderLevelStatus, MarketOrderLevelView, MarketOrderQueue,
};
use aeris_market_data::{
    BookSide, OrderLevelChange, OrderLevelEvent, OrderLevelInterruption, OrderLevelReset,
    OrderLevelSnapshot, OrderLevelUpdate, RestingOrder,
};

const BEST_BID: i64 = 2_000_000;
const BEST_ASK: i64 = 2_000_025;
const TICK: i64 = 25;

fn order_level_streams() -> StreamRequirements {
    StreamRequirements::BARS.with(MarketStream::OrderLevels)
}

fn metadata_for(
    instrument: &InstallProviderInstrument,
    session: u64,
    source_sequence: u64,
) -> EventMetadata {
    EventMetadata {
        provider_id: instrument.provider.clone(),
        instrument_id: instrument.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        source_sequence,
        session_generation: session,
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: Some(1_000),
            provider_unix_nanos: Some(1_001),
            received_unix_nanos: 1_002,
        },
    }
}

fn resting(order_id: &str, side: BookSide, price: i64, quantity: i64, priority: u64) -> RestingOrder {
    RestingOrder { order_id: order_id.into(), side, price, quantity, priority }
}

/// Covering image of the fixture book: two bids queued at the best price,
/// published out of priority order, one bid below them and one ask.
fn covering_image(
    instrument: &InstallProviderInstrument,
    session: u64,
    image: u64,
) -> RithmicRealtimeEvent {
    RithmicRealtimeEvent::OrderLevels(session, OrderLevelEvent::Snapshot(OrderLevelSnapshot {
        metadata: metadata_for(instrument, session, image * 1_000),
        image,
        revision: 0,
        provider_sequence: Some(image * 1_000),
        orders: vec![
            resting("bid-late", BookSide::Bid, BEST_BID, 3, 9),
            resting("bid-early", BookSide::Bid, BEST_BID, 2, 4),
            resting("bid-below", BookSide::Bid, BEST_BID - TICK, 5, 1),
            resting("ask-front", BookSide::Ask, BEST_ASK, 4, 2),
        ],
    }))
}

fn update_of(
    instrument: &InstallProviderInstrument,
    image: u64,
    previous_revision: u64,
    changes: Vec<OrderLevelChange>,
) -> RithmicRealtimeEvent {
    let sequence = image * 1_000 + previous_revision + 1;
    RithmicRealtimeEvent::OrderLevels(1, OrderLevelEvent::Update(OrderLevelUpdate {
        metadata: metadata_for(instrument, 1, sequence),
        image,
        previous_revision,
        revision: previous_revision + 1,
        provider_sequence: Some(sequence),
        changes,
    }))
}

fn reset_of(
    instrument: &InstallProviderInstrument,
    image: u64,
    source_sequence: u64,
    reason: OrderLevelInterruption,
    resnapshot: bool,
) -> RithmicRealtimeEvent {
    RithmicRealtimeEvent::OrderLevels(1, OrderLevelEvent::Reset(OrderLevelReset {
        metadata: metadata_for(instrument, 1, source_sequence),
        image,
        reason,
        resnapshot,
    }))
}

fn latest_order_book(events: &[MarketRuntimeEvent]) -> Option<&MarketOrderBookSnapshot> {
    events.iter().rev().find_map(|event| match event {
        MarketRuntimeEvent::OrderBookSnapshot(snapshot) => Some(snapshot),
        _ => None,
    })
}

fn latest_order_levels(boundary: &mut Boundary, consumer_id: ConsumerId) -> MarketOrderLevelView {
    let events = boundary.events_for(consumer_id);
    latest_order_book(&events)
        .expect("an order-book publication")
        .order_levels
        .as_deref()
        .cloned()
        .expect("the consumer demands order levels")
}

fn queue_of(queue: &MarketOrderQueue) -> Vec<(&str, u64)> {
    queue.orders.iter().map(|order| (order.order_id.as_str(), order.priority)).collect()
}

fn latest_subscription(boundary: &Boundary) -> Option<RithmicRealtimeDemand> {
    boundary.provider_requests().into_iter().rev().find_map(|request| match request {
        RithmicRealtimeControl::Subscribe(demand) => Some(demand),
        RithmicRealtimeControl::Stop => None,
    })
}

fn order_level_instruments(demand: &RithmicRealtimeDemand) -> Vec<String> {
    demand.instruments.iter()
        .filter(|instrument| instrument.order_levels)
        .map(|instrument| instrument.instrument.instrument_id.clone())
        .collect()
}

fn series_for(boundary: &Boundary, instrument: &InstallProviderInstrument) -> BarSeriesKey {
    let mut series = boundary.series();
    series.instrument_id.clone_from(&instrument.instrument_id);
    series.entitlement_id.clone_from(&instrument.entitlement_id);
    series
}

#[test]
fn order_level_images_reach_only_demanding_consumers_with_exact_order_identity() {
    let mut boundary = Boundary::new();
    let selected = instrument();
    boundary.demand_with_streams(boundary.series(), 1, order_level_streams());
    let subscription = latest_subscription(&boundary).expect("order-level demand subscribes");
    assert!(matches!(subscription.instruments.as_slice(), [demand]
        if demand.instrument.instrument_id == selected.instrument_id
            && demand.order_levels && !demand.order_book));
    let ladder = consumer(2);
    boundary.register_consumer(ladder);
    boundary.demand_for(
        ladder, boundary.series(), 1, StreamRequirements::BARS.with(MarketStream::Depth),
    );
    let subscription = latest_subscription(&boundary).expect("depth demand resubscribes");
    assert!(
        matches!(subscription.instruments.as_slice(), [demand]
            if demand.order_levels && demand.order_book),
        "aggregate depth is additive to order levels on one instrument subscription"
    );

    boundary.provider_event(covering_image(&selected, 1, 1));
    boundary.provider_event(update_of(&selected, 1, 0, vec![
        OrderLevelChange::Add(resting("bid-new", BookSide::Bid, BEST_BID, 1, 12)),
        OrderLevelChange::Change(resting("ask-front", BookSide::Ask, BEST_ASK, 6, 2)),
        OrderLevelChange::Delete {
            order_id: "bid-below".into(),
            side: BookSide::Bid,
            price: BEST_BID - TICK,
        },
    ]));

    let view = latest_order_levels(&mut boundary, consumer(1));
    assert_eq!(view.status, MarketOrderLevelStatus::Ready);
    assert_eq!(
        (view.provider_generation, view.image, view.revision, view.provider_sequence),
        (1, 1, 1, Some(1_001)),
    );
    assert_eq!(view.recoveries, 0);
    assert_eq!(view.bids.len(), 1, "the deleted order's price is gone");
    assert_eq!(
        (view.bids[0].price, view.bids[0].order_count, view.bids[0].quantity),
        (BEST_BID, 3, 6),
    );
    assert_eq!(
        queue_of(&view.bids[0]),
        [("bid-early", 4), ("bid-late", 9), ("bid-new", 12)],
        "the queue follows provider priority, not publication order"
    );
    assert_eq!(view.asks.len(), 1);
    assert_eq!(view.asks[0].orders, [resting("ask-front", BookSide::Ask, BEST_ASK, 6, 2)]);

    let events = boundary.events_for(ladder);
    let ladder_book = latest_order_book(&events).expect("the depth consumer receives the book");
    assert_eq!(ladder_book.order_levels, None, "a depth-only consumer never receives order levels");
}

#[test]
fn depth_demand_alone_never_subscribes_order_levels() {
    let mut boundary = Boundary::new();
    boundary.demand_with_streams(
        boundary.series(), 1, StreamRequirements::BARS.with(MarketStream::Depth),
    );
    let subscription = latest_subscription(&boundary).expect("depth demand subscribes");
    assert!(matches!(subscription.instruments.as_slice(), [demand]
        if demand.order_book && !demand.order_levels));
    boundary.provider_event(covering_image(&instrument(), 1, 1));
    let events = boundary.events();
    assert!(
        latest_order_book(&events).is_none_or(|book| book.order_levels.is_none()),
        "an unsolicited order-level image is never shown to a depth consumer"
    );
}

#[test]
fn an_order_level_gap_fails_closed_until_the_next_covering_image() {
    let mut boundary = Boundary::new();
    let selected = instrument();
    boundary.demand_with_streams(boundary.series(), 1, order_level_streams());
    assert_eq!(
        latest_order_levels(&mut boundary, consumer(1)).status,
        MarketOrderLevelStatus::AwaitingSnapshot,
    );

    boundary.provider_event(covering_image(&selected, 1, 1));
    boundary.provider_event(update_of(&selected, 1, 0, vec![
        OrderLevelChange::Change(resting("bid-late", BookSide::Bid, BEST_BID, 7, 9)),
    ]));
    boundary.provider_event(update_of(&selected, 1, 2, vec![
        OrderLevelChange::Add(resting("bid-skipped", BookSide::Bid, BEST_BID, 1, 20)),
    ]));
    let view = latest_order_levels(&mut boundary, consumer(1));
    assert_eq!(view.status, MarketOrderLevelStatus::Recovering(OrderLevelInterruption::SequenceGap));
    assert!(view.bids.is_empty() && view.asks.is_empty(), "no partial image is shown");
    assert_eq!(view.recoveries, 1);

    boundary.provider_event(update_of(&selected, 1, 3, vec![
        OrderLevelChange::Add(resting("bid-later", BookSide::Bid, BEST_BID, 1, 21)),
    ]));
    assert!(
        latest_order_book(&boundary.events()).is_none(),
        "updates after a gap cannot rebuild the discarded image"
    );

    boundary.provider_event(covering_image(&selected, 1, 2));
    let view = latest_order_levels(&mut boundary, consumer(1));
    assert_eq!(view.status, MarketOrderLevelStatus::Ready);
    assert_eq!((view.image, view.revision, view.provider_sequence), (2, 0, Some(2_000)));
    assert_eq!(view.recoveries, 1);
    assert_eq!(queue_of(&view.bids[0]), [("bid-early", 4), ("bid-late", 9)]);

    boundary.provider_event(reset_of(&selected, 2, 2_001, OrderLevelInterruption::Overflow, true));
    let view = latest_order_levels(&mut boundary, consumer(1));
    assert_eq!(view.status, MarketOrderLevelStatus::Recovering(OrderLevelInterruption::Overflow));
    assert!(view.bids.is_empty() && view.asks.is_empty());
    assert_eq!(view.recoveries, 2);

    boundary.provider_event(reset_of(
        &selected, 2, 2_002, OrderLevelInterruption::SnapshotRefused, false,
    ));
    let view = latest_order_levels(&mut boundary, consumer(1));
    assert_eq!(
        view.status,
        MarketOrderLevelStatus::Unavailable(OrderLevelInterruption::SnapshotRefused),
    );
    assert_eq!(view.recoveries, 3);
}

#[test]
fn a_retired_provider_session_cannot_keep_or_mutate_the_order_level_image() {
    let mut boundary = Boundary::new();
    let selected = instrument();
    boundary.demand_with_streams(boundary.series(), 1, order_level_streams());
    boundary.provider_event(covering_image(&selected, 1, 1));
    assert_eq!(
        latest_order_levels(&mut boundary, consumer(1)).status,
        MarketOrderLevelStatus::Ready,
    );

    boundary.provider_event(RithmicRealtimeEvent::Connecting(2));
    let view = latest_order_levels(&mut boundary, consumer(1));
    assert_eq!(view.status, MarketOrderLevelStatus::Stale);
    assert!(view.bids.is_empty() && view.asks.is_empty());

    boundary.provider_event(RithmicRealtimeEvent::Connected(2));
    boundary.events();
    boundary.provider_event(covering_image(&selected, 1, 2));
    assert!(
        latest_order_book(&boundary.events()).is_none(),
        "the retired session's image is fenced out"
    );

    boundary.provider_event(covering_image(&selected, 2, 1));
    let view = latest_order_levels(&mut boundary, consumer(1));
    assert_eq!(view.status, MarketOrderLevelStatus::Ready);
    assert_eq!((view.provider_generation, view.image), (2, 1));
}

#[test]
fn order_level_budget_admits_eight_instruments_under_multi_instrument_load() {
    const INSTRUMENTS: u64 = 12;
    const UPDATES: u64 = 80;
    let budget = super::super::super::RITHMIC_DESCRIPTOR
        .capabilities
        .maximum_order_level_instruments;
    assert_eq!(budget, 8);
    let mut boundary = Boundary::new();
    let instruments = (0..INSTRUMENTS).map(|index| {
        let mut listed = instrument();
        listed.instrument_id = format!("instrument:rithmic:CME:OL{index:02}");
        listed.provider_symbol = format!("OL{index:02}");
        listed.entitlement_id = format!("rithmic-test:CME:OL{index:02}");
        listed
    }).collect::<Vec<_>>();
    let consumers = (1..=INSTRUMENTS).map(consumer).collect::<Vec<_>>();
    let mut subscription = None;
    for (listed, consumer_id) in instruments.iter().zip(&consumers) {
        boundary.install(listed.clone());
        if *consumer_id != consumer(1) {
            boundary.register_consumer(*consumer_id);
        }
        let series = series_for(&boundary, listed);
        boundary.demand_for(*consumer_id, series, 1, order_level_streams());
        boundary.discard_history_requests();
        subscription = latest_subscription(&boundary).or(subscription);
    }
    let ids = |range: std::ops::Range<usize>| {
        instruments[range].iter().map(|listed| listed.instrument_id.clone()).collect::<Vec<_>>()
    };
    let subscription = subscription.expect("order-level demand subscribes");
    assert_eq!(subscription.instruments.len(), instruments.len());
    assert_eq!(order_level_instruments(&subscription), ids(0..budget));
    for consumer_id in &consumers[budget..] {
        let view = latest_order_levels(&mut boundary, *consumer_id);
        assert_eq!(view.status, MarketOrderLevelStatus::BudgetExhausted);
        assert!(view.bids.is_empty() && view.asks.is_empty());
    }

    // Admitted streams interleave as one provider session would deliver them.
    for listed in &instruments[..budget] {
        boundary.provider_event(covering_image(listed, 1, 1));
    }
    for revision in 0..UPDATES {
        let depth = i64::try_from(revision).expect("small revision") + 2;
        for listed in &instruments[..budget] {
            boundary.provider_event(update_of(listed, 1, revision, vec![
                OrderLevelChange::Add(resting(
                    &format!("queue-{revision}"), BookSide::Bid, BEST_BID, 1, 100 + revision,
                )),
                OrderLevelChange::Add(resting(
                    &format!("level-{revision}"), BookSide::Bid, BEST_BID - TICK * depth, 1, 1,
                )),
            ]));
        }
    }
    let queued = usize::try_from(UPDATES).expect("small update count") + 2;
    for consumer_id in &consumers[..budget] {
        let view = latest_order_levels(&mut boundary, *consumer_id);
        assert_eq!(view.status, MarketOrderLevelStatus::Ready);
        assert_eq!(
            (view.image, view.revision, view.provider_sequence, view.recoveries),
            (1, UPDATES, Some(1_000 + UPDATES), 0),
        );
        assert_eq!(view.bids.len(), MAXIMUM_ORDER_LEVEL_VIEW_PRICES, "prices are bounded");
        let front = &view.bids[0];
        assert_eq!(front.order_count, queued, "the count covers the whole queue");
        assert_eq!(front.orders.len(), MAXIMUM_ORDER_LEVEL_VIEW_QUEUE, "listed orders are bounded");
        assert_eq!(front.quantity, i64::try_from(UPDATES).expect("small update count") + 5);
        assert_eq!(queue_of(front)[..3], [("bid-early", 4), ("bid-late", 9), ("queue-0", 100)]);
    }

    let released = series_for(&boundary, &instruments[0]);
    boundary.demand_for(consumers[0], released.clone(), 2, StreamRequirements::BARS);
    boundary.discard_history_requests();
    let subscription = latest_subscription(&boundary).expect("released demand resubscribes");
    assert_eq!(
        order_level_instruments(&subscription),
        ids(1..budget + 1),
        "the freed grant goes to the next demanded instrument"
    );
    assert_eq!(
        latest_order_levels(&mut boundary, consumers[budget]).status,
        MarketOrderLevelStatus::AwaitingSnapshot,
    );

    boundary.demand_for(consumers[0], released, 3, order_level_streams());
    boundary.discard_history_requests();
    assert_eq!(
        latest_order_levels(&mut boundary, consumers[0]).status,
        MarketOrderLevelStatus::BudgetExhausted,
        "new demand never evicts a working order-level subscription"
    );
    if let Some(subscription) = latest_subscription(&boundary) {
        assert_eq!(order_level_instruments(&subscription), ids(1..budget + 1));
    }
}
