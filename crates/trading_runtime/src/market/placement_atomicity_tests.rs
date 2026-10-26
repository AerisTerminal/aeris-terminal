use super::*;
use crate::tests::{TestDirectory, config, instrument, market_order, start_service_with_config};
use crate::{OrderSide, OrderStatus, TradingServiceConfig, clock::ManualClock};
use aeris_trading::market::{FeedState, MarketFeedEvent};
use std::sync::{Arc, atomic::AtomicBool, mpsc};

pub(super) fn open_core(configuration: &TradingServiceConfig) -> TradingCore {
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
    core.apply_market_batch(feed::MarketFeedBatch {
        controls: vec![
            MarketFeedEvent::Terms(crate::tests::market_feed::terms()),
            crate::tests::market_feed::live_session(id.clone(), 1),
            crate::tests::market_feed::feed_state(id, 1, FeedState::Live),
        ],
        quotes: vec![crate::tests::market_feed::quote(1, 1, 10_000, 10_025)],
        resync: false,
    });
    assert!(
        core.market_observation_error.is_none(),
        "verified owner quote"
    );
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
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
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
