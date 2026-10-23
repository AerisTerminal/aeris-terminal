use super::*;
use aeris_instruments::{
    ContractDate, InstrumentDecimal, InstrumentMetadataProvenance, SessionHours,
};
use aeris_trading::market::{
    FeedReason, FeedState, InstrumentTerms, MarketFeedEvent, TradingQuote,
};
use std::{fs, path::PathBuf};

#[path = "replay/mod.rs"]
mod replay;

pub(crate) struct TestDirectory(PathBuf);

impl TestDirectory {
    pub(crate) fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock follows epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aeris-trading-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// What the scripted test venue supports.
pub(crate) const VENUE_CAPABILITIES: aeris_trading::venue::VenueCapabilities =
    aeris_trading::venue::VenueCapabilities {
        bracket_levels: aeris_trading::venue::MAXIMUM_BRACKET_LEVELS,
        client_order_id_limit: 50,
    };

pub(crate) fn config(directory: &TestDirectory) -> TradingServiceConfig {
    TradingServiceConfig {
        database_path: directory.0.join("trading.sqlite3"),
        retention: TradingRetention {
            maximum_orders: 4_096,
            maximum_fills: 8,
            maximum_order_events: 16,
            maximum_user_records_per_kind: 4,
        },
        clock: Arc::new(crate::clock::SystemClock::default()),
        execution: ExecutionPolicy::default(),
    }
}

pub(crate) fn start_service(directory: &TestDirectory) -> TradingService {
    start_service_with_config(config(directory))
}

pub(crate) fn start_service_with_config(configuration: TradingServiceConfig) -> TradingService {
    let service = TradingService::start(configuration).expect("service starts");
    if service
        .snapshot()
        .expect("initial snapshot")
        .accounts
        .is_empty()
    {
        service
            .register_account(TradingAccount {
                id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
                display_name: "SIM • Test".to_string(),
                environment: AccountEnvironment::Simulated,
                venue_id: "aeris-sim".to_string(),
                broker_ref: None,
                currency: "USD".to_string(),
                currency_scale: 2,
                starting_equity: Some(FixedPoint::try_new(5_000_000, 2).expect("equity")),
            })
            .expect("default test account registers");
    }
    service
}

mod foundation;
mod group_migration;
mod group_recovery;
pub(crate) mod market_feed;
mod oco;
mod property;
mod publication;

#[test]
fn simulated_currency_mismatch_is_typed_before_an_order_fact() {
    let directory = TestDirectory::new("currency-admission");
    let mut instrument = instrument();
    instrument.contract.currency = "EUR".into();
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument,
        market_feed::route(),
        10_000,
        10_025,
    );
    service.register_instrument(instrument).expect("instrument");
    let mut request = market_order("currency-mismatch", OrderSide::Buy, 1, 1_000);
    request.order_type = OrderType::Limit;
    request.time_in_force = TimeInForce::GoodTillCancelled;
    request.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    let before = service.snapshot().expect("before");
    assert_eq!(
        service.place_order(request).expect_err("currency mismatch"),
        aeris_trading::RejectReason::CurrencyMismatch.to_string()
    );
    assert_eq!(service.snapshot().expect("unchanged"), before);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn unknown_grid_order_is_accepted_with_a_persisted_warning() {
    let directory = TestDirectory::new("unknown-grid-warning");
    let mut instrument = instrument();
    instrument.contract.price_grid = aeris_instruments::PriceGrid::Unknown;
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument,
        market_feed::route(),
        10_000,
        10_025,
    );
    service.register_instrument(instrument).expect("instrument");
    let broker_id = TradingAccountId::try_new("warning-demo").expect("id");
    service
        .register_account(TradingAccount {
            id: broker_id.clone(),
            display_name: "test demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: aeris_trading::venue::BROKER_VENUE_ID.into(),
            broker_ref: Some("fixture-demo".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("broker account");
    let (_generation, _receiver) = service
        .attach_demo_venue(VENUE_CAPABILITIES)
        .expect("venue");
    let mut request = market_order("unknown-grid", OrderSide::Buy, 1, 1_000);
    request.order_type = OrderType::Limit;
    request.time_in_force = TimeInForce::GoodTillCancelled;
    request.limit_price = Some(FixedPoint::try_new(9_013, 2).expect("price"));
    let simulated = service
        .place_order(request.clone())
        .expect("unknown grid accepts");
    request.client_order_id = ClientOrderId::try_new("unknown-grid-broker").expect("id");
    request.account_id = broker_id;
    let broker = service.place_order(request).expect("broker accepts");
    let snapshot = service.snapshot().expect("snapshot");
    for placed in [simulated, broker] {
        let event = snapshot
            .order_events
            .iter()
            .find(|event| event.order_id == placed.id)
            .expect("acceptance event");
        assert_eq!(
            event.reason,
            Some(aeris_trading::OrderEventReason::RiskWarning)
        );
        assert!(
            event
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("price grid is unverified"))
        );
    }
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    assert_eq!(
        restarted.snapshot().expect("restored").order_events,
        snapshot.order_events
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn owner_serializes_eight_submitters_and_recovers_exact_order_revisions() {
    let directory = TestDirectory::new("owner-concurrency");
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_000,
        10_025,
    );
    service
        .register_instrument(instrument())
        .expect("instrument");
    let baseline = service.status().expect("baseline").revision;
    let workers = (0..8)
        .map(|worker| {
            let service = service.clone();
            std::thread::spawn(move || {
                for index in 0..125 {
                    let mut order = market_order(
                        &format!("owner-{worker}-{index}"),
                        OrderSide::Buy,
                        1,
                        1_000_000_000_000,
                    );
                    order.order_type = OrderType::Limit;
                    order.time_in_force = TimeInForce::GoodTillCancelled;
                    order.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
                    loop {
                        match service.place_order(order.clone()) {
                            Ok(accepted) => {
                                assert_eq!(accepted.client_order_id, order.client_order_id);
                                break;
                            }
                            Err(error) if error == "trading command queue is full" => {
                                std::thread::yield_now();
                            }
                            Err(error) => panic!("submit failed: {error}"),
                        }
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().expect("submitter");
    }
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.revision, baseline + 1_000);
    assert_eq!(snapshot.orders.len(), 1_000);
    let mut sequences = snapshot
        .order_events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences.len(), 1_000);
    assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert_eq!(
        snapshot
            .orders
            .iter()
            .map(|order| order.client_order_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        1_000
    );
    service.shutdown(Duration::from_secs(5)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    let recovered = restarted.snapshot().expect("recovered");
    assert_eq!(recovered.revision, snapshot.revision);
    assert_eq!(recovered.orders, snapshot.orders);
    restarted
        .shutdown(Duration::from_secs(5))
        .expect("shutdown");
}

#[test]
fn idle_manual_clock_owner_does_not_run_timer_before_deadline() {
    let directory = TestDirectory::new("owner-idle");
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let service = TradingService::start(configuration).expect("service");
    let diagnostics = || {
        service
            .request(Command::OwnerDiagnostics)
            .expect("owner diagnostics")
    };
    let (name, timer_calls) = diagnostics();
    assert_eq!(name, "aeris-trading-owner");
    for _ in 0..1_000 {
        service.status().expect("status");
    }
    assert_eq!(diagnostics().1, timer_calls);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn cancellation_uses_command_time_even_when_wall_clock_moves_backwards() {
    let directory = TestDirectory::new("cancel-command-time");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    // An open ticket keeps the quote subscribed after the last order is cancelled.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");
    push_live_quote(&feed, &instrument(), 1, 10_000, 10_025, 2);
    service.status().expect("quote drains");
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut order = market_order("clock-limit", OrderSide::Buy, 1, 1_000_000_000_000);
    order.order_type = OrderType::Limit;
    order.time_in_force = TimeInForce::GoodTillCancelled;
    order.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
    let accepted = service.place_order(order).expect("accepted");
    assert_eq!(accepted.accepted_unix_nanos, 1_000_000_000_000);
    assert_eq!(accepted.updated_unix_nanos, 1_000_000_000_000);
    assert_eq!(accepted.eligible_after_unix_nanos, 1_000_000_000_000);
    clock.set_wall_unix_nanos(1_090_000_000_000);
    let cancelled = service
        .cancel_order(ClientOrderId::try_new("clock-limit").expect("id"))
        .expect("cancel");
    assert_eq!(cancelled.revision, 2);
    assert_eq!(cancelled.updated_unix_nanos, 1_090_000_000_000);
    assert_eq!(cancelled.terminal_unix_nanos, Some(1_090_000_000_000));
    assert_eq!(
        cancelled.eligible_after_unix_nanos,
        accepted.eligible_after_unix_nanos
    );
    let first = service.snapshot().expect("snapshot");
    let event = first
        .order_events
        .iter()
        .find(|e| e.kind == OrderEventKind::Cancelled)
        .expect("cancel event");
    assert_eq!(event.event_unix_nanos, 1_090_000_000_000);
    clock.set_wall_unix_nanos(1_002_000_000_000);
    // Command time stays clamped at 1_090s, so admission needs a quote received by then.
    push_live_quote(&feed, &instrument(), 90_000, 10_000, 10_025, 2);
    service.status().expect("fresh quote drains");
    let mut second = market_order("clock-limit-2", OrderSide::Buy, 2, 1_002_000_000_000);
    second.order_type = OrderType::Limit;
    second.time_in_force = TimeInForce::GoodTillCancelled;
    second.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
    service.place_order(second).expect("second accepted");
    let cancelled = service
        .cancel_order(ClientOrderId::try_new("clock-limit-2").expect("id"))
        .expect("second cancel");
    assert_eq!(cancelled.revision, 2);
    assert_eq!(cancelled.updated_unix_nanos, 1_090_000_000_000);
    assert_eq!(cancelled.terminal_unix_nanos, Some(1_090_000_000_000));
    let last = service.snapshot().expect("snapshot");
    let event = last
        .order_events
        .iter()
        .rev()
        .find(|e| e.kind == OrderEventKind::Cancelled)
        .expect("second cancel event");
    assert!(event.event_unix_nanos >= 1_090_000_000_000);
    let mut third = market_order("clock-modify", OrderSide::Buy, 1, 1_002_000_000_000);
    third.order_type = OrderType::Limit;
    third.time_in_force = TimeInForce::GoodTillCancelled;
    third.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
    let accepted = service.place_order(third).expect("third accepted");
    let modified = service
        .modify_order(ModifyOrder {
            client_order_id: accepted.client_order_id.clone(),
            time_in_force: TimeInForce::GoodTillCancelled,
            limit_price: Some(FixedPoint::try_new(8_975, 2).expect("new limit")),
            stop_price: None,
            modified_unix_nanos: 1_002_000_000_000,
            provenance: provenance(3, 1_002_000_000_000),
        })
        .expect("modify");
    assert_eq!(modified.revision, 2);
    assert_eq!(modified.updated_unix_nanos, 1_090_000_000_000);
    assert_eq!(modified.eligible_after_unix_nanos, 1_090_000_000_000);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    let orders = restarted.snapshot().expect("restored").orders;
    assert!(orders.contains(&modified));
    assert!(
        orders
            .iter()
            .filter(|order| order.status == OrderStatus::Cancelled)
            .all(|order| order.terminal_unix_nanos == Some(1_090_000_000_000))
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn stale_modify_and_cancel_return_current_order_without_committing_facts() {
    let directory = TestDirectory::new("stale-order-revision");
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_000,
        10_025,
    );
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut request = market_order("stale-order", OrderSide::Buy, 1, 1_000);
    request.order_type = OrderType::Limit;
    request.time_in_force = TimeInForce::GoodTillCancelled;
    request.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    let accepted = service.place_order(request).expect("accepted");
    let modify = ModifyOrder {
        client_order_id: accepted.client_order_id.clone(),
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(FixedPoint::try_new(9_025, 2).expect("price")),
        stop_price: None,
        modified_unix_nanos: 2_000,
        provenance: provenance(2, 2_000),
    };
    let before = service.snapshot().expect("before stale commands");
    assert_eq!(
        service
            .modify_order_checked(RevisionedModifyOrder {
                order: modify.clone(),
                expected_revision: Some(accepted.revision + 1),
            })
            .expect_err("stale modify"),
        OrderCommandError::StaleRevision {
            current: Box::new(accepted.clone())
        }
    );
    assert_eq!(
        service
            .cancel_order_checked(RevisionedCancelOrder {
                client_order_id: accepted.client_order_id.clone(),
                expected_revision: Some(accepted.revision + 1),
            })
            .expect_err("stale cancel"),
        OrderCommandError::StaleRevision {
            current: Box::new(accepted.clone())
        }
    );
    assert_eq!(service.snapshot().expect("no facts written"), before);
    let modified = service
        .modify_order_checked(RevisionedModifyOrder {
            order: modify,
            expected_revision: Some(accepted.revision),
        })
        .expect("matching revision");
    assert_eq!(modified.revision, accepted.revision + 1);
    assert_eq!(
        modified.eligible_after_unix_nanos,
        modified.updated_unix_nanos
    );
    assert!(modified.eligible_after_unix_nanos >= accepted.eligible_after_unix_nanos);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    let before = restarted.snapshot().expect("restored");
    assert_eq!(
        restarted
            .cancel_order_checked(RevisionedCancelOrder {
                client_order_id: modified.client_order_id.clone(),
                expected_revision: Some(accepted.revision),
            })
            .expect_err("stale after restart"),
        OrderCommandError::StaleRevision {
            current: Box::new(modified.clone())
        }
    );
    assert_eq!(restarted.snapshot().expect("still unchanged"), before);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn stale_revision_after_fill_returns_the_terminal_order_without_a_new_fact() {
    let directory = TestDirectory::new("filled-stale-order");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument");
    let accepted = service
        .place_order(market_order("filled-stale", OrderSide::Buy, 1, 1_000))
        .expect("accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "filled"
    );
    let before = service.snapshot().expect("filled snapshot");
    let current = before
        .orders
        .iter()
        .find(|order| order.id == accepted.id)
        .expect("filled order")
        .clone();
    assert_eq!(current.status, OrderStatus::Filled);
    assert_eq!(current.revision, accepted.revision + 1);
    assert_eq!(
        service
            .cancel_order_checked(RevisionedCancelOrder {
                client_order_id: accepted.client_order_id.clone(),
                expected_revision: Some(accepted.revision),
            })
            .expect_err("stale cancel"),
        OrderCommandError::StaleRevision {
            current: Box::new(current.clone())
        }
    );
    assert_eq!(
        service
            .modify_order_checked(RevisionedModifyOrder {
                order: ModifyOrder {
                    client_order_id: accepted.client_order_id,
                    time_in_force: TimeInForce::GoodTillCancelled,
                    limit_price: Some(FixedPoint::try_new(9_000, 2).expect("price")),
                    stop_price: None,
                    modified_unix_nanos: 3_000,
                    provenance: provenance(3, 3_000),
                },
                expected_revision: Some(accepted.revision),
            })
            .expect_err("stale modify"),
        OrderCommandError::StaleRevision {
            current: Box::new(current)
        }
    );
    assert_eq!(service.snapshot().expect("unchanged"), before);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn client_id_resubmission_is_idempotent_only_for_matching_instructions_after_restart() {
    let directory = TestDirectory::new("client-id-reuse");
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_000,
        10_025,
    );
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut request = market_order("reused-id", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.time_in_force = TimeInForce::GoodTillCancelled;
    request.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
    let original = service
        .place_order(request.clone())
        .expect("first acceptance");
    assert_eq!(original.revision, 1);
    assert!(original.accepted_unix_nanos > 0);
    let baseline = service.snapshot().expect("initial facts");
    assert_eq!(
        service.place_order(request.clone()).expect("retry"),
        original
    );
    assert_eq!(service.snapshot().expect("idempotent facts"), baseline);

    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    assert_eq!(
        restarted.place_order(request.clone()).expect("retry"),
        original
    );
    assert_eq!(restarted.snapshot().expect("restored facts"), baseline);
    let different = [
        PlaceOrder {
            limit_price: Some(FixedPoint::try_new(8_975, 2).expect("different limit")),
            ..request.clone()
        },
        PlaceOrder {
            side: OrderSide::Sell,
            ..request.clone()
        },
        PlaceOrder {
            quantity: FixedPoint::try_new(2, 0).expect("different quantity"),
            ..request.clone()
        },
        PlaceOrder {
            time_in_force: TimeInForce::Day,
            ..request.clone()
        },
    ];
    for altered in different {
        let error = restarted
            .place_order(altered)
            .expect_err("conflicting client id");
        assert!(error.contains("ClientIdReused"), "{error}");
        assert_eq!(restarted.snapshot().expect("unchanged facts"), baseline);
    }
    let cancelled = restarted
        .cancel_order(request.client_order_id.clone())
        .expect("cancel");
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(
        restarted.place_order(request).expect("terminal retry"),
        cancelled
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

pub(crate) fn instrument() -> TradingInstrument {
    TradingInstrument {
        instrument_id: InstrumentId::try_new("instrument:fixture:CME:ESZ6").expect("instrument"),
        price_scale: 2,
        quantity_scale: 0,
        contract: ContractMetadata {
            tick_size: Some(InstrumentDecimal::try_new(25, 2).expect("tick")),
            price_grid: aeris_instruments::PriceGrid::Fixed(
                InstrumentDecimal::try_new(25, 2).expect("tick"),
            ),
            instrument_class: aeris_instruments::InstrumentClass::Future,
            point_value: Some(InstrumentDecimal::try_new(5_000, 2).expect("point value")),
            order_quantity_increment: Some(InstrumentDecimal::try_new(1, 0).expect("lot")),
            currency: "USD".to_string(),
            expiry: Some(ContractDate {
                year: 2026,
                month: 12,
                day: 18,
            }),
            first_notice: None,
            last_trade: Some(ContractDate {
                year: 2026,
                month: 12,
                day: 18,
            }),
            session_hours: vec![SessionHours {
                weekday: 1,
                open_seconds: 18 * 60 * 60,
                close_seconds: 17 * 60 * 60,
                timezone: "America/Chicago".to_string(),
            }],
            provenance: InstrumentMetadataProvenance {
                provider_id: "fixture".to_string(),
                provider_symbol: "ESZ6".to_string(),
                display_symbol: "ESZ6".to_string(),
                session_generation: 1,
            },
        },
    }
}

fn high_precision_crypto_instrument() -> TradingInstrument {
    TradingInstrument {
        instrument_id: InstrumentId::try_new("hyperliquid:perp:BTC").expect("instrument"),
        price_scale: 8,
        quantity_scale: 8,
        contract: ContractMetadata {
            tick_size: Some(InstrumentDecimal::try_new(1_000_000, 8).expect("tick")),
            price_grid: aeris_instruments::PriceGrid::Unknown,
            instrument_class: aeris_instruments::InstrumentClass::CryptoPerpetual,
            point_value: Some(InstrumentDecimal::try_new(100_000_000, 8).expect("point value")),
            order_quantity_increment: None,
            currency: "USD".to_string(),
            expiry: None,
            first_notice: None,
            last_trade: None,
            session_hours: Vec::new(),
            provenance: InstrumentMetadataProvenance {
                provider_id: "hyperliquid".to_string(),
                provider_symbol: "BTC".to_string(),
                display_symbol: "BTC".to_string(),
                session_generation: 1,
            },
        },
    }
}

#[test]
fn significant_figure_market_quotes_do_not_require_a_fixed_tick() {
    let directory = TestDirectory::new("significant-figure-quote");
    let mut crypto = high_precision_crypto_instrument();
    crypto.contract.tick_size = None;
    crypto.contract.price_grid = aeris_instruments::PriceGrid::SignificantFigures {
        figures: 5,
        max_decimals: 6,
    };
    let (service, _clock, feed) = live_feed_service(&directory);
    let route = aeris_trading::market::MarketRoute {
        provider_id: "fixture".into(),
        instrument_id: crypto.instrument_id.clone(),
        provider_symbol: "BTC".into(),
        display_symbol: "BTC".into(),
        venue_id: "HYPERLIQUID".into(),
        entitlement_id: None,
        price_scale: 8,
        quantity_scale: 8,
    };
    service
        .register_instrument(crypto.clone())
        .expect("terms register");
    bootstrap_live_feed(&feed, &crypto, route);
    service.status().expect("verified terms drain");
    assert_eq!(
        instrument_tick_units(
            &crypto,
            FixedPoint::try_new(8_210_000_000_000, 8).expect("mark"),
        ),
        Ok(100_000_000)
    );
    // A quote received exactly at the order's acceptance time is a mark, not a fill.
    service
        .place_order(PlaceOrder {
            client_order_id: ClientOrderId::try_new("sigfig-market-buy").expect("client"),
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            instrument_id: crypto.instrument_id.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::Day,
            quantity: FixedPoint::try_new(100_000_000, 8).expect("one unit"),
            limit_price: None,
            stop_price: None,
            submitted_unix_nanos: 2_000,
            provenance: provenance(2, 2_000),
        })
        .expect("market order accepted");
    assert_eq!(
        deliver_quote_at(
            &service,
            &feed,
            &crypto,
            1,
            8_209_000_000_000,
            8_210_000_000_000,
            1_000_000_000_000,
        ),
        []
    );
    assert_eq!(
        deliver_quote(
            &service,
            &feed,
            &crypto,
            3,
            8_209_000_000_000,
            8_210_000_000_000
        )
        .len(),
        1
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn chart_bracket_offsets_use_verified_significant_figure_grid() {
    let mut crypto = high_precision_crypto_instrument();
    crypto.contract.tick_size = None;
    crypto.contract.price_grid = aeris_instruments::PriceGrid::SignificantFigures {
        figures: 5,
        max_decimals: 6,
    };
    let entry = FixedPoint::try_new(8_210_000_000_000, 8).expect("entry");
    let stop = FixedPoint::try_new(8_209_700_000_000, 8).expect("stop");
    let target = FixedPoint::try_new(8_210_400_000_000, 8).expect("target");
    let template = chart_bracket_template(&crypto, OrderSide::Buy, entry, stop, target, "chart");
    let template = template.expect("owner uses the verified one-dollar grid");
    assert_eq!(template.stop_offset_ticks, 3);
    assert_eq!(template.targets[0].offset_ticks, 4);
    assert!(chart_bracket_template(&crypto, OrderSide::Buy, entry, target, stop, "bad").is_err());
}

pub(crate) fn provenance(sequence: u64, time: i64) -> TradingProvenance {
    TradingProvenance {
        venue_id: "aeris-sim".to_string(),
        provider_id: "fixture".to_string(),
        session_generation: 1,
        source_sequence: sequence,
        observed_unix_nanos: time,
    }
}

pub(crate) fn market_order(
    client_id: &str,
    side: OrderSide,
    sequence: u64,
    time: i64,
) -> PlaceOrder {
    PlaceOrder {
        client_order_id: ClientOrderId::try_new(client_id).expect("client id"),
        account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
        instrument_id: instrument().instrument_id,
        side,
        order_type: OrderType::Market,
        time_in_force: TimeInForce::Day,
        quantity: FixedPoint::try_new(1, 0).expect("quantity"),
        limit_price: None,
        stop_price: None,
        submitted_unix_nanos: time,
        provenance: provenance(sequence, time),
    }
}

pub(crate) fn observation(
    bid: i64,
    ask: i64,
    sequence: u64,
    time: i64,
) -> SimulatedMarketObservation {
    SimulatedMarketObservation {
        instrument_id: instrument().instrument_id,
        bid: FixedPoint::try_new(bid, 2).expect("bid"),
        ask: FixedPoint::try_new(ask, 2).expect("ask"),
        provenance: provenance(sequence, time),
    }
}

/// Starts a service on a fixed clock and returns its market-feed inbox; emergency commands
/// price from the owner's verified quote book, so tests that flatten or reverse must deliver
/// quotes through the real feed path instead of the direct-observation shortcut.
pub(crate) fn live_feed_service(
    directory: &TestDirectory,
) -> (
    TradingService,
    std::sync::Arc<crate::clock::ManualClock>,
    std::sync::Arc<crate::core::market::feed::MarketFeedInbox>,
) {
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(directory);
    configuration.clock = clock.clone();
    let service = start_service_with_config(configuration);
    let feed = service
        .attach_market_feed(std::sync::Arc::new(market_feed::RecordedInterest::default()))
        .expect("feed");
    (service, clock, feed)
}

/// Registers the verified route, an open session and the Live feed state for one registered
/// instrument, mirroring what the market runtime delivers for a subscribed instrument.
pub(crate) fn bootstrap_live_feed(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    route: aeris_trading::market::MarketRoute,
) {
    bootstrap_feed_with(
        feed,
        instrument,
        route,
        market_feed::live_session(instrument.instrument_id.clone(), 1),
    );
}

/// The same verified route and Live feed state with a session that never closes, for
/// fixtures whose deterministic clock sits beyond the fixed-epoch session end.
pub(crate) fn bootstrap_always_open_feed(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    route: aeris_trading::market::MarketRoute,
) {
    bootstrap_feed_with(
        feed,
        instrument,
        route,
        market_feed::always_open_session(instrument.instrument_id.clone(), 1),
    );
}

fn bootstrap_feed_with(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    route: aeris_trading::market::MarketRoute,
    session: MarketFeedEvent,
) {
    feed.push(MarketFeedEvent::Terms(InstrumentTerms {
        route,
        instrument_class: instrument.contract.instrument_class,
        price_grid: instrument.contract.price_grid,
        point_value: instrument.contract.point_value,
        quantity_increment: instrument.contract.order_quantity_increment,
        currency: instrument.contract.currency.clone(),
        expiry: instrument.contract.expiry,
        first_notice: instrument.contract.first_notice,
        last_trade: instrument.contract.last_trade,
        session_hours: instrument.contract.session_hours.clone(),
        session_generation: 1,
    }))
    .expect("terms");
    feed.push(session).expect("session");
    feed.push(market_feed::feed_state(
        instrument.instrument_id.clone(),
        1,
        FeedState::Live,
    ))
    .expect("live");
}

/// A provider resume drops the retained quote and returns the feed to Live, so a resting
/// market order waits for the next quote instead of filling at its placement.
pub(crate) fn resume_live_without_quote(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
) {
    feed.push(market_feed::feed_state(
        instrument.instrument_id.clone(),
        1,
        FeedState::Unavailable {
            reason: FeedReason::Recovering,
        },
    ))
    .expect("recovering");
    feed.push(market_feed::feed_state(
        instrument.instrument_id.clone(),
        1,
        FeedState::Live,
    ))
    .expect("live");
}

/// Winning round trips on the deterministic clock: each quote is delivered before its
/// placement, so the market order fills at its touch and the account returns to flat.
fn winning_round_trips(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    clock: &crate::clock::ManualClock,
    sequence: &mut u64,
    iterations: usize,
    label_prefix: &str,
) {
    use crate::clock::Clock;
    for _ in 0..iterations {
        for (side, bid, ask) in [
            (OrderSide::Buy, 9_999, 10_000),
            (OrderSide::Sell, 10_010, 10_011),
        ] {
            clock.advance(Duration::from_secs(10));
            let now = clock.wall_unix_nanos();
            *sequence += 1;
            let label = format!("{label_prefix}-{sequence}");
            deliver_quote_at(service, feed, &instrument(), *sequence, bid, ask, now);
            service
                .place_order(market_order(&label, side, *sequence, now))
                .expect("round-trip order accepted");
        }
    }
}

/// Pushes one Live quote through the feed; the timestamp stays within the freshness window of
/// the fixed `1_000_000_000_000` clock used by [`live_feed_service`].
pub(crate) fn push_live_quote(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    sequence: u64,
    bid: i64,
    ask: i64,
    price_scale: u8,
) {
    push_live_quote_at(
        feed,
        instrument,
        sequence,
        bid,
        ask,
        price_scale,
        1_000_000_000_000 + i64::try_from(sequence).expect("sequence") * 1_000_000,
    );
}

/// Pushes one Live quote received at an explicit owner-clock time.
pub(crate) fn push_live_quote_at(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    sequence: u64,
    bid: i64,
    ask: i64,
    price_scale: u8,
    received_unix_nanos: i64,
) {
    feed.push(MarketFeedEvent::Quote(TradingQuote {
        instrument_id: instrument.instrument_id.clone(),
        bid: FixedPoint::try_new(bid, price_scale).expect("bid"),
        ask: FixedPoint::try_new(ask, price_scale).expect("ask"),
        bid_size: None,
        ask_size: None,
        last: None,
        provider_id: "fixture".to_string(),
        provider_generation: 1,
        source_sequence: Some(sequence),
        exchange_unix_nanos: None,
        received_unix_nanos,
    }))
    .expect("quote");
}

/// Starts the common deterministic service fixture with verified Terms, an open session,
/// Live feed state, and a current quote installed by the trading owner's market inbox.
pub(crate) fn routed_fixture(
    directory: &TestDirectory,
    instrument: &TradingInstrument,
    route: aeris_trading::market::MarketRoute,
    bid: i64,
    ask: i64,
) -> (TradingService, Arc<crate::clock::ManualClock>) {
    let (service, clock, _feed) = quoted_fixture(directory, instrument, route, bid, ask);
    (service, clock)
}

/// [`routed_fixture`] that also returns the owner inbox for later quotes.
pub(crate) fn quoted_fixture(
    directory: &TestDirectory,
    instrument: &TradingInstrument,
    route: aeris_trading::market::MarketRoute,
    bid: i64,
    ask: i64,
) -> (
    TradingService,
    Arc<crate::clock::ManualClock>,
    Arc<crate::core::market::feed::MarketFeedInbox>,
) {
    let (service, clock, feed) = awaiting_quote_fixture(directory, instrument, route);
    push_live_quote(&feed, instrument, 1, bid, ask, instrument.price_scale);
    service
        .status()
        .expect("owner drains verified fixture feed");
    (service, clock, feed)
}

/// Starts the deterministic service fixture with verified Terms, an open session and Live feed
/// state but no quote yet, so a market entry stays Working until the owner's next quote.
pub(crate) fn awaiting_quote_fixture(
    directory: &TestDirectory,
    instrument: &TradingInstrument,
    route: aeris_trading::market::MarketRoute,
) -> (
    TradingService,
    Arc<crate::clock::ManualClock>,
    Arc<crate::core::market::feed::MarketFeedInbox>,
) {
    let (service, clock, feed) = live_feed_service(directory);
    bootstrap_live_feed(&feed, instrument, route);
    service
        .status()
        .expect("owner drains verified fixture feed");
    (service, clock, feed)
}

/// Reopens the fixture store on the same clock; the market runtime republishes the session
/// and Live state for the persisted route, but no quote has arrived yet.
pub(crate) fn reopen_awaiting_quote(
    directory: &TestDirectory,
    clock: &Arc<crate::clock::ManualClock>,
    instrument: &TradingInstrument,
    route: aeris_trading::market::MarketRoute,
) -> (
    TradingService,
    Arc<crate::core::market::feed::MarketFeedInbox>,
) {
    let mut configuration = config(directory);
    configuration.clock = clock.clone();
    let service = start_service_with_config(configuration);
    let feed = service
        .attach_market_feed(Arc::new(market_feed::RecordedInterest::default()))
        .expect("feed");
    bootstrap_live_feed(&feed, instrument, route);
    service.status().expect("owner drains reopened feed");
    (service, feed)
}

/// Delivers one Live quote through the owner inbox and returns the fills it executed, in
/// execution order.
pub(crate) fn deliver_quote(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    sequence: u64,
    bid: i64,
    ask: i64,
) -> Vec<Fill> {
    deliver_quote_at(
        service,
        feed,
        instrument,
        sequence,
        bid,
        ask,
        1_000_000_000_000 + i64::try_from(sequence).expect("sequence") * 1_000_000,
    )
}

/// Delivers one Live quote received at an explicit owner-clock time and returns its fills.
pub(crate) fn deliver_quote_at(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    sequence: u64,
    bid: i64,
    ask: i64,
    received_unix_nanos: i64,
) -> Vec<Fill> {
    let before = service.snapshot().expect("snapshot before quote").fills;
    push_live_quote_at(
        feed,
        instrument,
        sequence,
        bid,
        ask,
        instrument.price_scale,
        received_unix_nanos,
    );
    service.status().expect("owner applies quote");
    // Snapshots list the newest fill first; report the quote's fills in execution order.
    let mut fills = service
        .snapshot()
        .expect("snapshot after quote")
        .fills
        .into_iter()
        .filter(|fill| !before.contains(fill))
        .collect::<Vec<_>>();
    fills.reverse();
    fills
}

#[test]
fn snapshot_projects_registered_instruments_with_provider_symbols() {
    let directory = TestDirectory::new("snapshot-instruments");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.instruments, vec![instrument()]);
    assert_eq!(
        snapshot.instruments[0].contract.provenance.provider_symbol,
        "ESZ6"
    );
}

#[test]
fn whole_unit_contract_rejects_fractional_orders_at_market_data_scale() {
    let directory = TestDirectory::new("whole-unit-order");
    let mut contract = instrument();
    contract.quantity_scale = 8;
    contract.contract.order_quantity_increment =
        Some(InstrumentDecimal::try_new(100_000_000, 8).expect("one contract"));
    let mut route = market_feed::route();
    route.quantity_scale = 8;
    let (service, _clock) = routed_fixture(&directory, &contract, route, 10_000, 10_025);
    service
        .register_instrument(contract)
        .expect("whole-unit instrument registers");
    let mut order = market_order("fractional-contract", OrderSide::Buy, 1, 1_000);
    order.quantity = FixedPoint::try_new(50_000_000, 8).expect("half contract");
    assert_eq!(
        service.place_order(order.clone()).unwrap_err(),
        "order quantity must use whole provider units"
    );
    order.client_order_id = ClientOrderId::try_new("whole-contract").expect("client id");
    order.quantity = FixedPoint::try_new(100_000_000, 8).expect("one contract");
    service.place_order(order).expect("whole contract accepted");
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let reopened = start_service(&directory);
    let mut fractional = market_order("fractional-after-restart", OrderSide::Buy, 2, 2_000);
    fractional.quantity = FixedPoint::try_new(50_000_000, 8).expect("half contract");
    assert_eq!(
        reopened.place_order(fractional).unwrap_err(),
        "order quantity must use whole provider units"
    );
}

#[test]
fn refreshed_contract_terms_keep_one_instrument_and_an_open_position() {
    let directory = TestDirectory::new("metadata-refresh-position");
    let first = instrument();
    let (service, _clock, feed) = awaiting_quote_fixture(&directory, &first, market_feed::route());
    service
        .register_instrument(first.clone())
        .expect("initial registration");
    service
        .place_order(market_order("before-refresh", OrderSide::Buy, 1, 1_000))
        .expect("entry accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &first, 2, 9_999, 10_000).len(),
        1,
        "entry fills"
    );

    let mut refreshed = first.clone();
    refreshed.contract.provenance.session_generation = 2;
    service
        .register_instrument(refreshed.clone())
        .expect("metadata re-registers");
    let snapshot = service.snapshot().expect("refreshed snapshot");
    assert_eq!(snapshot.instruments, vec![refreshed]);
    assert_eq!(snapshot.positions.len(), 1);
    assert_eq!(snapshot.positions[0].net_quantity.units(), 1);
}

#[test]
fn practice_order_rejects_contract_with_another_currency() {
    let directory = TestDirectory::new("contract-currency");
    let mut non_usd = instrument();
    non_usd.contract.currency = "EUR".into();
    let (service, _clock, _feed) =
        awaiting_quote_fixture(&directory, &non_usd, market_feed::route());
    service
        .register_instrument(non_usd)
        .expect("source currency remains registered");
    assert_eq!(
        service
            .place_order(market_order("eur-against-usd", OrderSide::Buy, 1, 1_000))
            .unwrap_err(),
        "Practice contract currency differs from the account currency"
    );
}

#[test]
fn future_and_equity_practice_round_trips_use_their_point_values() {
    let directory = TestDirectory::new("contract-pnl");
    let future = instrument();
    let (service, _clock, feed) = awaiting_quote_fixture(&directory, &future, market_feed::route());
    assert_eq!(
        tick_value(&future, 2).expect("ES tick value"),
        FixedPoint::try_new(1_250, 2).expect("$12.50")
    );
    service
        .register_instrument(future.clone())
        .expect("ES registers");
    let mut equity = future.clone();
    equity.instrument_id = InstrumentId::try_new("fixture:Equity:AAPL").expect("equity id");
    equity.contract.point_value = Some(InstrumentDecimal::try_new(1, 0).expect("one per share"));
    equity.contract.tick_size = Some(InstrumentDecimal::try_new(1, 2).expect("cent"));
    equity.contract.provenance.display_symbol = "AAPL".into();
    service
        .register_instrument(equity.clone())
        .expect("AAPL registers");
    let mut equity_route = market_feed::route();
    equity_route.instrument_id = equity.instrument_id.clone();
    equity_route.provider_symbol = "AAPL".into();
    equity_route.display_symbol = "AAPL".into();
    // Open tickets keep both quotes subscribed while the other instrument trades.
    service
        .set_ticket_interest(vec![market_feed::route(), equity_route.clone()])
        .expect("ticket interest");
    bootstrap_live_feed(&feed, &equity, equity_route);
    service.status().expect("AAPL feed drains");

    let es_buy = market_order("es-buy", OrderSide::Buy, 1, 1_000);
    service.place_order(es_buy).expect("ES buy");
    assert_eq!(
        deliver_quote(&service, &feed, &future, 2, 9_999, 10_000).len(),
        1,
        "ES entry fill"
    );
    // The exit fills at placement against the owner's current quote.
    deliver_quote(&service, &feed, &future, 4, 10_400, 10_401);
    let es_exit = service
        .place_order(market_order("es-sell", OrderSide::Sell, 3, 3_000))
        .expect("ES sell");
    assert_eq!(es_exit.status, OrderStatus::Filled, "ES exit fill");

    let mut stock_buy = market_order("aapl-buy", OrderSide::Buy, 1, 1_000);
    stock_buy.instrument_id = equity.instrument_id.clone();
    stock_buy.quantity = FixedPoint::try_new(10, 0).expect("ten shares");
    service.place_order(stock_buy).expect("AAPL buy");
    assert_eq!(
        deliver_quote(&service, &feed, &equity, 2, 33_099, 33_100).len(),
        1,
        "AAPL entry fill"
    );
    deliver_quote(&service, &feed, &equity, 4, 33_200, 33_201);
    let mut stock_sell = market_order("aapl-sell", OrderSide::Sell, 3, 3_000);
    stock_sell.instrument_id = equity.instrument_id.clone();
    stock_sell.quantity = FixedPoint::try_new(10, 0).expect("ten shares");
    let stock_exit = service.place_order(stock_sell).expect("AAPL sell");
    assert_eq!(stock_exit.status, OrderStatus::Filled, "AAPL exit fill");

    let snapshot = service.snapshot().expect("round-trip snapshot");
    let realized = |id: &InstrumentId| {
        snapshot
            .positions
            .iter()
            .find(|position| &position.instrument_id == id)
            .expect("closed position")
            .realized_pnl
    };
    assert_eq!(realized(&future.instrument_id).units(), 20_000);
    assert_eq!(realized(&equity.instrument_id).units(), 1_000);
}

#[test]
fn practice_accounts_are_user_created_with_durable_opening_equity() {
    let directory = TestDirectory::new("practice-account");
    let service = TradingService::start(config(&directory)).expect("service starts");
    assert_eq!(
        service.snapshot().expect("empty snapshot").accounts,
        [] as [aeris_trading::TradingAccount; 0]
    );
    let created = service
        .create_practice_account(CreatePracticeAccount {
            display_name: "Evaluation".to_string(),
            starting_equity: FixedPoint::try_new(10_000_000, 2).expect("equity"),
        })
        .expect("account creates");
    assert_eq!(created.display_name, "SIM • Evaluation");
    let pnl = &service.snapshot().expect("snapshot").account_pnl[0];
    assert_eq!(pnl.balance, created.starting_equity);
    assert_eq!(pnl.equity, created.starting_equity);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let reopened = TradingService::start(config(&directory)).expect("service reopens");
    assert_eq!(
        reopened.snapshot().expect("restored snapshot").accounts[0],
        created
    );
    reopened
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn high_precision_crypto_pnl_updates_live_and_closes_without_overflow() {
    let directory = TestDirectory::new("crypto-pnl");
    let (service, _clock, feed) = live_feed_service(&directory);
    let instrument = high_precision_crypto_instrument();
    service
        .register_instrument(instrument.clone())
        .expect("crypto instrument registers");
    bootstrap_live_feed(
        &feed,
        &instrument,
        aeris_trading::market::MarketRoute {
            provider_id: "fixture".into(),
            instrument_id: instrument.instrument_id.clone(),
            provider_symbol: "BTC".into(),
            display_symbol: "BTC".into(),
            venue_id: "HYPERLIQUID".into(),
            entitlement_id: None,
            price_scale: 8,
            quantity_scale: 8,
        },
    );
    service.status().expect("feed drains");
    let account_id = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let order = PlaceOrder {
        client_order_id: ClientOrderId::try_new("btc-entry").expect("client id"),
        account_id: account_id.clone(),
        instrument_id: instrument.instrument_id.clone(),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        time_in_force: TimeInForce::Day,
        quantity: FixedPoint::try_new(1_500_000_000, 8).expect("15 BTC"),
        limit_price: None,
        stop_price: None,
        submitted_unix_nanos: 1_000,
        provenance: TradingProvenance {
            venue_id: "aeris-sim".to_string(),
            provider_id: "hyperliquid".to_string(),
            session_generation: 1,
            source_sequence: 1,
            observed_unix_nanos: 1_000,
        },
    };
    service.place_order(order.clone()).expect("entry accepted");

    push_live_quote(
        &feed,
        &instrument,
        2,
        8_399_900_000_000,
        8_400_000_000_000,
        8,
    );
    service.status().expect("entry fills");
    push_live_quote(
        &feed,
        &instrument,
        3,
        8_410_000_000_000,
        8_410_100_000_000,
        8,
    );
    service
        .status()
        .expect("live mark updates without overflow");
    let open = service.snapshot().expect("open snapshot");
    let position = open
        .positions
        .iter()
        .find(|position| position.instrument_id == instrument.instrument_id)
        .expect("open BTC position");
    assert_eq!(
        position.unrealized_pnl,
        FixedPoint::try_new(150_000, 2).expect("$1500 unrealized")
    );

    service
        .flatten(FlattenScope {
            account: Some(account_id),
            instrument: None,
        })
        .expect("close succeeds without overflow");
    let closed = service.snapshot().expect("closed snapshot");
    let position = closed
        .positions
        .iter()
        .find(|position| position.instrument_id == instrument.instrument_id)
        .expect("closed BTC position retained");
    assert_eq!(position.net_quantity.units(), 0);
    assert_eq!(
        position.realized_pnl,
        FixedPoint::try_new(150_000, 2).expect("$1500 realized")
    );
    assert_eq!(position.unrealized_pnl.units(), 0);
}

#[test]
fn weighted_average_entry_never_disappears_when_the_exact_ratio_repeats() {
    let average = weighted_average_entry_price(
        FixedPoint::try_new(8_402_000_000_000, 8).expect("first price"),
        300_000_000,
        FixedPoint::try_new(8_440_200_000_000, 8).expect("second price"),
        400_000_000,
    )
    .expect("weighted average");
    assert_eq!(
        average,
        FixedPoint::try_new(8_423_828_571_429, 8).expect("rounded average")
    );
}

#[test]
fn scale_in_realized_pnl_uses_the_exact_cost_basis_across_a_restart() {
    // VAL-TRD-054: buying 1 @ 100.01 and 2 @ 100.02 then selling 3 @ 100.03 realizes
    // exactly 0.04 points — 2.00 USD at the fixture's 50 USD point value — where the
    // rounded display average of 100.02 would have realized only 1.50 USD.
    let directory = TestDirectory::new("exact-cost-basis");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");

    let first = market_order("basis-first", OrderSide::Buy, 1, 1_000);
    service.place_order(first).expect("first order accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 1, 10_000, 10_001).len(),
        1,
        "first fill"
    );
    // The scale-in quote is delivered before the placement, so the market order fills at
    // its touch.
    deliver_quote(&service, &feed, &instrument(), 2, 10_001, 10_002);
    let mut second = market_order("basis-second", OrderSide::Buy, 2, 2_000);
    second.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    service.place_order(second).expect("second order accepted");
    let before = service.snapshot().expect("snapshot");
    let open = before
        .positions
        .iter()
        .find(|position| position.instrument_id == instrument().instrument_id)
        .expect("open position");
    assert_eq!(open.net_quantity.units(), 3);
    assert_eq!(
        open.average_entry_price,
        Some(FixedPoint::try_new(10_002, 2).expect("display average"))
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let (service, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    let mut exit = market_order("basis-exit", OrderSide::Sell, 3, 3_000);
    exit.quantity = FixedPoint::try_new(3, 0).expect("quantity");
    service.place_order(exit).expect("exit accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 3, 10_003, 10_004).len(),
        1,
        "exit fill"
    );
    let after = service.snapshot().expect("closed snapshot");
    let closed = after
        .positions
        .iter()
        .find(|position| position.instrument_id == instrument().instrument_id)
        .expect("closed position");
    assert_eq!(closed.net_quantity.units(), 0);
    assert_eq!(
        closed.realized_pnl,
        FixedPoint::try_new(200, 2).expect("exact realized")
    );
}
#[test]
fn fee_schedule_charges_maker_and_taker_fills_per_the_model() {
    // VAL-TRD-055: the taker fill pays taker basis points of notional and the resting-limit
    // fill pays maker basis points, each evaluated exactly and rounded once at the account
    // currency scale.
    let directory = TestDirectory::new("fee-maker-taker");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut schedule = aeris_trading::FeeSchedule::default();
    schedule.by_class.insert(
        aeris_instruments::InstrumentClass::Future,
        aeris_trading::FeeModel::NotionalBps {
            maker_bps: 1,
            taker_bps: 2,
        },
    );
    service
        .set_fee_schedule(account, schedule)
        .expect("fee schedule installs");

    // Taker: the market order fills the touch at 100.01; 2 bps of 5_000.50 rounds to 1.00.
    service
        .place_order(market_order("fee-taker", OrderSide::Buy, 1, 1_000))
        .expect("taker accepted");
    let taker_fills = deliver_quote(&service, &feed, &instrument(), 1, 10_000, 10_001);
    assert_eq!(taker_fills.len(), 1);

    // Maker: the resting limit fills when the ask drops to 99.75; 1 bps of 4_987.50 rounds
    // to 0.50.
    let mut resting = market_order("fee-maker", OrderSide::Buy, 2, 2_000);
    resting.order_type = OrderType::Limit;
    resting.limit_price = Some(FixedPoint::try_new(9_975, 2).expect("limit"));
    service.place_order(resting).expect("resting accepted");
    let maker_fills = deliver_quote(&service, &feed, &instrument(), 3, 9_950, 9_975);
    assert_eq!(maker_fills.len(), 1);

    let snapshot = service.snapshot().expect("snapshot");
    let commissions = snapshot
        .ledger_entries
        .iter()
        .filter(|entry| entry.kind == aeris_trading::LedgerEntryKind::Commission)
        .map(|entry| entry.amount.units())
        .collect::<Vec<_>>();
    assert_eq!(commissions, [-100, -50]);
    assert_eq!(
        snapshot.account_pnl[0].balance,
        Some(FixedPoint::try_new(4_999_850, 2).expect("balance"))
    );

    // The exact fee persists on each fill's durable accounting row.
    let database = rusqlite::Connection::open(config(&directory).database_path).expect("reader");
    for (fill, expected) in [(&taker_fills[0], 100), (&maker_fills[0], 50)] {
        let commission: i64 = database
            .query_row(
                "SELECT commission_units FROM fill_accounting WHERE fill_id = ?1",
                [fill.id.as_str()],
                |row| row.get(0),
            )
            .expect("durable commission");
        assert_eq!(commission, expected);
    }
}
/// Asserts the checkpoint fold and balance identity of one account after a retention run.
fn assert_checkpoint_rows(
    directory: &TestDirectory,
    checkpointed_fills: i64,
    remaining_fills: i64,
    checkpointed_entries: i64,
    remaining_entries: i64,
    balance_units: i64,
) {
    let database = rusqlite::Connection::open(config(directory).database_path).expect("reader");
    let folded_fills: i64 = database
        .query_row(
            "SELECT fill_count FROM position_checkpoints WHERE account_id = 'aeris-sim-1'",
            [],
            |row| row.get(0),
        )
        .expect("position checkpoint row");
    assert_eq!(
        folded_fills, checkpointed_fills,
        "the pruned prefix folded once"
    );
    let fills: i64 = database
        .query_row("SELECT count(*) FROM fills", [], |row| row.get(0))
        .expect("fill count");
    assert_eq!(fills, remaining_fills);
    let folded_entries: i64 = database
        .query_row(
            "SELECT entry_count FROM ledger_checkpoints WHERE account_id = 'aeris-sim-1'",
            [],
            |row| row.get(0),
        )
        .expect("ledger checkpoint row");
    let entries: i64 = database
        .query_row("SELECT count(*) FROM ledger_entries", [], |row| row.get(0))
        .expect("entry count");
    assert_eq!(folded_entries, checkpointed_entries);
    assert_eq!(entries, remaining_entries);
    let checkpoint_balance: i64 = database
        .query_row(
            "SELECT balance_units FROM ledger_checkpoints WHERE account_id = 'aeris-sim-1'",
            [],
            |row| row.get(0),
        )
        .expect("checkpoint balance");
    let entries_sum: i64 = database
        .query_row(
            "SELECT COALESCE(SUM(amount_units), 0) FROM ledger_entries",
            [],
            |row| row.get(0),
        )
        .expect("entries sum");
    assert_eq!(
        checkpoint_balance + entries_sum,
        balance_units,
        "checkpoint plus the remaining entries is the exact balance"
    );
}

#[test]
fn retention_writes_checkpoints_and_recovers_the_exact_projection() {
    // VAL-TRD-059: the retention run folds pruned fills and their ledger entries into
    // checkpoints in one transaction; after a restart every position equals its checkpoint
    // plus the retained suffix and the balance is unchanged.
    use crate::clock::Clock;
    let directory = TestDirectory::new("retention-checkpoint");
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    configuration.retention.maximum_fills = 4;
    let service = start_service_with_config(configuration);
    let feed = service
        .attach_market_feed(Arc::new(market_feed::RecordedInterest::default()))
        .expect("feed");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service
        .status()
        .expect("owner drains verified fixture feed");
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut schedule = aeris_trading::FeeSchedule::default();
    schedule.by_class.insert(
        aeris_instruments::InstrumentClass::Future,
        aeris_trading::FeeModel::NotionalBps {
            maker_bps: 1,
            taker_bps: 2,
        },
    );
    service
        .set_fee_schedule(account, schedule)
        .expect("fee schedule installs");
    // An open ticket keeps the quote subscribed while the position is flat between fills.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");

    // Every round trip wins (+25.00 gross before fees), so no discipline lock engages.
    let mut sequence = 1_u64;
    for _ in 0..6 {
        for (side, bid, ask) in [
            (OrderSide::Buy, 9_999, 10_000),
            (OrderSide::Sell, 10_050, 10_051),
        ] {
            clock.advance(Duration::from_secs(10));
            let now = clock.wall_unix_nanos();
            sequence += 1;
            let label = format!("rt-{sequence}");
            // The quote is delivered before the placement, so the market order fills at its
            // touch.
            deliver_quote_at(&service, &feed, &instrument(), sequence, bid, ask, now);
            service
                .place_order(market_order(&label, side, sequence, now))
                .expect("round-trip order accepted");
        }
    }
    // One open long 2 stays behind the round trips.
    clock.advance(Duration::from_secs(10));
    let now = clock.wall_unix_nanos();
    sequence += 1;
    let mut open_buy = market_order("open-buy", OrderSide::Buy, sequence, now);
    open_buy.time_in_force = TimeInForce::GoodTillCancelled;
    open_buy.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    deliver_quote_at(
        &service,
        &feed,
        &instrument(),
        sequence,
        10_000,
        10_001,
        now,
    );
    service.place_order(open_buy).expect("open buy accepted");

    let before = service.snapshot().expect("pre-retention snapshot");
    // Owner memory mirrors the retention window; the durable store holds all 13 fills.
    assert_eq!(before.fills.len(), 4);
    assert_eq!(before.positions.len(), 1);
    assert_eq!(
        before.positions[0].net_quantity,
        FixedPoint::try_new(2, 0).expect("net")
    );

    let database = rusqlite::Connection::open(config(&directory).database_path).expect("reader");
    assert_failed_retention_is_atomic(&database, &service, &clock);

    // The next hourly wake retries the same prefix and commits it once.
    clock.advance(Duration::from_secs(3_700));
    service.status().expect("retention retry");
    service.status().expect("retention barrier");
    let during = service.snapshot().expect("post-retention snapshot");
    assert_eq!(during.positions, before.positions);
    assert_eq!(during.account_pnl, before.account_pnl);

    assert_checkpoint_rows(
        &directory,
        9,
        4,
        13,
        7,
        before.account_pnl[0].balance.expect("balance").units(),
    );

    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    let recovered = restarted.snapshot().expect("recovered snapshot");
    assert_eq!(recovered.positions, before.positions);
    assert_eq!(recovered.account_pnl, before.account_pnl);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

fn assert_failed_retention_is_atomic(
    database: &rusqlite::Connection,
    service: &TradingService,
    clock: &crate::clock::ManualClock,
) {
    // Fail at the actual delete after checkpoints have been written. SQLite rolls
    // the whole retention transaction back, including both checkpoint families.
    database
        .execute_batch(
            "CREATE TRIGGER fail_retention_delete BEFORE DELETE ON fills
             BEGIN SELECT RAISE(ABORT, 'injected crash between checkpoint and delete'); END;",
        )
        .expect("install failure");
    clock.advance(Duration::from_secs(3_700));
    service.status().expect("retention tick");
    service.status().expect("failed-retention barrier");
    let failed_fills: i64 = database
        .query_row("SELECT count(*) FROM fills", [], |row| row.get(0))
        .expect("fills after failed retention");
    assert_eq!(failed_fills, 13, "no fill deletion can survive the crash");
    for table in ["position_checkpoints", "ledger_checkpoints"] {
        let rows: i64 = database
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("checkpoint after failed retention");
        assert_eq!(rows, 0, "{table} must roll back with the delete");
    }
    database
        .execute_batch("DROP TRIGGER fail_retention_delete")
        .expect("remove failure");
}

#[test]
fn retention_never_prunes_fills_pinned_by_a_live_group() {
    // VAL-TRD-059: rows referenced by an active group are never deleted, even when they fall
    // outside the retention window.
    use crate::clock::Clock;
    let directory = TestDirectory::new("retention-group-pin");
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    configuration.retention.maximum_fills = 1;
    let service = start_service_with_config(configuration);
    let feed = service
        .attach_market_feed(Arc::new(market_feed::RecordedInterest::default()))
        .expect("feed");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service
        .status()
        .expect("owner drains verified fixture feed");
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    // An open ticket keeps the quote subscribed across the flat periods, so the owner's
    // observation revisions stay monotone while the round trips trade.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");

    // Winning round trips keep discipline locks out of the way.
    let mut sequence = 1_u64;
    winning_round_trips(&service, &feed, &clock, &mut sequence, 3, "pin-rt");
    clock.advance(Duration::from_secs(10));
    let now = clock.wall_unix_nanos();
    sequence += 1;
    let mut entry = market_order("pinned-entry", OrderSide::Buy, sequence, now);
    entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    entry.time_in_force = TimeInForce::GoodTillCancelled;
    // A provider resume drops the retained quote, so the entry waits for a quote received
    // after its acceptance instead of filling at the placement.
    resume_live_without_quote(&feed, &instrument());
    service.status().expect("owner drains the provider resume");
    let awaiting = service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: bracket_template(),
        })
        .expect("bracket accepted");
    let PlacementOutcome::Managed(awaiting) = awaiting else {
        panic!("a simulated bracket is managed locally");
    };
    assert_eq!(awaiting.status, BracketViewStatus::AwaitingEntry);
    // A quote is eligible only after the entry's acceptance time.
    assert_eq!(
        deliver_quote_at(
            &service,
            &feed,
            &instrument(),
            sequence,
            9_975,
            10_000,
            now + 1_000_000,
        )
        .len(),
        1,
        "entry fills"
    );
    let active = service.snapshot().expect("active snapshot");
    assert_eq!(active.managed_brackets[0].status, BracketViewStatus::Active);
    let entry_order = active
        .orders
        .iter()
        .find(|order| order.client_order_id.as_str() == "pinned-entry")
        .expect("entry order")
        .id
        .as_str()
        .to_string();

    clock.advance(Duration::from_secs(3_700));
    service.status().expect("retention tick");
    // The owner replies to a command before its next loop turn notices the due retention
    // timer, so a second round-trip is the deterministic barrier after the retention pass.
    service.status().expect("retention barrier");

    let database = rusqlite::Connection::open(config(&directory).database_path).expect("reader");
    let entry_fills: i64 = database
        .query_row(
            "SELECT count(*) FROM fills WHERE order_id = ?1",
            [entry_order.as_str()],
            |row| row.get(0),
        )
        .expect("entry fill count");
    assert_eq!(entry_fills, 1, "the active group's entry fill is pinned");
    let total_fills: i64 = database
        .query_row("SELECT count(*) FROM fills", [], |row| row.get(0))
        .expect("fill count");
    assert_eq!(total_fills, 1, "only the pinned fill survives");
    let checkpointed_fills: i64 = database
        .query_row(
            "SELECT fill_count FROM position_checkpoints WHERE account_id = 'aeris-sim-1'",
            [],
            |row| row.get(0),
        )
        .expect("position checkpoint row");
    assert_eq!(checkpointed_fills, 6, "the prunable prefix folded exactly");

    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    let recovered = restarted.snapshot().expect("recovered snapshot");
    assert_eq!(recovered.positions, active.positions);
    assert_eq!(recovered.account_pnl, active.account_pnl);
    assert_eq!(
        recovered.managed_brackets[0].status,
        BracketViewStatus::Active
    );
}

#[test]
fn retention_keeps_fills_and_events_referenced_by_an_open_order() {
    // Simulated fills are all-or-nothing. Model a partially-filled broker-era
    // order in SQLite to exercise the open-order retention predicate.
    let directory = TestDirectory::new("retention-open-order-pin");
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    configuration.retention.maximum_fills = 1;
    configuration.retention.maximum_order_events = 1;
    let service = start_service_with_config(configuration);
    let feed = service
        .attach_market_feed(Arc::new(market_feed::RecordedInterest::default()))
        .expect("feed");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service
        .status()
        .expect("owner drains verified fixture feed");
    service
        .register_instrument(instrument())
        .expect("instrument");
    let order = service
        .place_order(market_order(
            "partially-filled-pin",
            OrderSide::Buy,
            1,
            1_000,
        ))
        .expect("order");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 10_000, 10_025).len(),
        1,
        "fill"
    );
    // The later-fill quote is delivered before the placement, so the market order fills at
    // its touch.
    deliver_quote(&service, &feed, &instrument(), 4, 10_010, 10_025);
    service
        .place_order(market_order("later-fill", OrderSide::Sell, 3, 3_000))
        .expect("later order");
    let path = config(&directory).database_path;
    let connection = rusqlite::Connection::open(&path).expect("fixture database");
    connection
        .execute(
            "UPDATE orders SET status = 'partially_filled' WHERE id = ?1",
            [order.id.as_str()],
        )
        .expect("model partially filled order");
    clock.advance(Duration::from_secs(3_700));
    service.status().expect("retention tick");
    service.status().expect("retention barrier");
    for (table, condition) in [
        ("fills", "order_id"),
        ("order_events", "order_id"),
        ("orders", "id"),
    ] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE {condition} = ?1"),
                [order.id.as_str()],
                |row| row.get(0),
            )
            .expect("pinned rows");
        assert!(
            count > 0,
            "{table} rows referenced by the open order survive"
        );
    }
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

fn equity_watch_profile() -> RiskProfile {
    RiskProfile {
        account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
        profile_id: "equity-watch".to_string(),
        version: 1,
        session_start_unix_nanos: 1,
        session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
        daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("daily loss"),
        trailing_drawdown: None,
        trailing_mode: TrailingDrawdownMode::Intraday,
        max_contracts: FixedPoint::try_new(5, 0).expect("contracts"),
        consistency_max_single_trade_percent: None,
        restricted_until_unix_nanos: None,
        economic_event_rule: None,
        enabled: true,
    }
}

#[test]
fn equity_samples_follow_fills_timer_changes_sessions_and_restart() {
    // VAL-TRD-059: every fill writes one equity sample, the 60 s timer writes only when
    // equity changed plus one sample at each risk session boundary, and the in-memory ring
    // is rebuilt from the durable history across a restart.
    use crate::clock::Clock;
    let directory = TestDirectory::new("equity-samples");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .register_risk_profile(equity_watch_profile())
        .expect("profile registers");

    // The first equity tick fires one interval after open and writes the initial sample.
    clock.advance(Duration::from_secs(61));
    service.status().expect("startup tick");
    let initial = service
        .snapshot()
        .expect("initial snapshot")
        .equity_samples
        .len();
    assert!(initial >= 1, "the startup tick samples the account");

    // One sample per fill, even when equity is unchanged.
    clock.advance(Duration::from_secs(1));
    let accepted_at = clock.wall_unix_nanos();
    service
        .place_order(market_order("eq-fill", OrderSide::Buy, 1, accepted_at))
        .expect("order accepted");
    // A quote is eligible only after the order's acceptance time.
    clock.advance(Duration::from_millis(1));
    let now = clock.wall_unix_nanos();
    let fills = deliver_quote_at(&service, &feed, &instrument(), 1, 10_000, 10_001, now);
    assert_eq!(fills.len(), 1, "fill");
    let after_fill = service.snapshot().expect("post-fill snapshot");
    assert_eq!(after_fill.equity_samples.len(), initial + 1);
    assert_eq!(
        after_fill
            .equity_samples
            .last()
            .expect("fill sample")
            .at_unix_nanos,
        now,
        "the fill sample carries the fill time"
    );

    // The next tick settles the post-fill mark once; after that an unchanged equity stops
    // the timer from writing.
    clock.advance(Duration::from_secs(61));
    service.status().expect("settling tick");
    let settled = service
        .snapshot()
        .expect("settled snapshot")
        .equity_samples
        .len();
    assert!(
        settled == initial + 1 || settled == initial + 2,
        "the settling tick writes at most one changed sample"
    );
    for tick in 0..10 {
        clock.advance(Duration::from_secs(61));
        service.status().expect("unchanged tick");
        assert_eq!(
            service
                .snapshot()
                .expect("unchanged snapshot")
                .equity_samples
                .len(),
            settled,
            "unchanged equity wrote a sample on tick {tick}"
        );
    }

    // A quote move changes unrealized, so the next timer tick samples once.
    let moved_at = clock.wall_unix_nanos();
    deliver_quote_at(&service, &feed, &instrument(), 2, 10_050, 10_051, moved_at);
    clock.advance(Duration::from_secs(61));
    service.status().expect("changed tick");
    let after_move = service.snapshot().expect("changed snapshot");
    assert_eq!(after_move.equity_samples.len(), settled + 1);
    assert_eq!(
        after_move
            .equity_samples
            .last()
            .expect("timer sample")
            .at_unix_nanos,
        clock.wall_unix_nanos(),
        "the timer sample carries the tick time"
    );

    // A UTC-day crossing writes the risk session-end sample even though equity is unchanged.
    clock.advance(Duration::from_mins(1_450));
    service.status().expect("session tick");
    let after_session = service.snapshot().expect("session snapshot");
    assert_eq!(after_session.equity_samples.len(), settled + 2);
    let before_restart = after_session.equity_samples;

    // The ring is rebuilt from the durable history; sampling resumes one interval after
    // open, so the immediate post-restart snapshot replays the samples unchanged.
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    // Reopen on the same clock so no session boundary passes across the restart.
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let restarted = start_service_with_config(configuration);
    let recovered = restarted
        .snapshot()
        .expect("recovered snapshot")
        .equity_samples;
    assert_eq!(recovered, before_restart);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}
#[test]
fn per_instrument_fee_override_wins_over_the_class_model_across_a_restart() {
    // VAL-TRD-055: the ESZ6 override charges 2.50 per contract while the future class model
    // would charge 1.00, and the schedule and its fees survive a restart byte-exact.
    let directory = TestDirectory::new("fee-override-restart");
    let (service, clock, inbox) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut schedule = aeris_trading::FeeSchedule::default();
    schedule.by_class.insert(
        aeris_instruments::InstrumentClass::Future,
        aeris_trading::FeeModel::PerContract {
            per_side: FixedPoint::try_new(100, 2).expect("class fee"),
        },
    );
    schedule.by_instrument.insert(
        instrument().instrument_id.clone(),
        aeris_trading::FeeModel::PerContract {
            per_side: FixedPoint::try_new(250, 2).expect("override fee"),
        },
    );
    service
        .set_fee_schedule(account.clone(), schedule.clone())
        .expect("schedule");
    service
        .place_order(market_order("fee-override", OrderSide::Buy, 1, 1_000))
        .expect("accepted");
    assert_eq!(
        deliver_quote(&service, &inbox, &instrument(), 1, 10_000, 10_001).len(),
        1,
        "fill"
    );
    let before = service.snapshot().expect("snapshot");
    assert_eq!(before.fee_schedules, vec![(account.clone(), schedule)]);
    let fees = before
        .ledger_entries
        .iter()
        .filter(|entry| entry.kind == aeris_trading::LedgerEntryKind::Commission)
        .map(|entry| entry.amount.units())
        .collect::<Vec<_>>();
    assert_eq!(fees, [-250]);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let (service, inbox) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    let recovered = service.snapshot().expect("recovered snapshot");
    assert_eq!(recovered.fee_schedules, before.fee_schedules);
    assert_eq!(recovered.ledger_entries, before.ledger_entries);
    service
        .place_order(market_order("fee-override-exit", OrderSide::Sell, 2, 2_000))
        .expect("exit accepted");
    assert_eq!(
        deliver_quote(&service, &inbox, &instrument(), 2, 10_000, 10_001).len(),
        1,
        "exit fill"
    );
    let fees = service
        .snapshot()
        .expect("snapshot")
        .ledger_entries
        .iter()
        .filter(|entry| entry.kind == aeris_trading::LedgerEntryKind::Commission)
        .map(|entry| entry.amount.units())
        .collect::<Vec<_>>();
    assert_eq!(fees, [-250, -250]);
}

#[test]
fn per_unit_fee_clamps_to_its_minimum_and_notional_cap() {
    // VAL-TRD-055: per-unit charges clamp to the configured minimum and the notional cap,
    // and an unset or explicit none model charges nothing.
    let instrument = instrument();
    let fill = |price: i64, quantity: i64| Fill {
        id: aeris_trading::FillId::try_new("fill-1").expect("fill"),
        order_id: aeris_trading::OrderId::try_new("sim-order-1").expect("order"),
        account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
        instrument_id: instrument.instrument_id.clone(),
        side: OrderSide::Buy,
        price: FixedPoint::try_new(price, 2).expect("price"),
        quantity: FixedPoint::try_new(quantity, 0).expect("quantity"),
        execution_unix_nanos: 1_000,
        provenance: provenance(1, 1_000),
    };
    let fee = |model: aeris_trading::FeeModel, fill: &Fill, liquidity: aeris_trading::Liquidity| {
        let mut schedule = aeris_trading::FeeSchedule::default();
        schedule
            .by_class
            .insert(aeris_instruments::InstrumentClass::Future, model);
        crate::core::ledger::fee_for_fill(Some(&schedule), &instrument, fill, liquidity, 2)
            .expect("fee")
    };
    // 0.10 per unit on one unit clamps up to the 0.50 minimum.
    let clamped = fee(
        aeris_trading::FeeModel::PerUnit {
            per_unit: FixedPoint::try_new(10, 2).expect("rate"),
            minimum: Some(FixedPoint::try_new(50, 2).expect("minimum")),
            maximum_bps: None,
        },
        &fill(10_000, 1),
        aeris_trading::Liquidity::Taker,
    );
    assert_eq!(clamped, FixedPoint::try_new(50, 2).expect("minimum fee"));
    // 5.00 per unit on ten units clamps down to 1 bps of the 50_000.00 notional: 5.00.
    let capped = fee(
        aeris_trading::FeeModel::PerUnit {
            per_unit: FixedPoint::try_new(500, 2).expect("rate"),
            minimum: None,
            maximum_bps: Some(1),
        },
        &fill(10_000, 10),
        aeris_trading::Liquidity::Taker,
    );
    assert_eq!(capped, FixedPoint::try_new(500, 2).expect("capped fee"));
    // Maker and taker basis points split on the same 5_000.00 notional: 0.50 and 1.50.
    let model = aeris_trading::FeeModel::NotionalBps {
        maker_bps: 1,
        taker_bps: 3,
    };
    assert_eq!(
        fee(model, &fill(10_000, 1), aeris_trading::Liquidity::Maker),
        FixedPoint::try_new(50, 2).expect("maker fee")
    );
    assert_eq!(
        fee(model, &fill(10_000, 1), aeris_trading::Liquidity::Taker),
        FixedPoint::try_new(150, 2).expect("taker fee")
    );
    // Free models and missing schedules charge nothing.
    assert_eq!(
        fee(
            aeris_trading::FeeModel::None,
            &fill(10_000, 1),
            aeris_trading::Liquidity::Taker
        ),
        FixedPoint::try_new(0, 2).expect("free")
    );
    assert_eq!(
        crate::core::ledger::fee_for_fill(
            None,
            &instrument,
            &fill(10_000, 1),
            aeris_trading::Liquidity::Taker,
            2,
        )
        .expect("no schedule"),
        FixedPoint::try_new(0, 2).expect("free")
    );
}

#[test]
fn fee_schedule_rejects_unknown_accounts_instruments_and_invalid_models() {
    // VAL-TRD-055: the command validates its account, its instrument overrides, the model
    // rates, and refuses broker accounts.
    let directory = TestDirectory::new("fee-schedule-validation");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let unknown_account = service
        .set_fee_schedule(
            TradingAccountId::try_new("aeris-sim-9").expect("account"),
            aeris_trading::FeeSchedule::default(),
        )
        .expect_err("unknown account");
    assert!(unknown_account.contains("not registered"));

    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut unknown_instrument = aeris_trading::FeeSchedule::default();
    unknown_instrument.by_instrument.insert(
        InstrumentId::try_new("instrument:fixture:CME:UNKNOWN").expect("instrument"),
        aeris_trading::FeeModel::None,
    );
    let error = service
        .set_fee_schedule(account.clone(), unknown_instrument)
        .expect_err("unknown instrument");
    assert!(error.contains("is not registered"));

    let mut negative = aeris_trading::FeeSchedule::default();
    negative.by_class.insert(
        aeris_instruments::InstrumentClass::Future,
        aeris_trading::FeeModel::PerContract {
            per_side: FixedPoint::try_new(-1, 2).expect("negative"),
        },
    );
    service
        .set_fee_schedule(account.clone(), negative)
        .expect_err("negative fee");
    let mut excessive = aeris_trading::FeeSchedule::default();
    excessive.by_class.insert(
        aeris_instruments::InstrumentClass::Future,
        aeris_trading::FeeModel::NotionalBps {
            maker_bps: 10_001,
            taker_bps: 2,
        },
    );
    service
        .set_fee_schedule(account, excessive)
        .expect_err("fee rate above the maximum");

    service
        .register_account(TradingAccount {
            id: TradingAccountId::try_new("demo-1").expect("account"),
            display_name: "test demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: aeris_trading::venue::BROKER_VENUE_ID.into(),
            broker_ref: Some("fixture-demo".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("broker account");
    let broker = service
        .set_fee_schedule(
            TradingAccountId::try_new("demo-1").expect("account"),
            aeris_trading::FeeSchedule::default(),
        )
        .expect_err("broker account");
    assert!(broker.contains("simulated"));
}

#[test]
fn startup_repairs_legacy_nonflat_position_with_missing_average_from_fill_ledger() {
    let directory = TestDirectory::new("position-repair");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");

    let mut first = market_order("repair-first", OrderSide::Buy, 1, 1_000);
    first.quantity = FixedPoint::try_new(3, 0).expect("quantity");
    service.place_order(first).expect("first order accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "first fill"
    );

    // The scale-in quote is delivered before the placement, so the market order fills at
    // its touch.
    deliver_quote(&service, &feed, &instrument(), 4, 10_000, 10_001);
    let mut second = market_order("repair-second", OrderSide::Buy, 3, 3_000);
    second.quantity = FixedPoint::try_new(4, 0).expect("quantity");
    service.place_order(second).expect("second order accepted");

    let before = service.snapshot().expect("snapshot");
    let before_position = before
        .positions
        .iter()
        .find(|position| position.instrument_id == instrument().instrument_id)
        .expect("open position");
    assert_eq!(before_position.net_quantity.units(), 7);
    assert_eq!(
        before_position.average_entry_price,
        Some(FixedPoint::try_new(10_001, 2).expect("rounded average"))
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let connection =
        rusqlite::Connection::open(directory.0.join("trading.sqlite3")).expect("database opens");
    connection
        .execute(
            "UPDATE positions
             SET average_units = NULL, average_scale = NULL, unrealized_units = 999
             WHERE account_id = 'aeris-sim-1' AND instrument_id = ?1",
            [instrument().instrument_id.as_str()],
        )
        .expect("legacy corruption injected");
    drop(connection);

    let (reopened, reopened_feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    let repaired = reopened.snapshot().expect("repaired snapshot");
    let position = repaired
        .positions
        .iter()
        .find(|position| position.instrument_id == instrument().instrument_id)
        .expect("repaired position");
    assert_eq!(position.net_quantity.units(), 7);
    assert_eq!(
        position.average_entry_price,
        Some(FixedPoint::try_new(10_001, 2).expect("repaired average"))
    );
    assert_eq!(position.unrealized_pnl.units(), 0);

    let mut third = market_order("repair-third", OrderSide::Buy, 5, 5_000);
    third.quantity = FixedPoint::try_new(1, 0).expect("quantity");
    reopened
        .place_order(third)
        .expect("new order accepted after repair");
    let fills = deliver_quote(&reopened, &reopened_feed, &instrument(), 6, 10_024, 10_025);
    assert_eq!(fills.len(), 1);
    let after_trade = reopened.snapshot().expect("post-repair trading snapshot");
    assert!(after_trade.market_observation_error.is_none());
    let position = after_trade
        .positions
        .iter()
        .find(|position| position.instrument_id == instrument().instrument_id)
        .expect("updated position");
    assert_eq!(position.net_quantity.units(), 8);
    assert!(position.average_entry_price.is_some());
    reopened
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn irreparable_legacy_position_fails_before_committing_another_fill_and_can_be_deleted() {
    let directory = TestDirectory::new("position-preflight");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .place_order(market_order("legacy-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "entry fills"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let connection =
        rusqlite::Connection::open(directory.0.join("trading.sqlite3")).expect("database opens");
    connection
        .execute("DELETE FROM fills", [])
        .expect("retained fill ledger removed");
    // Corrupt the whole projection: without the fill ledger, the display average or the
    // exact cost basis the owner cannot recompute or continue the position safely.
    connection
        .execute(
            "UPDATE positions SET average_units = NULL, average_scale = NULL,
                cost_basis_hi = NULL, cost_basis_lo = NULL, cost_price_scale = NULL,
                cost_quantity_scale = NULL
             WHERE account_id = 'aeris-sim-1'",
            [],
        )
        .expect("legacy corruption injected");
    drop(connection);

    let (reopened, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    reopened
        .place_order(market_order("must-not-fill", OrderSide::Buy, 3, 3_000))
        .expect("order itself is accepted");
    assert_eq!(
        deliver_quote(&reopened, &feed, &instrument(), 4, 10_024, 10_025),
        []
    );
    let snapshot = reopened.snapshot().expect("snapshot");
    let error = snapshot
        .market_observation_error
        .as_deref()
        .expect("invalid existing position blocks before fill commit");
    assert!(error.contains("position average is unavailable"));
    let order = snapshot
        .orders
        .iter()
        .find(|order| order.client_order_id.as_str() == "must-not-fill")
        .expect("order retained");
    assert_eq!(order.filled_quantity.units(), 0);
    assert_eq!(snapshot.fills, [] as [aeris_trading::Fill; 0]);
    assert_eq!(
        snapshot
            .positions
            .iter()
            .find(|position| position.account_id.as_str() == "aeris-sim-1")
            .expect("legacy position")
            .net_quantity
            .units(),
        1
    );

    reopened
        .delete_practice_account(TradingAccountId::try_new("aeris-sim-1").expect("account"))
        .expect("confirmed destructive deletion remains available for corrupt state");
    assert!(
        reopened
            .snapshot()
            .expect("post-delete snapshot")
            .accounts
            .iter()
            .all(|account| account.id.as_str() != "aeris-sim-1")
    );
    reopened
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn confirmed_practice_account_delete_purges_open_state_and_is_durable() {
    let directory = TestDirectory::new("practice-account-delete");
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        9_975,
        10_000,
    );
    let created = service
        .create_practice_account(CreatePracticeAccount {
            display_name: "Disposable".to_string(),
            starting_equity: FixedPoint::try_new(5_000_000, 2).expect("equity"),
        })
        .expect("account creates");
    service
        .register_instrument(instrument())
        .expect("instrument registers");

    let mut working = market_order("delete-guard", OrderSide::Buy, 1, 1_000);
    working.account_id = created.id.clone();
    working.order_type = OrderType::Limit;
    working.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
    service
        .place_order(working)
        .expect("working order accepted");
    let mut market = market_order("delete-position", OrderSide::Buy, 1, 1_100);
    market.account_id = created.id.clone();
    assert_eq!(
        service
            .place_order(market)
            .expect("market order accepted")
            .status,
        OrderStatus::Filled,
        "market order fills against the owner quote"
    );
    let before = service.snapshot().expect("snapshot before deletion");
    assert!(
        before
            .orders
            .iter()
            .any(|order| order.account_id == created.id)
    );
    assert!(before.positions.iter().any(|position| {
        position.account_id == created.id && position.net_quantity.units() != 0
    }));

    let deleted = service
        .delete_practice_account(created.id.clone())
        .expect("confirmed destructive delete succeeds");
    assert_eq!(deleted, created);
    let snapshot = service.snapshot().expect("snapshot");
    assert!(
        snapshot
            .accounts
            .iter()
            .all(|account| account.id != created.id)
    );
    assert!(
        snapshot
            .orders
            .iter()
            .all(|order| order.account_id != created.id)
    );
    assert!(
        snapshot
            .fills
            .iter()
            .all(|fill| fill.account_id != created.id)
    );
    assert!(
        snapshot
            .positions
            .iter()
            .all(|position| position.account_id != created.id)
    );

    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let reopened = TradingService::start(config(&directory)).expect("service reopens");
    assert!(
        reopened
            .snapshot()
            .expect("restored snapshot")
            .accounts
            .iter()
            .all(|account| account.id != created.id)
    );
    reopened
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

fn bracket_template() -> BracketStrategyTemplate {
    BracketStrategyTemplate {
        template_id: "two-target".to_string(),
        revision: 1,
        name: "Two target managed bracket".to_string(),
        stop_offset_ticks: 8,
        targets: vec![
            BracketTarget {
                offset_ticks: 8,
                quantity_percent: 50,
            },
            BracketTarget {
                offset_ticks: 16,
                quantity_percent: 50,
            },
        ],
        trailing_stop: Some(TrailingStopRule {
            activation_ticks: 12,
            distance_ticks: 6,
        }),
        break_even: Some(BreakEvenRule {
            activation_ticks: 8,
            offset_ticks: 1,
        }),
        enabled: true,
    }
}

#[test]
fn cancelling_a_held_leg_cancels_the_pending_entry_too() {
    // Regression: a pending bracket terminated through a held leg left its working entry
    // naked; the cascade must retire the entry with the group.
    let directory = TestDirectory::new("held-leg-cascade");
    let (service, _clock, _feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let entry = market_order("held-leg-entry", OrderSide::Buy, 1, 1_000);
    let awaiting = service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: bracket_template(),
        })
        .expect("bracket accepted");
    let PlacementOutcome::Managed(awaiting) = awaiting else {
        panic!("a simulated bracket is managed locally");
    };
    assert_eq!(awaiting.status, BracketViewStatus::AwaitingEntry);

    let snapshot = service.snapshot().expect("snapshot");
    let held_stop = snapshot
        .orders
        .iter()
        .find(|order| order.order_type == OrderType::Stop && order.status == OrderStatus::Inactive)
        .expect("held stop leg")
        .client_order_id
        .clone();
    service.cancel_order(held_stop).expect("held leg cancel");

    let cancelled = service.snapshot().expect("cancelled snapshot");
    let entry = cancelled
        .orders
        .iter()
        .find(|order| order.client_order_id.as_str() == "held-leg-entry")
        .expect("entry order");
    assert_eq!(entry.status, OrderStatus::Cancelled);
    assert!(
        cancelled.orders.iter().all(|order| !order.status.is_open()),
        "no order of the cancelled bracket may stay open"
    );
    assert_eq!(
        cancelled.managed_brackets[0].status,
        BracketViewStatus::Cancelled
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn cancelling_pending_bracket_entry_cascades_to_every_held_leg() {
    let directory = TestDirectory::new("pending-entry-cancel-cascade");
    let (service, _clock, _feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut entry = market_order("cancel-pending-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = quantity(2);
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: bracket_template(),
        })
        .expect("pending bracket");
    let pending = service.snapshot().expect("pending");
    let legs = pending
        .orders
        .iter()
        .filter(|order| order.status == OrderStatus::Inactive)
        .map(|order| order.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(legs.len(), 3, "one stop and two targets");
    service
        .cancel_order(ClientOrderId::try_new("cancel-pending-entry").expect("entry id"))
        .expect("entry cancelled");
    let cancelled = service.snapshot().expect("cancelled");
    assert_eq!(
        cancelled.managed_brackets[0].status,
        BracketViewStatus::Cancelled
    );
    for leg in legs {
        assert_eq!(
            cancelled
                .orders
                .iter()
                .find(|order| order.id == leg)
                .unwrap()
                .status,
            OrderStatus::Cancelled
        );
        assert_leg_event(
            &cancelled,
            &leg,
            OrderEventKind::Cancelled,
            aeris_trading::OrderEventReason::Cascade,
        );
    }
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn managed_bracket_activates_after_entry_and_oco_survives_restart() {
    let directory = TestDirectory::new("managed-bracket");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("bracket-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    let awaiting = service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: bracket_template(),
        })
        .expect("bracket accepted");
    let PlacementOutcome::Managed(awaiting) = awaiting else {
        panic!("a simulated bracket is managed locally");
    };
    assert_eq!(awaiting.status, BracketViewStatus::AwaitingEntry);
    assert!(awaiting.stop_client_order_id.is_none());

    let entry_fills = deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000);
    assert_eq!(entry_fills.len(), 1, "entry fills");
    let active = service.snapshot().expect("active snapshot");
    let bracket = &active.managed_brackets[0];
    assert_eq!(bracket.status, BracketViewStatus::Active);
    assert_eq!(bracket.target_client_order_ids.len(), 2);
    assert_eq!(BracketView::MANAGEMENT_LABEL, "LOCAL-MANAGED");
    let working = active
        .orders
        .iter()
        .filter(|order| order.status.is_open())
        .collect::<Vec<_>>();
    assert_eq!(working.len(), 3);
    assert!(working.iter().any(|order| {
        order.order_type == OrderType::Stop
            && order.stop_price == Some(FixedPoint::try_new(9_800, 2).expect("stop"))
            && order.quantity == FixedPoint::try_new(2, 0).expect("quantity")
    }));

    let target_fills = deliver_quote(&service, &feed, &instrument(), 3, 10_200, 10_225);
    assert_eq!(target_fills.len(), 1, "first target fills");
    let scaled = service.snapshot().expect("scaled snapshot");
    assert!(
        scaled.completed_trade_pnl.is_empty(),
        "a partial exit must not report final trade P&L"
    );
    assert_eq!(
        scaled.fill_realized_pnl.get(&scaled.fills[0].id),
        Some(&scaled.positions[0].realized_pnl),
        "a partial exit reports the P&L it realized"
    );
    assert!(
        !scaled.fill_realized_pnl.contains_key(&scaled.fills[1].id),
        "an entry realizes nothing"
    );
    let stop = scaled
        .orders
        .iter()
        .find(|order| order.order_type == OrderType::Stop && order.status.is_open())
        .expect("replacement stop");
    assert_eq!(stop.quantity, FixedPoint::try_new(1, 0).expect("quantity"));
    assert_leg_event(
        &scaled,
        &stop.id,
        OrderEventKind::Resized,
        aeris_trading::OrderEventReason::CoverageShrink,
    );
    assert_eq!(stop.stop_price, Some(price(10_025)));

    let stop_fills = deliver_quote(&service, &feed, &instrument(), 4, 10_000, 10_025);
    assert_eq!(stop_fills.len(), 1, "stop fills");
    let complete = service.snapshot().expect("complete snapshot");
    assert_eq!(complete.completed_trade_pnl.len(), 1);
    assert_eq!(
        complete.completed_trade_pnl.get(&complete.fills[0].id),
        Some(&complete.positions[0].realized_pnl)
    );
    let exits = complete.fill_realized_pnl.values().collect::<Vec<_>>();
    assert_eq!(exits.len(), 2, "both exits report their realized P&L");
    assert_eq!(
        exits[0].checked_add(*exits[1]).expect("exit sum"),
        complete.positions[0].realized_pnl,
        "the exits add up to the trade's final P&L"
    );
    assert_eq!(
        complete.managed_brackets[0].status,
        BracketViewStatus::Completed
    );
    assert!(complete.orders.iter().all(|order| !order.status.is_open()));
    assert_eq!(complete.positions[0].net_quantity.units(), 0);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = start_service(&directory);
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.managed_brackets, complete.managed_brackets);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn stop_entry_bracket_rests_above_the_market_and_arms_at_its_entry() {
    let directory = TestDirectory::new("stop-entry-bracket");
    let (service, _clock, feed) = quoted_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        9_975,
        10_000,
    );
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("stop-entry", OrderSide::Buy, 1, 1_000);
    entry.order_type = OrderType::Stop;
    entry.stop_price = Some(FixedPoint::try_new(10_100, 2).expect("entry"));
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: fixed_stop_bracket_template(),
        })
        .expect("bracket accepted");

    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000),
        [],
        "market below the entry"
    );
    let resting = service.snapshot().expect("resting snapshot");
    assert_eq!(
        resting.managed_brackets[0].status,
        BracketViewStatus::AwaitingEntry
    );
    assert!(resting.fills.is_empty(), "the entry waits for its price");

    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 3, 10_075, 10_100).len(),
        1,
        "market reaches the entry"
    );
    let armed = service.snapshot().expect("armed snapshot");
    assert_eq!(armed.managed_brackets[0].status, BracketViewStatus::Active);
    assert_eq!(
        armed.fills[0].price,
        FixedPoint::try_new(10_100, 2).expect("fill")
    );
    assert!(armed.orders.iter().any(|order| {
        order.status.is_open()
            && order.order_type == OrderType::Stop
            && order.stop_price == Some(FixedPoint::try_new(9_900, 2).expect("stop"))
    }));
    assert!(
        armed
            .orders
            .iter()
            .filter(|order| order.status.is_open())
            .all(|order| order.time_in_force == TimeInForce::GoodTillCancelled),
        "activated stop and targets must be GTC regardless of the entry's time in force"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn resting_buy_and_sell_limits_fill_at_their_limit_not_the_better_touch() {
    let directory = TestDirectory::new("resting-limit-price");
    let (service, _clock, feed) = quoted_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_000,
        10_025,
    );
    service.register_instrument(instrument()).expect("terms");
    let place_limit = |id, side, limit| {
        let mut order = market_order(id, side, 1, 1_000);
        order.order_type = OrderType::Limit;
        order.time_in_force = TimeInForce::GoodTillCancelled;
        order.limit_price = Some(price(limit));
        service.place_order(order).expect("limit accepted")
    };
    let buy = place_limit("resting-buy", OrderSide::Buy, 9_900);
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_775, 9_800).len(),
        1,
        "ask crosses the buy limit"
    );
    let snapshot = service.snapshot().expect("buy fill");
    assert_eq!(
        snapshot
            .fills
            .iter()
            .find(|fill| fill.order_id == buy.id)
            .expect("buy fills")
            .price,
        price(9_900)
    );
    let sell = place_limit("resting-sell", OrderSide::Sell, 10_200);
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 3, 10_300, 10_325).len(),
        1,
        "bid crosses the sell limit"
    );
    let snapshot = service.snapshot().expect("sell fill");
    assert_eq!(
        snapshot
            .fills
            .iter()
            .find(|fill| fill.order_id == sell.id)
            .expect("sell fills")
            .price,
        price(10_200)
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn crossed_limits_execute_by_price_then_acceptance_sequence() {
    let directory = TestDirectory::new("limit-priority");
    let (service, _clock, feed) = quoted_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_025,
        10_050,
    );
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut placed = Vec::new();
    for (id, limit) in [
        ("low-first", 9_900),
        ("high-second", 10_000),
        ("high-third", 10_000),
    ] {
        let mut request = market_order(id, OrderSide::Buy, 1, 1_000);
        request.order_type = OrderType::Limit;
        request.time_in_force = TimeInForce::GoodTillCancelled;
        request.limit_price = Some(price(limit));
        placed.push(service.place_order(request).expect("limit"));
    }
    let fills = deliver_quote(&service, &feed, &instrument(), 2, 9_775, 9_800);
    assert_eq!(
        fills.iter().map(|fill| &fill.order_id).collect::<Vec<_>>(),
        vec![&placed[1].id, &placed[2].id, &placed[0].id]
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn crossed_stops_precede_limits_in_trigger_price_order() {
    let directory = TestDirectory::new("stop-priority");
    let (service, _clock, feed) = quoted_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        9_975,
        10_000,
    );
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut placed = Vec::new();
    // A resting buy limit above valid buy stops would be marketable against any quote the
    // stops accept, so the gap quote crosses a resting sell limit instead.
    for (id, side, kind, level) in [
        ("limit-first", OrderSide::Sell, OrderType::Limit, 10_250),
        ("high-stop", OrderSide::Buy, OrderType::Stop, 10_200),
        ("low-stop", OrderSide::Buy, OrderType::Stop, 10_100),
    ] {
        let mut request = market_order(id, side, 1, 1_000);
        request.order_type = kind;
        request.time_in_force = TimeInForce::GoodTillCancelled;
        match kind {
            OrderType::Limit => request.limit_price = Some(price(level)),
            OrderType::Stop => request.stop_price = Some(price(level)),
            _ => unreachable!("fixture"),
        }
        placed.push(service.place_order(request).expect("order"));
    }
    let fills = deliver_quote(&service, &feed, &instrument(), 2, 10_275, 10_300);
    assert_eq!(
        fills.iter().map(|fill| &fill.order_id).collect::<Vec<_>>(),
        vec![&placed[2].id, &placed[1].id, &placed[0].id]
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn point_value_scales_exact_marks_and_closed_trade_pnl() {
    for (label, point_value, expected_mark, expected_realized) in
        [("future", 50, 5_000, 12_500), ("crypto", 1, 100, 250)]
    {
        let directory = TestDirectory::new(label);
        let mut terms = instrument();
        let mut route = market_feed::route();
        if label == "crypto" {
            terms.instrument_id = InstrumentId::try_new("hyperliquid:perp:BTC").expect("id");
            terms.contract.instrument_class = aeris_instruments::InstrumentClass::CryptoPerpetual;
            terms.contract.expiry = None;
            terms.contract.last_trade = None;
            terms.contract.session_hours.clear();
            terms.contract.provenance.provider_id = "hyperliquid".into();
            terms.contract.provenance.provider_symbol = "BTC".into();
            terms.contract.provenance.display_symbol = "BTC".into();
            route.instrument_id = terms.instrument_id.clone();
            route.provider_symbol = "BTC".into();
            route.display_symbol = "BTC".into();
            route.venue_id = "HYPERLIQUID".into();
        }
        terms.contract.point_value =
            Some(InstrumentDecimal::try_new(point_value, 0).expect("point value"));
        let (service, _clock, feed) = awaiting_quote_fixture(&directory, &terms, route);
        service
            .register_instrument(terms.clone())
            .expect("instrument");
        let mut buy = market_order("multiplier-buy", OrderSide::Buy, 1, 1_000);
        buy.instrument_id = terms.instrument_id.clone();
        buy.quantity = contracts(2);
        service.place_order(buy).expect("buy");
        assert_eq!(
            deliver_quote(&service, &feed, &terms, 2, 499_975, 500_000).len(),
            1,
            "entry fill"
        );
        deliver_quote(&service, &feed, &terms, 3, 500_050, 500_075);
        assert_eq!(
            service.snapshot().expect("marked").positions[0]
                .unrealized_pnl
                .units(),
            expected_mark
        );
        // The exit fills at placement against the owner's current quote.
        deliver_quote(&service, &feed, &terms, 5, 500_125, 500_150);
        let mut sell = market_order("multiplier-sell", OrderSide::Sell, 4, 4_000);
        sell.instrument_id = terms.instrument_id;
        sell.quantity = contracts(2);
        assert_eq!(
            service.place_order(sell).expect("sell").status,
            OrderStatus::Filled,
            "exit fill"
        );
        assert_eq!(
            service.snapshot().expect("closed").positions[0]
                .realized_pnl
                .units(),
            expected_realized
        );
        service.shutdown(Duration::from_secs(2)).expect("shutdown");
    }
}

#[test]
fn a_pending_bracket_carries_its_held_legs_and_arms_them_where_they_were_moved() {
    let directory = TestDirectory::new("planned-bracket-legs");
    let route = market_feed::route();
    let (service, clock, _feed) = quoted_fixture(&directory, &instrument(), route, 9_975, 10_000);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("planned-entry", OrderSide::Buy, 1, 1_000);
    entry.order_type = OrderType::Stop;
    entry.stop_price = Some(price(10_100));
    entry.quantity = quantity(2);
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: fixed_stop_bracket_template(),
        })
        .expect("bracket accepted");

    let (stop_client, stop_order) = pending_held_stop(&service);
    let move_stop = |units| {
        service.move_planned_leg(MovePlannedLeg {
            client_order_id: stop_client.clone(),
            price: price(units),
        })
    };
    assert!(move_stop(10_150).is_err(), "a stop cannot cross its entry");
    assert!(move_stop(9_960).is_err(), "a stop stays on whole ticks");
    assert_eq!(move_stop(9_950).expect("stop moves").price, price(9_950));
    let after_move = service.snapshot().expect("moved-leg snapshot");
    assert_leg_event(
        &after_move,
        &stop_order,
        OrderEventKind::Modified,
        aeris_trading::OrderEventReason::LegMoved,
    );

    service
        .modify_order(ModifyOrder {
            client_order_id: ClientOrderId::try_new("planned-entry").expect("client id"),
            time_in_force: TimeInForce::GoodTillCancelled,
            limit_price: None,
            stop_price: Some(price(10_125)),
            modified_unix_nanos: 2_000,
            provenance: provenance(2, 2_000),
        })
        .expect("entry moves");
    let moved = service.snapshot().expect("moved snapshot");
    let moved_legs = held_bracket_legs(&moved);
    let followed = moved_legs
        .iter()
        .find(|(id, _, _, _, _)| *id == stop_client)
        .expect("held stop follows the entry");
    assert_eq!(
        followed.3,
        Some(price(9_975)),
        "the legs keep their distance and follow the entry"
    );
    assert_leg_event(
        &moved,
        &stop_order,
        OrderEventKind::Modified,
        aeris_trading::OrderEventReason::ParentRepriced,
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let (restarted, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(
        held_bracket_legs(&restored),
        moved_legs,
        "the held legs survive a restart at their moved prices"
    );
    let entry_fills = deliver_quote(&restarted, &feed, &instrument(), 3, 10_100, 10_125);
    assert_eq!(entry_fills.len(), 1, "market reaches the entry");
    let armed = restarted.snapshot().expect("armed snapshot");
    assert!(
        armed
            .orders
            .iter()
            .all(|order| order.status != OrderStatus::Inactive),
        "the entry fill activated every held leg"
    );
    assert!(armed.orders.iter().any(|order| {
        order.client_order_id == stop_client
            && order.status.is_open()
            && order.stop_price == Some(price(9_975))
    }));
    assert_leg_event(
        &armed,
        &stop_order,
        OrderEventKind::Activated,
        aeris_trading::OrderEventReason::LocalManaged,
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

/// Asserts the pending bracket's held stop and scale-out targets and returns the stop's
/// client and order ids.
fn pending_held_stop(service: &TradingService) -> (ClientOrderId, aeris_trading::OrderId) {
    let pending = service.snapshot().expect("pending snapshot");
    let pending_legs = held_bracket_legs(&pending);
    let mut planned_ids = pending
        .planned_legs
        .iter()
        .map(|leg| leg.client_order_id.as_str().to_string())
        .collect::<Vec<_>>();
    planned_ids.sort_unstable();
    let mut held_ids = pending_legs
        .iter()
        .map(|leg| leg.0.as_str().to_string())
        .collect::<Vec<_>>();
    held_ids.sort_unstable();
    assert_eq!(
        planned_ids, held_ids,
        "the legacy reader view carries the held legs as the entry's planned legs"
    );
    let stop_leg = pending_legs
        .iter()
        .find(|(_, _, order_type, _, _)| *order_type == OrderType::Stop)
        .expect("held stop");
    assert_eq!(
        (stop_leg.1, stop_leg.3, stop_leg.4),
        (OrderSide::Sell, Some(price(9_900)), quantity(2)),
        "the entry's stop shows before it fills"
    );
    let mut targets = pending_legs
        .iter()
        .filter(|(_, _, order_type, _, _)| *order_type == OrderType::Limit)
        .map(|(_, side, _, leg_price, quantity)| (*side, *leg_price, *quantity))
        .collect::<Vec<_>>();
    targets.sort_by_key(|(_, leg_price, _)| leg_price.map(FixedPoint::units));
    assert_eq!(
        targets,
        [
            (OrderSide::Sell, Some(price(10_300)), quantity(1)),
            (OrderSide::Sell, Some(price(10_500)), quantity(1)),
        ],
        "the scale-out targets show before the entry fills"
    );
    let stop_client = stop_leg.0.clone();
    let stop_order = pending
        .orders
        .iter()
        .find(|order| order.client_order_id == stop_client)
        .expect("held stop order")
        .id
        .clone();
    (stop_client, stop_order)
}

/// The held legs of pending group-owned brackets: inactive orders keyed by client id.
fn held_bracket_legs(
    snapshot: &TradingSnapshot,
) -> Vec<(
    ClientOrderId,
    OrderSide,
    OrderType,
    Option<FixedPoint>,
    FixedPoint,
)> {
    let mut legs = snapshot
        .orders
        .iter()
        .filter(|order| order.status == OrderStatus::Inactive)
        .map(|order| {
            (
                order.client_order_id.clone(),
                order.side,
                order.order_type,
                order.limit_price.or(order.stop_price),
                order.quantity,
            )
        })
        .collect::<Vec<_>>();
    legs.sort_by_key(|(id, _, _, _, _)| id.as_str().to_string());
    legs
}

/// Asserts a leg lifecycle event with the expected kind and reason was recorded.
fn assert_leg_event(
    snapshot: &TradingSnapshot,
    order_id: &aeris_trading::OrderId,
    kind: OrderEventKind,
    reason: aeris_trading::OrderEventReason,
) {
    assert!(
        snapshot.order_events.iter().any(|event| {
            event.order_id == *order_id && event.kind == kind && event.reason == Some(reason)
        }),
        "a {kind:?} event with reason {reason:?} was recorded"
    );
}

fn fixed_stop_bracket_template() -> BracketStrategyTemplate {
    BracketStrategyTemplate {
        trailing_stop: None,
        break_even: None,
        ..bracket_template()
    }
}

#[test]
fn bracket_children_wait_for_next_quote_and_stop_cancels_targets() {
    let directory = TestDirectory::new("wide-quote-bracket");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut entry = market_order("wide-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = quantity(2);
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: fixed_stop_bracket_template(),
        })
        .expect("bracket");
    deliver_quote(&service, &feed, &instrument(), 2, 9_775, 10_000);
    let armed = service.snapshot().expect("armed");
    assert_eq!(armed.fills.len(), 1);
    assert_eq!(
        armed
            .orders
            .iter()
            .filter(|order| order.status.is_open())
            .count(),
        3,
        "the entry's own quote crosses the newly armed stop, but cannot fill it"
    );

    let fills = deliver_quote(&service, &feed, &instrument(), 3, 9_775, 10_225);
    assert_eq!(
        fills.len(),
        1,
        "a sibling must not execute from a stale candidate"
    );
    let settled = service.snapshot().expect("settled");
    assert_eq!(settled.positions[0].net_quantity.units(), 0);
    assert_eq!(
        settled.managed_brackets[0].status,
        BracketViewStatus::Completed
    );
    assert!(settled.orders.iter().all(|order| !order.status.is_open()));
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let reopened = start_service(&directory);
    let restored = reopened.snapshot().expect("restored");
    assert_eq!(restored.orders, settled.orders);
    assert_eq!(restored.fills, settled.fills);
    assert_eq!(restored.managed_brackets, settled.managed_brackets);
    reopened.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn active_bracket_cancellation_cascades_only_from_the_stop() {
    for cancel_stop in [false, true] {
        let directory = TestDirectory::new(if cancel_stop {
            "active-stop-cascade"
        } else {
            "active-target-only"
        });
        let (service, _clock, feed) =
            awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
        service.register_instrument(instrument()).expect("terms");
        let mut entry = market_order("cascade-entry", OrderSide::Buy, 1, 1_000);
        entry.quantity = quantity(2);
        place_managed_bracket(&service, entry);
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000);
        let armed = service.snapshot().expect("active bracket");
        let stop = armed
            .orders
            .iter()
            .find(|order| order.order_type == OrderType::Stop && order.status.is_open())
            .expect("active stop");
        let targets = armed
            .orders
            .iter()
            .filter(|order| order.order_type == OrderType::Limit && order.status.is_open())
            .collect::<Vec<_>>();
        assert_eq!(targets.len(), 2);
        let cancelled = if cancel_stop { stop } else { targets[0] };
        service
            .cancel_order(cancelled.client_order_id.clone())
            .expect("cancel active leg");
        let after = service.snapshot().expect("cascade");
        for target in targets {
            let current = after
                .orders
                .iter()
                .find(|order| order.id == target.id)
                .expect("target persists");
            let cascaded = cancel_stop && target.id != cancelled.id;
            assert_eq!(
                current.status,
                if cascaded || target.id == cancelled.id {
                    OrderStatus::Cancelled
                } else {
                    OrderStatus::Working
                }
            );
            if cascaded {
                assert_leg_event(
                    &after,
                    &target.id,
                    OrderEventKind::Cancelled,
                    aeris_trading::OrderEventReason::Cascade,
                );
            }
        }
        let current_stop = after
            .orders
            .iter()
            .find(|order| order.id == stop.id)
            .expect("stop persists");
        assert_eq!(
            current_stop.status,
            if cancel_stop {
                OrderStatus::Cancelled
            } else {
                OrderStatus::Working
            }
        );
        service.shutdown(Duration::from_secs(2)).expect("shutdown");
    }
}

#[test]
fn last_target_fill_cancels_the_resized_stop_with_typed_cascade() {
    let directory = TestDirectory::new("last-target-cascade");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut entry = market_order("last-target-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = quantity(2);
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: fixed_stop_bracket_template(),
        })
        .expect("bracket");
    deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000);
    let armed = service.snapshot().expect("armed");
    let stop_id = armed
        .orders
        .iter()
        .find(|order| order.order_type == OrderType::Stop && order.status.is_open())
        .expect("stop")
        .id
        .clone();
    deliver_quote(&service, &feed, &instrument(), 3, 10_200, 10_225);
    let halfway = service.snapshot().expect("halfway");
    let stop = halfway
        .orders
        .iter()
        .find(|order| order.id == stop_id)
        .expect("stop");
    assert_eq!(stop.quantity, quantity(1));
    assert_leg_event(
        &halfway,
        &stop_id,
        OrderEventKind::Resized,
        aeris_trading::OrderEventReason::CoverageShrink,
    );
    deliver_quote(&service, &feed, &instrument(), 4, 10_400, 10_425);
    let complete = service.snapshot().expect("complete");
    assert_eq!(complete.positions[0].net_quantity.units(), 0);
    assert_eq!(
        complete.managed_brackets[0].status,
        BracketViewStatus::Completed
    );
    assert_eq!(
        complete
            .orders
            .iter()
            .find(|order| order.id == stop_id)
            .expect("stop")
            .status,
        OrderStatus::Cancelled
    );
    assert_leg_event(
        &complete,
        &stop_id,
        OrderEventKind::Cancelled,
        aeris_trading::OrderEventReason::Cascade,
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn maker_bracket_entry_stop_and_fees_survive_both_restarts() {
    let directory = TestDirectory::new("maker-bracket-fee-chain");
    let (service, clock, feed) = quoted_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_000,
        10_025,
    );
    service.register_instrument(instrument()).expect("terms");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut schedule = aeris_trading::FeeSchedule::default();
    schedule.by_class.insert(
        aeris_instruments::InstrumentClass::Future,
        aeris_trading::FeeModel::PerContract {
            per_side: FixedPoint::try_new(5, 2).expect("per-contract fee"),
        },
    );
    service
        .set_fee_schedule(account.clone(), schedule)
        .expect("fee schedule");
    let mut entry = market_order("maker-bracket", OrderSide::Buy, 2, 2_000);
    entry.order_type = OrderType::Limit;
    entry.time_in_force = TimeInForce::GoodTillCancelled;
    entry.limit_price = Some(price(9_900));
    entry.quantity = quantity(2);
    place_managed_bracket(&service, entry);
    assert_eq!(service.snapshot().expect("resting").fills.len(), 0);
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 3, 9_875, 9_900).len(),
        1,
        "maker entry fills"
    );
    let active = service.snapshot().expect("active");
    assert_eq!(active.fills.len(), 1);
    assert_eq!(active.fills[0].price, price(9_900));
    assert_eq!(active.managed_brackets[0].status, BracketViewStatus::Active);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let (restarted, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    assert_eq!(
        restarted.snapshot().expect("recovered").ledger_entries,
        active.ledger_entries
    );
    let exit = deliver_quote(&restarted, &feed, &instrument(), 4, 9_675, 9_700);
    assert_eq!(exit.len(), 1);
    assert_eq!(exit[0].price, price(9_675));
    let settled = restarted.snapshot().expect("settled");
    assert_eq!(settled.positions[0].net_quantity, quantity(0));
    assert_eq!(
        settled.managed_brackets[0].status,
        BracketViewStatus::Completed
    );
    for target in settled.orders.iter().filter(|order| {
        order.order_type == OrderType::Limit && order.client_order_id.as_str() != "maker-bracket"
    }) {
        assert_leg_event(
            &settled,
            &target.id,
            OrderEventKind::Cancelled,
            aeris_trading::OrderEventReason::Cascade,
        );
    }
    let entries = settled
        .ledger_entries
        .iter()
        .filter(|entry| entry.account_id == account)
        .map(|entry| (entry.kind, entry.amount.units()))
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        [
            (aeris_trading::LedgerEntryKind::Deposit, 5_000_000),
            (aeris_trading::LedgerEntryKind::Commission, -10),
            (aeris_trading::LedgerEntryKind::RealizedPnl, -22_500),
            (aeris_trading::LedgerEntryKind::Commission, -10),
        ]
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
    let again = start_service(&directory);
    let recovered = again.snapshot().expect("second restart");
    assert_eq!(recovered.orders, settled.orders);
    assert_eq!(recovered.fills, settled.fills);
    assert_eq!(recovered.ledger_entries, settled.ledger_entries);
    assert_eq!(recovered.managed_brackets, settled.managed_brackets);
    again.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn identical_verified_terms_after_restart_do_not_advance_trading_revision() {
    let directory = TestDirectory::new("terms-revision-restart");
    let service = start_service(&directory);
    let terms = instrument();
    service
        .register_instrument(terms.clone())
        .expect("first terms");
    let before = service.snapshot().expect("before restart");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    assert_eq!(
        restarted.snapshot().expect("recovered").revision,
        before.revision
    );
    restarted
        .register_instrument(terms)
        .expect("identical terms");
    assert_eq!(
        restarted.snapshot().expect("replayed terms").revision,
        before.revision,
        "a repeated identical Terms event is not a new durable fact"
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn bracket_off_grid_child_refuses_entry_without_writing_facts() {
    let directory = TestDirectory::new("bracket-grid-preflight");
    let service = start_service(&directory);
    let mut contract = instrument();
    contract.contract.price_grid =
        aeris_instruments::PriceGrid::Fixed(InstrumentDecimal::try_new(50, 2).expect("grid"));
    service.register_instrument(contract).expect("instrument");
    let mut entry = market_order("bracket-grid", OrderSide::Buy, 1, 1_000);
    entry.order_type = OrderType::Limit;
    entry.time_in_force = TimeInForce::GoodTillCancelled;
    entry.limit_price = Some(price(10_000));
    entry.quantity = quantity(2);
    let mut template = fixed_stop_bracket_template();
    template.stop_offset_ticks = 1;
    template.targets[0].offset_ticks = 1;
    template.targets[1].offset_ticks = 2;
    let before = service.snapshot().expect("before");
    assert_eq!(
        service
            .place_inline_bracket(PlaceInlineBracket { entry, template })
            .expect_err("off-grid stop and target refuse the whole group"),
        aeris_trading::RejectReason::OffGrid.to_string()
    );
    assert_eq!(service.snapshot().expect("after"), before);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let reopened = start_service(&directory);
    assert_eq!(reopened.snapshot().expect("restored"), before);
    reopened.shutdown(Duration::from_secs(2)).expect("shutdown");
}

fn place_managed_bracket(service: &TradingService, entry: PlaceOrder) {
    let placement = service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: fixed_stop_bracket_template(),
        })
        .expect("bracket accepted");
    assert!(matches!(placement, PlacementOutcome::Managed(_)));
}

fn quantity(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 0).expect("quantity")
}

#[test]
fn bracket_entry_that_reverses_a_position_protects_only_the_reversed_exposure() {
    let directory = TestDirectory::new("bracket-reversal");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .place_order(market_order("existing-long", OrderSide::Buy, 1, 1_000))
        .expect("long accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "long fills"
    );
    let mut entry = market_order("reversing-entry", OrderSide::Sell, 3, 3_000);
    entry.quantity = quantity(3);
    place_managed_bracket(&service, entry);

    // The reversing entry fills at the retained quote's bid at placement; the next quote is
    // only a mark.
    deliver_quote(&service, &feed, &instrument(), 4, 9_975, 10_000);
    let armed = service.snapshot().expect("armed snapshot");
    assert_eq!(armed.market_observation_error, None);
    assert_eq!(
        armed.positions[0].net_quantity,
        FixedPoint::try_new(-2, 0).expect("short")
    );
    assert_eq!(armed.managed_brackets[0].status, BracketViewStatus::Active);
    let working = armed
        .orders
        .iter()
        .filter(|order| order.status.is_open())
        .collect::<Vec<_>>();
    assert!(working.iter().all(|order| order.side == OrderSide::Buy));
    let stop = working
        .iter()
        .find(|order| order.order_type == OrderType::Stop)
        .expect("protective stop");
    assert_eq!(
        stop.quantity,
        quantity(2),
        "the stop covers only the new short"
    );
    assert_eq!(
        stop.stop_price,
        Some(FixedPoint::try_new(10_175, 2).expect("stop"))
    );
    let targets = working
        .iter()
        .filter(|order| order.order_type == OrderType::Limit)
        .map(|order| order.quantity)
        .collect::<Vec<_>>();
    assert_eq!(targets, vec![quantity(1), quantity(1)]);

    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 5, 10_175, 10_200).len(),
        1,
        "stop fills"
    );
    let stopped = service.snapshot().expect("stopped snapshot");
    assert_eq!(stopped.positions[0].net_quantity.units(), 0);
    assert_eq!(
        stopped.managed_brackets[0].status,
        BracketViewStatus::Completed
    );
    assert!(stopped.orders.iter().all(|order| !order.status.is_open()));
}

#[test]
fn bracket_entry_that_only_reduces_a_position_completes_without_children() {
    let directory = TestDirectory::new("bracket-reduction");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut long = market_order("existing-long", OrderSide::Buy, 1, 1_000);
    long.quantity = quantity(2);
    service.place_order(long).expect("long accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "long fills"
    );
    place_managed_bracket(
        &service,
        market_order("reducing-entry", OrderSide::Sell, 3, 3_000),
    );

    // The reducing entry fills at the retained quote's bid at placement; the next quote is
    // only a mark.
    deliver_quote(&service, &feed, &instrument(), 4, 9_975, 10_000);
    let reduced = service.snapshot().expect("reduced snapshot");
    assert_eq!(reduced.market_observation_error, None);
    assert_eq!(reduced.positions[0].net_quantity, quantity(1));
    let bracket = &reduced.managed_brackets[0];
    assert_eq!(bracket.status, BracketViewStatus::Completed);
    assert_eq!(bracket.stop_client_order_id, None);
    assert_eq!(bracket.target_client_order_ids, []);
    assert!(reduced.orders.iter().all(|order| !order.status.is_open()));
}

#[test]
fn bracket_that_cannot_be_armed_is_rejected_once_and_other_orders_still_fill() {
    let directory = TestDirectory::new("bracket-arming-failure");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    place_managed_bracket(
        &service,
        market_order("unprotectable-entry", OrderSide::Buy, 1, 1_000),
    );
    // Filled at 0.50, the 8-tick stop would sit at -1.50.
    deliver_quote(&service, &feed, &instrument(), 2, 25, 50);
    let rejected = service.snapshot().expect("rejected snapshot");
    assert_eq!(rejected.market_observation_error, None);
    assert_eq!(rejected.positions[0].net_quantity, quantity(1));
    assert_eq!(
        rejected.managed_brackets[0].status,
        BracketViewStatus::Cancelled
    );
    let entry = rejected
        .orders
        .iter()
        .find(|order| order.client_order_id.as_str() == "unprotectable-entry")
        .expect("entry order");
    assert!(rejected.order_events.iter().any(|event| {
        event.order_id == entry.id
            && event.kind == OrderEventKind::Rejected
            && event.detail.as_deref()
                == Some(
                    "LOCAL-MANAGED bracket protection was not placed: \
                     bracket price must remain positive",
                )
    }));

    // The market ticks up before the manual stop is placed, so its 0.50 trigger sits below
    // the current bid instead of crossing the retained entry quote.
    deliver_quote(&service, &feed, &instrument(), 3, 60, 85);
    let mut stop = market_order("manual-stop", OrderSide::Sell, 3, 3_000);
    stop.order_type = OrderType::Stop;
    stop.time_in_force = TimeInForce::GoodTillCancelled;
    stop.stop_price = Some(FixedPoint::try_new(50, 2).expect("on-grid stop"));
    service.place_order(stop).expect("manual stop accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 4, 30, 55).len(),
        1,
        "the rejected bracket is not retried and the manual stop fills"
    );
    let flat = service.snapshot().expect("flat snapshot");
    assert_eq!(flat.market_observation_error, None);
    assert_eq!(flat.positions[0].net_quantity.units(), 0);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = start_service(&directory);
    assert_eq!(
        restarted
            .snapshot()
            .expect("restored snapshot")
            .managed_brackets,
        flat.managed_brackets
    );
}

#[test]
fn bracket_targets_split_in_whole_order_increments() {
    let targets = |percents: &[u8]| {
        percents
            .iter()
            .zip(1..)
            .map(|(&quantity_percent, offset_ticks)| BracketTarget {
                offset_ticks,
                quantity_percent,
            })
            .collect::<Vec<_>>()
    };
    let split = |units: i64, scale: u8, percents: &[u8], increment: i64| {
        allocated_target_quantities(
            FixedPoint::try_new(units, scale).expect("quantity"),
            &targets(percents),
            increment,
        )
        .map(|split| {
            split
                .into_iter()
                .map(|(index, quantity)| (index, quantity.units()))
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(split(5, 0, &[50, 50], 1), Ok(vec![(0, 2), (1, 3)]));
    assert_eq!(split(1, 0, &[50, 50], 1), Ok(vec![(1, 1)]));
    assert_eq!(
        split(10, 0, &[33, 33, 34], 1),
        Ok(vec![(0, 3), (1, 3), (2, 4)])
    );
    assert_eq!(
        split(300_000_000, 8, &[50, 50], 100_000_000),
        Ok(vec![(0, 100_000_000), (1, 200_000_000)])
    );
    assert!(split(150_000_000, 8, &[100], 100_000_000).is_err());
}

#[test]
fn protective_orders_reduce_locked_positions_without_reopening_risk() {
    let directory = TestDirectory::new("protective-order");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("protected-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    service.place_order(entry).expect("entry accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "entry fills"
    );
    service
        .lock_account(
            TradingAccountId::try_new("aeris-sim-1").expect("account"),
            "manual test lock".to_string(),
            3_000,
        )
        .expect("account locks");

    let mut protective = market_order("protective-stop", OrderSide::Sell, 3, 3_000);
    protective.order_type = OrderType::Stop;
    protective.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    protective.stop_price = Some(FixedPoint::try_new(9_800, 2).expect("stop"));
    service
        .place_protective_order(PlaceProtection {
            order: protective,
            role: ProtectionRole::StopLoss,
        })
        .expect("reduce-only protection remains available");
    let locked = service
        .place_order(market_order("locked-entry", OrderSide::Buy, 4, 4_000))
        .expect_err("risk-increasing order remains locked");
    assert!(locked.contains("AccountLocked"), "{locked}");
    assert!(locked.contains("manual test lock"), "{locked}");
    let mut oversized = market_order("oversized-stop", OrderSide::Sell, 5, 5_000);
    oversized.order_type = OrderType::Stop;
    oversized.quantity = FixedPoint::try_new(3, 0).expect("quantity");
    oversized.stop_price = Some(FixedPoint::try_new(9_700, 2).expect("stop"));
    assert!(
        service
            .place_protective_order(PlaceProtection {
                order: oversized,
                role: ProtectionRole::StopLoss,
            })
            .expect_err("oversized protection rejected")
            .contains("must reduce")
    );
    assert_eq!(
        service.snapshot().expect("snapshot").protective_orders,
        vec![ProtectionView {
            client_order_id: ClientOrderId::try_new("protective-stop").expect("client id"),
            role: ProtectionRole::StopLoss,
        }]
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let restarted = start_service(&directory);
    assert_eq!(
        restarted
            .snapshot()
            .expect("snapshot")
            .protective_orders
            .len(),
        1
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn position_protection_reports_the_stop_and_target_that_trigger_first() {
    let directory = TestDirectory::new("position-protection");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("protected-long", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    service.place_order(entry).expect("entry accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "entry fills"
    );
    let protect = |client_id: &str, sequence, role, price_units| {
        let mut order = market_order(client_id, OrderSide::Sell, sequence, 3_000);
        order.quantity = FixedPoint::try_new(1, 0).expect("quantity");
        let price = Some(FixedPoint::try_new(price_units, 2).expect("price"));
        match role {
            ProtectionRole::StopLoss => {
                order.order_type = OrderType::Stop;
                order.stop_price = price;
            }
            ProtectionRole::TakeProfit => {
                order.order_type = OrderType::Limit;
                order.limit_price = price;
            }
        }
        service
            .place_protective_order(PlaceProtection { order, role })
            .expect("protection accepted");
    };
    protect("stop", 3, ProtectionRole::StopLoss, 9_800);
    protect("far-target", 4, ProtectionRole::TakeProfit, 10_300);
    protect("near-target", 5, ProtectionRole::TakeProfit, 10_200);

    let snapshot = service.snapshot().expect("snapshot");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let protection = snapshot.position_protection(&account, &instrument().instrument_id);
    assert_eq!(protection.stop_loss.map(FixedPoint::units), Some(9_800));
    assert_eq!(
        protection.take_profit.map(FixedPoint::units),
        Some(10_200),
        "the nearer of two targets on a long triggers first"
    );
    let other = TradingAccountId::try_new("aeris-sim-2").expect("account");
    assert_eq!(
        snapshot.position_protection(&other, &instrument().instrument_id),
        PositionProtection::default()
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

/// Opens a long position of `quantity` and protects all of it with a stop at 98.00 and a
/// target at 103.00.
fn protected_long(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    quantity: i64,
) {
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    service.place_order(entry).expect("entry accepted");
    assert_eq!(
        deliver_quote(service, feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "entry fills"
    );
    for (client_id, role, price) in [
        ("stop", ProtectionRole::StopLoss, 9_800),
        ("target", ProtectionRole::TakeProfit, 10_300),
    ] {
        let mut order = market_order(client_id, OrderSide::Sell, 3, 3_000);
        order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
        let price = Some(FixedPoint::try_new(price, 2).expect("price"));
        if role == ProtectionRole::StopLoss {
            order.order_type = OrderType::Stop;
            order.stop_price = price;
        } else {
            order.order_type = OrderType::Limit;
            order.limit_price = price;
        }
        service
            .place_protective_order(PlaceProtection { order, role })
            .expect("protection accepted");
    }
}

/// The quantity of each open protective order, by client identifier.
fn open_protection(service: &TradingService) -> Vec<(String, i64)> {
    service
        .snapshot()
        .expect("snapshot")
        .orders
        .into_iter()
        .filter(|order| {
            order.status.is_open() && ["stop", "target"].contains(&order.client_order_id.as_str())
        })
        .map(|order| {
            (
                order.client_order_id.as_str().to_string(),
                order.quantity.units(),
            )
        })
        .collect()
}

fn market(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    client_id: &str,
    side: OrderSide,
    quantity: i64,
    step: u64,
) {
    let time = i64::try_from(step).expect("step") * 1_000;
    let mut order = market_order(client_id, side, step, time);
    order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    let before = service.snapshot().expect("snapshot before placement").fills;
    service.place_order(order).expect("order accepted");
    // With a retained quote the market order fills at its placement; after a restart the
    // owner book is empty, so the delivered quote fills the resting order instead. Either
    // way exactly one new fill commits across the placement and the quote.
    deliver_quote(service, feed, &instrument(), step + 1, 9_975, 10_000);
    let after = service.snapshot().expect("snapshot after the quote").fills;
    assert_eq!(
        after.iter().filter(|fill| !before.contains(fill)).count(),
        1,
        "order fills"
    );
}

#[test]
fn protection_follows_the_position_as_it_grows_shrinks_and_closes() {
    let directory = TestDirectory::new("protection-follows-position");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    protected_long(&service, &feed, 2);

    market(&service, &feed, "add", OrderSide::Buy, 4, 4);
    let grown = vec![("stop".to_string(), 6), ("target".to_string(), 6)];
    let mut protection = open_protection(&service);
    protection.sort();
    assert_eq!(
        protection, grown,
        "adding to the position grows its stop and target"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let (service, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    let mut protection = open_protection(&service);
    protection.sort();
    assert_eq!(protection, grown, "the grown protection survives a restart");

    market(&service, &feed, "trim", OrderSide::Sell, 2, 6);
    let mut protection = open_protection(&service);
    protection.sort();
    assert_eq!(
        protection,
        vec![("stop".to_string(), 4), ("target".to_string(), 4)],
        "a partial close shrinks the protection"
    );

    market(&service, &feed, "exit", OrderSide::Sell, 4, 8);
    assert_eq!(
        open_protection(&service),
        Vec::<(String, i64)>::new(),
        "closing the position cancels its protection"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn a_filled_stop_cancels_the_target_and_a_reversal_cancels_protection() {
    let directory = TestDirectory::new("protection-stop-and-reversal");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    protected_long(&service, &feed, 2);
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 4, 9_775, 9_800).len(),
        1,
        "stop fills"
    );
    assert_eq!(
        open_protection(&service),
        Vec::<(String, i64)>::new(),
        "the target of a stopped-out position is cancelled"
    );
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.positions[0].net_quantity.units(), 0);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let directory = TestDirectory::new("protection-reversal");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    protected_long(&service, &feed, 2);
    market(&service, &feed, "reverse", OrderSide::Sell, 5, 4);
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.positions[0].net_quantity.units(), -3);
    assert_eq!(
        open_protection(&service),
        Vec::<(String, i64)>::new(),
        "a sell protection cannot protect the short a reversal opened"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn intraday_trailing_drawdown_tracks_peak_on_every_market_observation() {
    use crate::clock::Clock;
    let directory = TestDirectory::new("intraday-drawdown");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "intraday-100".to_string(),
            version: 1,
            // The session starts at the deterministic clock so no session boundary passes
            // during the test.
            session_start_unix_nanos: clock.wall_unix_nanos(),
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("daily loss"),
            trailing_drawdown: Some(FixedPoint::try_new(10_000, 2).expect("drawdown")),
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(5, 0).expect("contracts"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("profile registers");
    service
        .place_order(market_order("drawdown-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "entry fills"
    );
    deliver_quote(&service, &feed, &instrument(), 3, 10_300, 10_325);
    let peak = service.snapshot().expect("peak snapshot");
    assert_eq!(
        peak.risk_rule_states[0].peak_session_pnl,
        FixedPoint::try_new(15_000, 2).expect("peak")
    );
    assert_eq!(
        peak.risk_meters[0].trailing_drawdown_remaining,
        Some(FixedPoint::try_new(10_000, 2).expect("remaining"))
    );
    deliver_quote(&service, &feed, &instrument(), 4, 10_100, 10_125);
    let locked = service.snapshot().expect("locked snapshot");
    assert!(locked.risk_locks[0].reason.contains("intraday trailing"));
    assert_eq!(
        locked.risk_meters[0].trailing_drawdown_remaining,
        Some(FixedPoint::try_new(0, 2).expect("remaining"))
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    // Restart on the same deterministic clock: a system-clock restart would roll the risk
    // session across every boundary since the fixture epoch and reset the trailing peak.
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let restarted = start_service_with_config(configuration);
    let restored = restarted.snapshot().expect("snapshot");
    assert_eq!(restored.risk_rule_states, locked.risk_rule_states);
    assert_eq!(restored.risk_locks, locked.risk_locks);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn maximum_contracts_counts_all_working_order_scenarios() {
    let directory = TestDirectory::new("working-exposure");
    let (service, _clock, _feed) = quoted_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_000,
        10_025,
    );
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "two-contracts".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("daily loss"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::EndOfDay,
            max_contracts: FixedPoint::try_new(2, 0).expect("contracts"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("profile registers");
    for (id, sequence) in [("working-one", 1), ("working-two", 2)] {
        let mut order = market_order(
            id,
            OrderSide::Buy,
            sequence,
            i64::try_from(sequence).expect("sequence fits") * 1_000,
        );
        order.order_type = OrderType::Limit;
        order.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
        // A verified route derives a DAY deadline from the acceptance session, so a restart
        // on the system clock expires DAY orders before the event comparison below. The
        // scenario counts resting orders, so they rest without a session deadline.
        order.time_in_force = TimeInForce::GoodTillCancelled;
        service.place_order(order).expect("working order accepted");
    }
    assert!(
        service
            .snapshot()
            .expect("snapshot")
            .order_events
            .iter()
            .filter(|event| event.kind == OrderEventKind::Accepted)
            .all(
                |event| event.reason == Some(aeris_trading::OrderEventReason::RiskWarning)
                    && event
                        .detail
                        .as_deref()
                        .is_some_and(|detail| !detail.starts_with("RISK WARNING"))
            )
    );
    assert_eq!(
        service.snapshot().expect("snapshot").risk_meters[0].contracts_remaining,
        FixedPoint::try_new(0, 0).expect("remaining")
    );
    let mut third = market_order("working-three", OrderSide::Buy, 3, 3_000);
    third.order_type = OrderType::Limit;
    third.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
    third.time_in_force = TimeInForce::GoodTillCancelled;
    assert!(
        service
            .place_order(third)
            .expect_err("third working order exceeds the limit")
            .contains("maximum-contract")
    );
    let events = service.snapshot().expect("before restart").order_events;
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let restarted = start_service(&directory);
    assert_eq!(
        restarted
            .snapshot()
            .expect("typed reason restored")
            .order_events,
        events
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn bracket_is_blocked_when_its_stop_would_reach_a_loss_limit() {
    let directory = TestDirectory::new("bracket-stop-risk");
    let (service, _clock, _feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let account_id = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let profile = |version, daily_loss_units| RiskProfile {
        account_id: account_id.clone(),
        profile_id: "bracket-risk".to_string(),
        version,
        session_start_unix_nanos: 1,
        session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
        daily_loss_limit: FixedPoint::try_new(daily_loss_units, 2).expect("daily loss"),
        trailing_drawdown: None,
        trailing_mode: TrailingDrawdownMode::EndOfDay,
        max_contracts: FixedPoint::try_new(2, 0).expect("contracts"),
        consistency_max_single_trade_percent: None,
        restricted_until_unix_nanos: None,
        economic_event_rule: None,
        enabled: true,
    };
    service
        .register_risk_profile(profile(1, 20_000))
        .expect("profile registers");
    let mut blocked_entry = market_order("blocked-bracket", OrderSide::Buy, 1, 1_000);
    blocked_entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    assert!(
        service
            .place_inline_bracket(PlaceInlineBracket {
                entry: blocked_entry,
                template: bracket_template(),
            })
            .expect_err("eight ES ticks equal the complete two-hundred-dollar budget")
            .contains("loss at stop")
    );
    service
        .register_risk_profile(profile(2, 20_001))
        .expect("profile revision registers");
    let mut accepted_entry = market_order("accepted-bracket", OrderSide::Buy, 2, 2_000);
    accepted_entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: accepted_entry,
            template: bracket_template(),
        })
        .expect("risk budget above the stop loss accepts the bracket");
    assert!(
        service
            .register_risk_profile(profile(2, 30_000))
            .expect_err("profile revisions cannot regress")
            .contains("revision must advance")
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn consistency_rule_tracks_completed_trades_atomically_across_restart() {
    let directory = TestDirectory::new("consistency");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    // An open ticket keeps the quote subscribed while the position is flat between trades.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "fifty-percent-consistency".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("daily loss"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::EndOfDay,
            max_contracts: FixedPoint::try_new(2, 0).expect("contracts"),
            consistency_max_single_trade_percent: Some(50),
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("profile registers");

    for (entry_id, exit_id, sequence) in [
        ("winner-one-entry", "winner-one-exit", 1_u64),
        ("winner-two-entry", "winner-two-exit", 5_u64),
    ] {
        let sequence_time = i64::try_from(sequence).expect("sequence fits");
        // Each leg's quote is delivered before its placement, so the market order fills at
        // that quote's touch.
        deliver_quote(&service, &feed, &instrument(), sequence + 1, 9_975, 10_000);
        service
            .place_order(market_order(
                entry_id,
                OrderSide::Buy,
                sequence,
                1_000 * sequence_time,
            ))
            .expect("entry accepted");
        deliver_quote(&service, &feed, &instrument(), sequence + 3, 10_100, 10_125);
        service
            .place_order(market_order(
                exit_id,
                OrderSide::Sell,
                sequence + 2,
                1_000 * (sequence_time + 2),
            ))
            .expect("exit accepted");

        let snapshot = service.snapshot().expect("snapshot");
        let completed = if sequence == 1 { 1 } else { 2 };
        assert_eq!(
            snapshot.risk_rule_states[0].total_winning_pnl,
            FixedPoint::try_new(5_000 * completed, 2).expect("gross winning P/L")
        );
        assert_eq!(
            snapshot.risk_rule_states[0].largest_winning_trade_pnl,
            FixedPoint::try_new(5_000, 2).expect("largest winner")
        );
        assert_eq!(
            snapshot.risk_meters[0].consistency_current_percent,
            Some(if sequence == 1 { 100 } else { 50 })
        );
        assert_eq!(
            snapshot.risk_meters[0].consistency_additional_profit_required,
            Some(
                FixedPoint::try_new(if sequence == 1 { 5_000 } else { 0 }, 2)
                    .expect("required profit")
            )
        );
    }
    let before_restart = service.snapshot().expect("snapshot").risk_rule_states;
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let restarted = start_service(&directory);
    assert_eq!(
        restarted.snapshot().expect("snapshot").risk_rule_states,
        before_restart
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn session_plan_enforces_checklist_hours_and_maximum_loss_across_restart() {
    let directory = TestDirectory::new("session-plan");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let account_id = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let plan = |revision: u32, completed: bool| SessionPlan {
        account_id: account_id.clone(),
        plan_id: "morning-plan".to_string(),
        revision,
        session_start_unix_nanos: 500,
        session_end_unix_nanos: 10_000,
        bias: SessionBias::Long,
        maximum_loss: FixedPoint::try_new(5_000, 2).expect("maximum loss"),
        session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
        allowed_setups: vec!["opening-drive".to_string()],
        active_setup: completed.then(|| "opening-drive".to_string()),
        checklist: vec![SessionChecklistItem {
            item_id: "news-reviewed".to_string(),
            label: "Review scheduled news".to_string(),
            completed,
        }],
        levels: vec![SessionPlanLevel {
            instrument_id: instrument().instrument_id,
            label: "overnight high".to_string(),
            price: FixedPoint::try_new(10_250, 2).expect("level"),
        }],
    };
    service
        .register_session_plan(plan(1, false))
        .expect("draft plan registers");
    assert!(
        service
            .place_order(market_order("plan-not-ready", OrderSide::Buy, 1, 1_000))
            .expect_err("incomplete checklist blocks entry")
            .contains("checklist")
    );
    service
        .register_session_plan(plan(2, true))
        .expect("ready plan revision registers");
    assert!(
        service
            .place_order(market_order("outside-hours", OrderSide::Buy, 2, 11_000))
            .expect_err("outside-hours trade is blocked")
            .contains("outside the session plan")
    );
    service
        .place_order(market_order("planned-entry", OrderSide::Buy, 3, 1_000))
        .expect("planned entry accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 4, 9_975, 10_000).len(),
        1,
        "entry fills"
    );
    // The losing quote is delivered before the exit placement, so the resting market exit
    // executes at its touch and the plan lock evaluates.
    deliver_quote(&service, &feed, &instrument(), 6, 9_900, 9_925);
    service
        .place_order(market_order("planned-loss", OrderSide::Sell, 5, 3_000))
        .expect("planned exit accepted");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.session_plans, vec![plan(2, true)]);
    assert_eq!(snapshot.session_adherence_reviews.len(), 1);
    assert!(!snapshot.session_adherence_reviews[0].maximum_loss_respected);
    assert!(
        snapshot.risk_locks[0]
            .reason
            .contains("session plan maximum loss")
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let restarted = start_service(&directory);
    let restored = restarted.snapshot().expect("snapshot");
    assert_eq!(restored.session_plans, snapshot.session_plans);
    assert_eq!(
        restored.session_adherence_reviews,
        snapshot.session_adherence_reviews
    );
    assert_eq!(restored.risk_locks, snapshot.risk_locks);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn tilt_rules_apply_size_reduction_and_restart_safe_rapid_loss_cooldown() {
    let directory = TestDirectory::new("tilt-cooldown");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    // An open ticket keeps the quote subscribed while the position is flat between trades.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");

    let complete_losing_trade = |service: &TradingService,
                                 feed: &crate::core::market::feed::MarketFeedInbox,
                                 prefix: &str,
                                 sequence: u64| {
        // Each leg's quote is delivered before its placement, so the market order fills
        // at that quote's touch.
        deliver_quote(service, feed, &instrument(), sequence + 1, 9_975, 10_000);
        service
            .place_order(market_order(
                &format!("{prefix}-entry"),
                OrderSide::Buy,
                sequence,
                i64::try_from(sequence).expect("sequence fits"),
            ))
            .expect("entry accepted");
        deliver_quote(service, feed, &instrument(), sequence + 3, 9_900, 9_925);
        service
            .place_order(market_order(
                &format!("{prefix}-exit"),
                OrderSide::Sell,
                sequence + 2,
                i64::try_from(sequence + 2).expect("sequence fits"),
            ))
            .expect("exit accepted");
    };
    complete_losing_trade(&service, &feed, "loss-one", 1);
    let mut larger = market_order("larger-after-loss", OrderSide::Buy, 5, 5);
    larger.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    assert!(
        service
            .place_order(larger)
            .expect_err("size increase after a loss is reduced")
            .contains("tilt size reduction")
    );
    complete_losing_trade(&service, &feed, "loss-two", 6);
    let locked = service.snapshot().expect("snapshot");
    assert_eq!(locked.discipline_states[0].rapid_loss_count, 2);
    let cooldown_until = locked.discipline_states[0]
        .cooldown_until_unix_nanos
        .expect("cooldown");
    assert!(locked.risk_locks[0].reason.contains("rapid-loss cooldown"));
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let (restarted, _feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    assert!(
        restarted
            .place_order(market_order(
                "cooldown-reject",
                OrderSide::Buy,
                10,
                cooldown_until - 1,
            ))
            .expect_err("cooldown survives restart")
            .contains("risk-locked")
    );
    restarted
        .place_order(market_order(
            "cooldown-expired",
            OrderSide::Buy,
            11,
            cooldown_until,
        ))
        .expect("cooldown expires at its exact boundary");
    assert_eq!(
        restarted.snapshot().expect("snapshot").discipline_states[0].cooldown_until_unix_nanos,
        None
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn filled_stop_produces_a_fast_reentry_warning() {
    let directory = TestDirectory::new("stop-reentry");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let template = BracketStrategyTemplate {
        template_id: "single-target-stop".to_string(),
        revision: 1,
        name: "Single target stop".to_string(),
        stop_offset_ticks: 8,
        targets: vec![BracketTarget {
            offset_ticks: 16,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    };
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("stopped-entry", OrderSide::Buy, 1, 1_000_000_000),
            template,
        })
        .expect("bracket accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "entry fills"
    );
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 3, 9_775, 9_800).len(),
        1,
        "stop fills"
    );
    let evaluation = service
        .evaluate_risk(market_order(
            "fast-reentry",
            OrderSide::Buy,
            4,
            // One tick after the stop's fill time, well inside the 60-second re-entry window.
            1_000_004_000_000,
        ))
        .expect("warning does not silently reject a one-off re-entry");
    assert!(
        evaluation
            .warnings
            .contains(&aeris_trading::RiskWarning::FastStopReentry)
    );
    assert!(
        !order_acceptance_detail(&evaluation.warnings).contains("RISK WARNING"),
        "display text must not be a control-flow discriminator"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn management_moves_the_stop_only_at_one_tick_or_more() {
    let directory = TestDirectory::new("management-sub-tick");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    // One trailing rule: activate 12 ticks above the entry, then trail 6 ticks behind.
    let template = BracketStrategyTemplate {
        template_id: "managed-sub-tick".to_string(),
        revision: 1,
        name: "Managed single target".to_string(),
        stop_offset_ticks: 8,
        targets: vec![BracketTarget {
            offset_ticks: 100,
            quantity_percent: 100,
        }],
        trailing_stop: Some(TrailingStopRule {
            activation_ticks: 12,
            distance_ticks: 6,
        }),
        break_even: None,
        enabled: true,
    };
    let entry_client = ClientOrderId::try_new("managed-entry").expect("client id");
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("managed-entry", OrderSide::Buy, 1, 1_000_000_000),
            template,
        })
        .expect("bracket accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 9_975, 10_000).len(),
        1,
        "entry fills at 100.00; stop starts at 98.00"
    );
    let stop_client = managed_child_client_order_id(&entry_client, "stop", 0).expect("stop id");
    let stop_price = |service: &TradingService| {
        service
            .snapshot()
            .expect("snapshot")
            .orders
            .into_iter()
            .find(|order| order.client_order_id == stop_client)
            .expect("stop order")
            .stop_price
    };
    // A 13-tick run activates the trail: the stop moves to bid - 6 ticks = 101.75.
    deliver_quote(&service, &feed, &instrument(), 3, 10_325, 10_350);
    assert_eq!(
        stop_price(&service),
        FixedPoint::try_new(10_175, 2).ok(),
        "the trail tightens the stop to 101.75"
    );

    // A sub-tick further move (0.05 of a 0.25 tick) writes no event and no transaction.
    let settled = service.snapshot().expect("settled snapshot");
    deliver_quote(&service, &feed, &instrument(), 4, 10_330, 10_355);
    let after_sub_tick = service.snapshot().expect("sub-tick snapshot");
    assert_eq!(
        (
            after_sub_tick.revision,
            after_sub_tick.order_events.len(),
            stop_price(&service),
        ),
        (
            settled.revision,
            settled.order_events.len(),
            FixedPoint::try_new(10_175, 2).ok(),
        ),
        "a sub-tick move produces no event and no transaction"
    );

    // A full further tick tightens the stop again with a management event.
    deliver_quote(&service, &feed, &instrument(), 5, 10_350, 10_375);
    let after_tick = service.snapshot().expect("one-tick snapshot");
    assert_eq!(
        stop_price(&service),
        FixedPoint::try_new(10_200, 2).ok(),
        "the trail follows a full tick"
    );
    let stop_id = after_tick
        .orders
        .iter()
        .find(|order| order.client_order_id == stop_client)
        .expect("stop order")
        .id
        .clone();
    assert_eq!(
        after_tick.order_events.len(),
        settled.order_events.len() + 1,
        "exactly one management event is written"
    );
    assert_leg_event(
        &after_tick,
        &stop_id,
        OrderEventKind::Modified,
        aeris_trading::OrderEventReason::Management,
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

fn replace_risk_policy_mid_trade(service: &TradingService) {
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "mid-trade-policy".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("session baseline"),
            daily_loss_limit: FixedPoint::try_new(1_000_000, 2).expect("loss limit"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(100, 0).expect("contract limit"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: false,
        })
        .expect("changing risk policy mid-trade does not reset trade P&L");
}

/// One entry and one full exit: the exit carries both the trade's final P&L and the P&L it
/// realized, and the entry carries neither.
fn assert_completed_trade_pnl(snapshot: &TradingSnapshot, expected: FixedPoint) {
    assert_eq!(snapshot.completed_trade_pnl.len(), 1);
    assert_eq!(
        snapshot.completed_trade_pnl.get(&snapshot.fills[0].id),
        Some(&expected)
    );
    assert!(
        !snapshot
            .completed_trade_pnl
            .contains_key(&snapshot.fills[1].id)
    );
    assert_eq!(
        snapshot.fill_realized_pnl,
        BTreeMap::from([(snapshot.fills[0].id.clone(), expected)]),
        "only the exit realizes P&L"
    );
}

#[test]
fn simulated_execution_and_records_survive_restart_and_export() {
    let directory = TestDirectory::new("restart");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let first = service
        .place_order(market_order("client-open", OrderSide::Buy, 1, 1_000))
        .expect("buy accepted");
    assert_eq!(first.status, OrderStatus::Working);
    let duplicate = service
        .place_order(market_order("client-open", OrderSide::Buy, 2, 1_001))
        .expect("duplicate is idempotent");
    assert_eq!(duplicate.id, first.id);
    let fills = deliver_quote(&service, &feed, &instrument(), 3, 9_975, 10_000);
    assert_eq!(fills.len(), 1);
    let open_snapshot = service.snapshot().expect("snapshot");
    assert!(open_snapshot.completed_trade_pnl.is_empty());
    assert_eq!(open_snapshot.orders[0].status, OrderStatus::Filled);
    assert_filled_lifecycle(&first, &open_snapshot.orders[0]);
    assert_eq!(
        open_snapshot.positions[0].unrealized_pnl,
        FixedPoint::try_new(-1_250, 2).expect("unrealized pnl")
    );
    assert_eq!(
        open_snapshot.position_pnl[0].unrealized_ticks,
        Some(FixedPoint::try_new(-1, 0).expect("unrealized ticks"))
    );
    replace_risk_policy_mid_trade(&service);
    service
        .shutdown(Duration::from_secs(2))
        .expect("open trade survives a clean shutdown");
    let (service, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    service
        .place_order(market_order("client-close", OrderSide::Sell, 4, 3_000))
        .expect("sell accepted");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 5, 10_100, 10_125).len(),
        1,
        "sell fills"
    );
    service
        .put_user_record(UserRecord {
            id: "journal-1".to_string(),
            kind: UserRecordKind::JournalEntry,
            revision: 1,
            updated_unix_nanos: 5_000,
            json: r#"{"note":"disciplined trade"}"#.to_string(),
        })
        .expect("journal stores");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.fills.len(), 2);
    assert_completed_trade_pnl(
        &snapshot,
        FixedPoint::try_new(5_000, 2).expect("final trade pnl"),
    );
    assert_eq!(snapshot.positions.len(), 1);
    assert_eq!(snapshot.positions[0].net_quantity.units(), 0);
    assert_eq!(
        snapshot.positions[0].realized_pnl,
        FixedPoint::try_new(5_000, 2).expect("pnl")
    );
    assert_eq!(
        snapshot.position_pnl[0].realized_ticks,
        Some(FixedPoint::try_new(4, 0).expect("realized ticks"))
    );
    service
        .export(directory.0.join("export"))
        .expect("export succeeds");
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = start_service(&directory);
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.orders.len(), 2);
    assert_eq!(restored.orders, snapshot.orders);
    assert!(
        restored
            .orders
            .iter()
            .all(|order| order.filled_quantity == order.quantity)
    );
    assert_eq!(restored.fills.len(), 2);
    assert_eq!(restored.completed_trade_pnl, snapshot.completed_trade_pnl);
    assert_eq!(restored.fill_realized_pnl, snapshot.fill_realized_pnl);
    assert_eq!(restored.positions, snapshot.positions);
    assert!(
        fs::read_to_string(directory.0.join("export/executions.csv"))
            .expect("CSV export")
            .contains("sim-fill")
    );
    assert!(
        fs::read_to_string(directory.0.join("export/user_records.json"))
            .expect("JSON export")
            .contains("disciplined trade")
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

fn assert_filled_lifecycle(accepted: &Order, filled: &Order) {
    assert_eq!(filled.revision, accepted.revision + 1);
    assert!(filled.updated_unix_nanos >= accepted.accepted_unix_nanos);
    assert_eq!(filled.terminal_unix_nanos, Some(filled.updated_unix_nanos));
    assert_eq!(filled.filled_quantity, filled.quantity);
}

/// Fills one market order at the observed touch: buys at `ask`, sells at `bid`.
fn execute_market_order(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    step: u64,
    side: OrderSide,
    quantity: i64,
    (bid, ask): (i64, i64),
) {
    let sequence = step * 2 + 1;
    let time = i64::try_from(sequence).expect("time") * 1_000;
    let mut order = market_order(&format!("round-trip-{step}"), side, sequence, time);
    order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    // The quote is delivered before the placement, so the market order fills at its touch
    // whether the owner book retained the previous quote or was waiting for one.
    deliver_quote(service, feed, &instrument(), sequence + 1, bid, ask);
    let placed = service.place_order(order).expect("order accepted");
    assert_eq!(placed.status, OrderStatus::Filled, "the market order fills");
}

fn price(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 2).expect("price")
}

fn contracts(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 0).expect("quantity")
}

#[test]
fn entry_and_exit_fills_form_one_round_trip_with_its_final_pnl() {
    let directory = TestDirectory::new("round-trip");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    execute_market_order(&service, &feed, 1, OrderSide::Buy, 1, (9_975, 10_000));
    let open = service.snapshot().expect("open snapshot");
    assert_eq!(open.round_trips.len(), 1);
    assert!(!open.round_trips[0].closed);
    assert_eq!(open.round_trips[0].final_pnl, None);
    assert_eq!(open.round_trips[0].exit, None);

    execute_market_order(&service, &feed, 2, OrderSide::Sell, 1, (10_100, 10_125));
    let snapshot = service.snapshot().expect("closed snapshot");
    assert_eq!(snapshot.fills.len(), 2);
    let [trip] = snapshot.round_trips.as_slice() else {
        panic!("one round trip, not one row per fill");
    };
    assert_eq!(trip.side, OrderSide::Buy);
    assert!(trip.closed);
    let entry = trip.entry.expect("entry");
    let exit = trip.exit.expect("exit");
    assert_eq!(
        entry.first_unix_nanos,
        snapshot.fills[1].execution_unix_nanos
    );
    assert_eq!(exit.last_unix_nanos, snapshot.fills[0].execution_unix_nanos);
    assert_eq!(
        (entry.average_price, entry.quantity),
        (price(10_000), contracts(1))
    );
    assert_eq!(
        (exit.average_price, exit.quantity),
        (price(10_100), contracts(1))
    );
    assert_eq!(
        trip.final_pnl,
        Some(FixedPoint::try_new(5_000, 2).expect("pnl"))
    );
    assert_eq!(trip.open_pnl, None);
    assert_eq!(trip.current_pnl(), trip.final_pnl);
}

#[test]
fn open_round_trip_pnl_follows_live_marks_and_partial_exits() {
    let directory = TestDirectory::new("open-round-trip-pnl");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");

    execute_market_order(&service, &feed, 1, OrderSide::Buy, 2, (9_975, 10_000));
    let snapshot = service.snapshot().expect("open snapshot");
    assert_eq!(snapshot.round_trips[0].current_pnl(), Some(price(-2_500)));
    assert_eq!(snapshot.round_trips[0].final_pnl, None);

    deliver_quote(&service, &feed, &instrument(), 5, 10_100, 10_125);
    let snapshot = service.snapshot().expect("marked snapshot");
    assert_eq!(snapshot.round_trips[0].current_pnl(), Some(price(10_000)));

    execute_market_order(&service, &feed, 2, OrderSide::Sell, 1, (10_100, 10_125));
    let snapshot = service.snapshot().expect("partial exit snapshot");
    let open_trade = |trips: &[TradeRoundTrip]| {
        let [open] = trips.iter().filter(|trip| !trip.closed).collect::<Vec<_>>()[..] else {
            panic!("one open trade, got {trips:?}");
        };
        open.clone()
    };
    let closing_pnl = |trips: &[TradeRoundTrip]| {
        trips
            .iter()
            .filter(|trip| trip.closed)
            .map(|trip| (trip.exit.map(|exit| exit.quantity), trip.final_pnl))
            .collect::<Vec<_>>()
    };
    let open = open_trade(&snapshot.round_trips);
    assert_eq!(open.current_pnl(), Some(price(10_000)));
    assert_eq!(open.final_pnl, None);
    assert_eq!(
        closing_pnl(&snapshot.round_trips),
        [(Some(contracts(1)), Some(price(5_000)))],
        "the partial exit is history as soon as it fills"
    );
    let account = &snapshot.account_pnl[0];
    assert_eq!(account.realized, price(5_000));
    assert_eq!(account.unrealized, Some(price(5_000)));
    assert_eq!(
        account
            .balance
            .zip(account.unrealized)
            .and_then(|(balance, unrealized)| balance.checked_add(unrealized).ok()),
        account.equity,
        "equity is the balance plus open P&L"
    );

    deliver_quote(&service, &feed, &instrument(), 7, 10_200, 10_225);
    let snapshot = service.snapshot().expect("updated mark snapshot");
    assert_eq!(
        open_trade(&snapshot.round_trips).current_pnl(),
        Some(price(15_000))
    );

    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let (restarted, restarted_feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    let snapshot = restarted.snapshot().expect("restored open snapshot");
    // The last quote mark is process-local; a new feed quote refreshes it after restart.
    assert_eq!(snapshot.positions[0].net_quantity, contracts(1));
    deliver_quote(
        &restarted,
        &restarted_feed,
        &instrument(),
        8,
        10_200,
        10_225,
    );
    let snapshot = restarted.snapshot().expect("refreshed open snapshot");
    assert_eq!(
        open_trade(&snapshot.round_trips).current_pnl(),
        Some(price(15_000))
    );

    execute_market_order(
        &restarted,
        &restarted_feed,
        3,
        OrderSide::Sell,
        1,
        (10_200, 10_225),
    );
    let snapshot = restarted.snapshot().expect("closed snapshot");
    assert!(snapshot.round_trips.iter().all(|trip| trip.closed));
    assert_eq!(
        closing_pnl(&snapshot.round_trips),
        [
            (Some(contracts(1)), Some(price(10_000))),
            (Some(contracts(1)), Some(price(5_000))),
        ],
        "each exit is its own closed trade, newest first"
    );
}

#[test]
fn scale_ins_partial_exits_and_reversals_group_by_position_cycle() {
    let directory = TestDirectory::new("round-trip-reversal");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    // An open ticket keeps the quote subscribed while the position is flat between fills.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");
    execute_market_order(&service, &feed, 1, OrderSide::Buy, 1, (9_975, 10_000));
    execute_market_order(&service, &feed, 2, OrderSide::Buy, 1, (10_075, 10_100));
    // Closes the two-lot long and opens a one-lot short in the same fill.
    execute_market_order(&service, &feed, 3, OrderSide::Sell, 3, (10_200, 10_225));
    execute_market_order(&service, &feed, 4, OrderSide::Buy, 1, (10_075, 10_100));
    let snapshot = service.snapshot().expect("snapshot");
    let [short, long] = snapshot.round_trips.as_slice() else {
        panic!("two round trips, got {:?}", snapshot.round_trips);
    };

    assert_eq!(long.side, OrderSide::Buy);
    let long_entry = long.entry.expect("long entry");
    let long_exit = long.exit.expect("long exit");
    assert_eq!(
        (long_entry.average_price, long_entry.quantity),
        (price(10_050), contracts(2))
    );
    assert_eq!(
        (long_exit.average_price, long_exit.quantity),
        (price(10_200), contracts(2))
    );
    assert_eq!(
        long.final_pnl,
        Some(FixedPoint::try_new(15_000, 2).expect("long pnl"))
    );

    assert_eq!(short.side, OrderSide::Sell);
    let short_entry = short.entry.expect("short entry");
    let short_exit = short.exit.expect("short exit");
    assert_eq!(
        (short_entry.average_price, short_entry.quantity),
        (price(10_200), contracts(1))
    );
    assert_eq!(
        (short_exit.average_price, short_exit.quantity),
        (price(10_100), contracts(1))
    );
    assert_eq!(
        short.final_pnl,
        Some(FixedPoint::try_new(5_000, 2).expect("short pnl"))
    );
    assert!(long.closed && short.closed);
}

#[test]
fn v21_reversal_and_scale_out_accounting_agrees_across_views_and_restart() {
    use crate::clock::Clock;
    let directory = TestDirectory::new("v21-fill-accounting");
    let (service, clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument");
    // An open ticket keeps the quote subscribed while the position is flat between fills.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "accounting-agreement".to_string(),
            version: 1,
            // The session starts at the deterministic clock so no session boundary passes
            // during the test.
            session_start_unix_nanos: clock.wall_unix_nanos(),
            session_start_realized_pnl: price(0),
            daily_loss_limit: price(100_000),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::EndOfDay,
            max_contracts: contracts(10),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("risk profile");
    let steps = [
        (1, OrderSide::Buy, 2, (9_975, 10_000), 0),
        (2, OrderSide::Sell, 3, (10_100, 10_125), 10_000),
        (3, OrderSide::Buy, 1, (9_875, 9_900), 20_000),
        (4, OrderSide::Buy, 3, (9_975, 10_000), 20_000),
        (5, OrderSide::Sell, 1, (10_100, 10_125), 25_000),
        (6, OrderSide::Sell, 2, (10_200, 10_225), 45_000),
    ];
    for (step, side, quantity, quote, expected) in steps {
        execute_market_order(&service, &feed, step, side, quantity, quote);
        let snapshot = service.snapshot().expect("snapshot");
        let expected = price(expected);
        let journal = snapshot
            .fill_realized_pnl
            .values()
            .try_fold(price(0), |sum, pnl| sum.checked_add(*pnl))
            .expect("journal total");
        let trips = snapshot
            .round_trips
            .iter()
            .filter_map(|trip| trip.final_pnl)
            .try_fold(price(0), FixedPoint::checked_add)
            .expect("round-trip total");
        assert_eq!(journal, expected, "journal after step {step}");
        assert_eq!(trips, expected, "round trips after step {step}");
        assert_eq!(
            snapshot.account_pnl[0].realized, expected,
            "account after step {step}"
        );
        assert_eq!(
            snapshot.risk_meters[0].current_realized_pnl, expected,
            "risk after step {step}"
        );
    }
    let before = service.snapshot().expect("before restart");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    assert_v21_accounting_rows(&directory);
    // Restart on the same deterministic clock: a system-clock restart would roll the risk
    // session across every boundary since the fixture epoch and re-baseline realized P&L.
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let restarted = start_service_with_config(configuration);
    let after = restarted.snapshot().expect("after restart");
    assert_eq!(after.fill_realized_pnl, before.fill_realized_pnl);
    assert_eq!(after.completed_trade_pnl, before.completed_trade_pnl);
    assert_eq!(after.round_trips, before.round_trips);
    assert_eq!(after.account_pnl, before.account_pnl);
    assert_eq!(after.risk_meters, before.risk_meters);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

fn assert_v21_accounting_rows(directory: &TestDirectory) {
    let database = rusqlite::Connection::open(directory.0.join("trading.sqlite3"))
        .expect("inspect committed accounting");
    let row_count: i64 = database
        .query_row("SELECT COUNT(*) FROM fill_accounting", [], |row| row.get(0))
        .expect("accounting rows");
    assert_eq!(row_count, 6);
    let cycle_ids: String = database
        .query_row(
            "SELECT group_concat(cycle_id, ',') FROM (
             SELECT accounting.cycle_id FROM fill_accounting accounting
             JOIN fills ON fills.id=accounting.fill_id
             ORDER BY fills.execution_unix_nanos
         )",
            [],
            |row| row.get(0),
        )
        .expect("cycle identities");
    assert_eq!(
        cycle_ids, "0,0,1,2,2,2",
        "the reversal closes its original cycle and opens the next"
    );
    let legacy_writes: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM fills WHERE completed_trade_pnl_units IS NOT NULL
            OR realized_pnl_units IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("legacy fill columns");
    assert_eq!(legacy_writes, 0, "V21 never writes superseded P&L columns");
    let old_cycles: i64 = database
        .query_row(
            "SELECT (SELECT COUNT(*) FROM trade_pnl_cycles)
            + (SELECT COUNT(*) FROM risk_trade_cycles)",
            [],
            |row| row.get(0),
        )
        .expect("legacy cycles");
    assert_eq!(old_cycles, 0, "V21 never writes superseded cycles");
}

#[test]
fn a_trade_opened_before_retained_history_keeps_its_exit_pnl_without_a_guessed_entry() {
    let directory = TestDirectory::new("round-trip-retention");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    // An open ticket keeps the quote subscribed while the position is flat between fills.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("ticket interest");
    execute_market_order(&service, &feed, 1, OrderSide::Buy, 1, (9_975, 10_000));
    for step in 0..4 {
        execute_market_order(
            &service,
            &feed,
            2 + step * 2,
            OrderSide::Buy,
            1,
            (9_975, 10_000),
        );
        execute_market_order(
            &service,
            &feed,
            3 + step * 2,
            OrderSide::Sell,
            1,
            (9_975, 10_000),
        );
    }
    execute_market_order(&service, &feed, 10, OrderSide::Sell, 1, (9_975, 10_000));
    let snapshot = service.snapshot().expect("snapshot");
    assert!(snapshot.fills.len() < 10, "the opening fill was retired");
    let retained_exits = snapshot
        .fills
        .iter()
        .filter(|fill| fill.side == OrderSide::Sell)
        .count();
    assert_eq!(snapshot.round_trips.len(), retained_exits);
    for trip in &snapshot.round_trips {
        assert_eq!(trip.side, OrderSide::Buy);
        assert!(trip.closed);
        assert_eq!(trip.entry, None, "a partial entry would misstate the trade");
        assert_eq!(
            trip.final_pnl,
            Some(price(-1_250)),
            "each exit keeps its own realized P&L after the opening fill retires"
        );
    }
}

#[test]
fn risk_profile_cancel_and_lock_state_are_authoritative_and_restart_safe() {
    let directory = TestDirectory::new("risk");
    let (service, _clock, _feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "practice-rules".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("session baseline"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("loss limit"),
            trailing_drawdown: Some(FixedPoint::try_new(200_000, 2).expect("drawdown")),
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(1, 0).expect("contracts"),
            consistency_max_single_trade_percent: Some(50),
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("risk profile stores");
    let meter = service.snapshot().expect("risk meter snapshot").risk_meters;
    assert_eq!(meter.len(), 1);
    assert_eq!(
        meter[0].daily_loss_remaining,
        FixedPoint::try_new(100_000, 2).expect("loss remaining")
    );
    assert_eq!(
        meter[0].contracts_remaining,
        FixedPoint::try_new(1, 0).expect("contracts remaining")
    );

    let working = service
        .place_order(market_order("risk-open", OrderSide::Buy, 1, 1_000))
        .expect("first order accepted");
    let modified = service
        .modify_order(ModifyOrder {
            client_order_id: working.client_order_id.clone(),
            time_in_force: TimeInForce::GoodTillCancelled,
            limit_price: None,
            stop_price: None,
            modified_unix_nanos: 1_500,
            provenance: provenance(2, 1_500),
        })
        .expect("modify accepted");
    assert_eq!(modified.time_in_force, TimeInForce::GoodTillCancelled);
    let cancelled = service
        .cancel_order(working.client_order_id.clone())
        .expect("cancel accepted");
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert!(
        service
            .snapshot()
            .expect("snapshot")
            .order_events
            .iter()
            .any(|event| event.kind == OrderEventKind::Cancelled)
    );

    service
        .lock_account(
            TradingAccountId::try_new("aeris-sim-1").expect("account"),
            "manual daily stop".to_string(),
            2_000,
        )
        .expect("lock stores");
    let locked = service
        .place_order(market_order("risk-locked", OrderSide::Buy, 2, 2_001))
        .expect_err("locked account rejects order");
    assert!(locked.contains("risk-locked"));
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = start_service(&directory);
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.risk_profiles.len(), 1);
    assert_eq!(restored.risk_locks.len(), 1);
    assert_eq!(
        restored.risk_meters[0].lock_reason.as_deref(),
        Some("manual daily stop")
    );
    restarted
        .unlock_account(TradingAccountId::try_new("aeris-sim-1").expect("account"))
        .expect("unlock stores");
    assert_eq!(
        restarted.snapshot().expect("snapshot").risk_locks,
        [] as [risk::RiskLock; 0]
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

#[test]
fn news_time_restriction_blocks_and_persists_a_hard_lock() {
    let directory = TestDirectory::new("news-restriction");
    let (service, _clock, _feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "news-rules".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("loss limit"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::EndOfDay,
            max_contracts: FixedPoint::try_new(2, 0).expect("contracts"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: Some(5_000),
            economic_event_rule: None,
            enabled: true,
        })
        .expect("restricted profile stores");
    assert!(
        service
            .place_order(market_order("news-window", OrderSide::Buy, 1, 4_000))
            .expect_err("news-time restriction blocks and locks")
            .contains("news/session restriction")
    );
    assert!(
        service.snapshot().expect("snapshot").risk_locks[0]
            .reason
            .contains("news/session restriction")
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let restarted = start_service(&directory);
    assert!(
        restarted.snapshot().expect("snapshot").risk_locks[0]
            .reason
            .contains("news/session restriction")
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn flatten_closes_positions_and_survives_a_restart() {
    let directory = TestDirectory::new("flatten");
    let (service, clock, feed) = live_feed_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("feed drains");
    service
        .place_order(market_order("flatten-open", OrderSide::Buy, 1, 1_000))
        .expect("buy accepted");
    push_live_quote(&feed, &instrument(), 2, 9_975, 10_000, 2);
    service.status().expect("buy fills");
    push_live_quote(&feed, &instrument(), 3, 10_100, 10_125, 2);
    service.status().expect("flatten quote");
    let fills = service
        .flatten(FlattenScope {
            account: Some(TradingAccountId::try_new("aeris-sim-1").expect("account")),
            instrument: None,
        })
        .expect("flatten succeeds");
    assert_eq!(fills.fills.len(), 1);
    assert_eq!(fills.pending_exits, []);
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.positions[0].net_quantity.units(), 0);
    assert_eq!(snapshot.fills.len(), 2);
    assert_eq!(snapshot.orders.len(), 2);
    let flattened = snapshot
        .orders
        .iter()
        .find(|order| order.id == fills.fills[0].order_id)
        .expect("flatten order");
    assert_eq!(flattened.revision, 2);
    assert_eq!(
        flattened.terminal_unix_nanos,
        Some(flattened.updated_unix_nanos)
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = TradingService::start(configuration).expect("service reopens");
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.positions[0].net_quantity.units(), 0);
    assert_eq!(restored.fills.len(), 2);
    assert_eq!(restored.orders, snapshot.orders);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

#[test]
fn economic_event_lock_is_profile_driven_durable_and_idempotent() {
    let directory = TestDirectory::new("economic-event-lock");
    let service = start_service(&directory);
    let account_id = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .register_risk_profile(RiskProfile {
            account_id: account_id.clone(),
            profile_id: "high-impact-lock".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("daily loss"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(5, 0).expect("contracts"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: Some(EconomicEventRiskRule {
                action: EconomicEventRiskAction::Lock,
                minimum_importance: EconomicEventRiskImportance::High,
                lead_seconds: 300,
            }),
            enabled: true,
        })
        .expect("profile registers");
    let scheduled = 1_000_000_000_000;
    let observed = scheduled - 60_000_000_000;
    let event = EconomicEventRiskTrigger {
        event_id: "bls-employment-2026-10".to_string(),
        title: "Employment Situation".to_string(),
        source: "BLS".to_string(),
        importance: EconomicEventRiskImportance::High,
        scheduled_unix_nanos: scheduled,
        source_release_unix_nanos: observed - 1,
        observed_unix_nanos: observed,
    };
    let first = service
        .apply_economic_event_risk(event.clone())
        .expect("event rule applies");
    assert_eq!(first.locked_accounts, 1);
    assert_eq!(
        service
            .apply_economic_event_risk(event)
            .expect("duplicate is fenced")
            .locked_accounts,
        0
    );
    for index in 0..5 {
        service
            .apply_economic_event_risk(EconomicEventRiskTrigger {
                event_id: format!("retention-event-{index}"),
                title: "Scheduled release".to_string(),
                source: "Official fixture".to_string(),
                importance: EconomicEventRiskImportance::High,
                scheduled_unix_nanos: scheduled,
                source_release_unix_nanos: observed - 1,
                observed_unix_nanos: observed + index,
            })
            .expect("distinct event applies");
    }
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let database = rusqlite::Connection::open(&config(&directory).database_path)
        .expect("database reopens for retention assertion");
    let retained_actions = database
        .query_row(
            "SELECT COUNT(*) FROM economic_event_risk_actions",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("retained event action count");
    assert_eq!(retained_actions, 4);
    drop(database);
    let restarted = start_service(&directory);
    assert_eq!(
        restarted.snapshot().expect("snapshot").risk_locks[0].account_id,
        account_id
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn global_flatten_closes_every_registered_account() {
    let directory = TestDirectory::new("flatten-all");
    let (service, _clock, feed) = live_feed_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("feed drains");
    let secondary = TradingAccountId::try_new("aeris-sim-2").expect("secondary account");
    service
        .register_account(TradingAccount {
            id: secondary.clone(),
            display_name: "Secondary SIM".to_string(),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".to_string(),
            broker_ref: None,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("secondary account registers");
    service
        .place_order(market_order("flatten-all-one", OrderSide::Buy, 1, 1_000))
        .expect("primary order accepted");
    let mut secondary_order = market_order("flatten-all-two", OrderSide::Buy, 2, 1_001);
    secondary_order.account_id = secondary;
    service
        .place_order(secondary_order)
        .expect("secondary order accepted");
    push_live_quote(&feed, &instrument(), 3, 9_975, 10_000, 2);
    service.status().expect("orders fill");
    assert_eq!(service.snapshot().expect("snapshot").fills.len(), 2);
    push_live_quote(&feed, &instrument(), 4, 10_100, 10_125, 2);
    service.status().expect("flatten quote");
    assert_eq!(
        service
            .flatten(FlattenScope::default())
            .expect("global flatten succeeds")
            .fills
            .len(),
        2
    );
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.positions.len(), 2);
    assert!(
        snapshot
            .positions
            .iter()
            .all(|position| position.net_quantity.units() == 0)
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn reverse_closes_and_reopens_the_exact_opposite_position() {
    let directory = TestDirectory::new("reverse-position");
    let (service, _clock, feed) = live_feed_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("feed drains");
    service
        .place_order(market_order("reverse-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry accepts");
    push_live_quote(&feed, &instrument(), 2, 9_975, 10_000, 2);
    service.status().expect("entry fills");
    push_live_quote(&feed, &instrument(), 3, 10_100, 10_125, 2);
    service.status().expect("reverse quote");
    let fills = service
        .reverse_position_at(
            TradingAccountId::try_new("aeris-sim-1").expect("account"),
            instrument().instrument_id,
        )
        .expect("position reverses");
    assert_eq!(fills.fills.len(), 2);
    assert_eq!(fills.pending_exits, []);
    let position = service.snapshot().expect("snapshot").positions[0].clone();
    assert_eq!(position.net_quantity.units(), -1);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn account_flatten_closes_two_positions_without_desktop_interest_in_second_instrument() {
    let directory = TestDirectory::new("account-flatten-chartless");
    let (service, clock, feed) = live_feed_service(&directory);
    let btc = instrument();
    let mut eth = instrument();
    eth.instrument_id =
        InstrumentId::try_new("instrument:fixture:CME:CLZ6").expect("second instrument");
    let eth_route = aeris_trading::market::MarketRoute {
        provider_id: "fixture".into(),
        instrument_id: eth.instrument_id.clone(),
        provider_symbol: "CLZ6".into(),
        display_symbol: "CLZ6".into(),
        venue_id: "CME".into(),
        entitlement_id: None,
        price_scale: 2,
        quantity_scale: 0,
    };
    service.register_instrument(btc.clone()).expect("btc");
    service.register_instrument(eth.clone()).expect("eth");
    service
        .set_ticket_interest(vec![market_feed::route(), eth_route.clone()])
        .expect("initial desktop interest");
    bootstrap_live_feed(&feed, &btc, market_feed::route());
    bootstrap_live_feed(&feed, &eth, eth_route);
    service.status().expect("feed drains");
    service
        .place_order(market_order("btc-entry", OrderSide::Buy, 1, 1_000))
        .expect("btc buy");
    let mut eth_order = market_order("eth-entry", OrderSide::Buy, 2, 1_001);
    eth_order.instrument_id = eth.instrument_id.clone();
    service.place_order(eth_order).expect("eth buy");
    push_live_quote(&feed, &btc, 2, 9_975, 10_000, 2);
    push_live_quote(&feed, &eth, 2, 7_975, 8_000, 2);
    service.status().expect("both entries fill");
    assert_eq!(service.snapshot().expect("snapshot").fills.len(), 2);

    // The desktop now shows BTC alone. Open positions retain owner-managed quote demand.
    service
        .set_ticket_interest(vec![market_feed::route()])
        .expect("desktop closes eth chart");
    push_live_quote(&feed, &btc, 3, 10_100, 10_125, 2);
    push_live_quote(&feed, &eth, 3, 8_100, 8_125, 2);
    service.status().expect("owner quotes drain");
    let outcome = service
        .flatten(FlattenScope {
            account: Some(TradingAccountId::try_new("aeris-sim-1").expect("account")),
            instrument: None,
        })
        .expect("account flatten");
    assert_eq!(outcome.unpriced, []);
    assert_eq!(outcome.fills.len(), 2);
    for (instrument_id, bid) in [(&btc.instrument_id, 10_100), (&eth.instrument_id, 8_100)] {
        assert_eq!(
            outcome
                .fills
                .iter()
                .filter(|fill| &fill.instrument_id == instrument_id
                    && fill.price == FixedPoint::try_new(bid, 2).expect("bid"))
                .count(),
            1,
            "exactly one closing fill at the owner's bid"
        );
    }
    let snapshot = service.snapshot().expect("flat snapshot");
    assert_eq!(snapshot.fills.len(), 4);
    assert!(
        snapshot
            .positions
            .iter()
            .all(|position| position.net_quantity.units() == 0)
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");

    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = TradingService::start(configuration).expect("service restarts");
    let restored = restarted.snapshot().expect("restored");
    assert_eq!(restored.fills.len(), 4);
    assert!(
        restored
            .positions
            .iter()
            .all(|position| position.net_quantity.units() == 0)
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn flatten_scope_isolates_one_account_instrument_pair() {
    let directory = TestDirectory::new("flatten-scope");
    let (service, _clock, feed) = live_feed_service(&directory);
    let first = instrument();
    let mut second = instrument();
    second.instrument_id =
        InstrumentId::try_new("instrument:fixture:CME:CLZ6").expect("second instrument id");
    service
        .register_instrument(first.clone())
        .expect("first instrument registers");
    service
        .register_instrument(second.clone())
        .expect("second instrument registers");
    let second_route = aeris_trading::market::MarketRoute {
        provider_id: "fixture".into(),
        instrument_id: second.instrument_id.clone(),
        provider_symbol: "CLZ6".into(),
        display_symbol: "CLZ6".into(),
        venue_id: "CME".into(),
        entitlement_id: None,
        price_scale: 2,
        quantity_scale: 0,
    };
    // Both instruments carry ticket demand, as they would with open charts; otherwise the
    // interest recompute evicts the idle instrument's quote-book entry on the next command.
    service
        .set_ticket_interest(vec![market_feed::route(), second_route.clone()])
        .expect("ticket interest");
    bootstrap_live_feed(&feed, &first, market_feed::route());
    bootstrap_live_feed(&feed, &second, second_route);
    service.status().expect("feed drains");
    let secondary = TradingAccountId::try_new("aeris-sim-2").expect("secondary account");
    service
        .register_account(TradingAccount {
            id: secondary.clone(),
            display_name: "Secondary SIM".to_string(),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".to_string(),
            broker_ref: None,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("secondary account registers");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .place_order(market_order("scope-a-one", OrderSide::Buy, 1, 1_000))
        .expect("first pair order");
    let mut other_instrument = market_order("scope-b-one", OrderSide::Buy, 2, 1_001);
    other_instrument.instrument_id = second.instrument_id.clone();
    service
        .place_order(other_instrument)
        .expect("second instrument order");
    let mut other_account = market_order("scope-a-two", OrderSide::Buy, 3, 1_002);
    other_account.account_id = secondary.clone();
    service
        .place_order(other_account)
        .expect("second account order");
    push_live_quote(&feed, &first, 4, 9_975, 10_000, 2);
    push_live_quote(&feed, &second, 1, 7_975, 8_000, 2);
    service.status().expect("entries fill");
    assert_eq!(service.snapshot().expect("snapshot").fills.len(), 3);
    push_live_quote(&feed, &first, 5, 10_100, 10_125, 2);
    service.status().expect("flatten quote");

    let outcome = service
        .flatten(FlattenScope {
            account: Some(account.clone()),
            instrument: Some(first.instrument_id.clone()),
        })
        .expect("scoped flatten succeeds");
    assert_eq!(outcome.fills.len(), 1);
    assert_eq!(
        outcome.fills[0].price,
        FixedPoint::try_new(10_100, 2).expect("bid")
    );
    assert_eq!(outcome.unpriced, []);
    let snapshot = service.snapshot().expect("snapshot");
    let net = |account_id: &TradingAccountId, instrument_id: &InstrumentId| {
        snapshot
            .positions
            .iter()
            .find(|position| {
                position.account_id == *account_id && position.instrument_id == *instrument_id
            })
            .map(|position| position.net_quantity.units())
    };
    assert_eq!(net(&account, &first.instrument_id), Some(0));
    assert_eq!(net(&account, &second.instrument_id), Some(1));
    assert_eq!(net(&secondary, &first.instrument_id), Some(1));
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn flatten_keeps_and_reports_positions_without_a_verified_quote() {
    let directory = TestDirectory::new("flatten-unpriced");
    let (service, clock, feed) = live_feed_service(&directory);
    let priced = instrument();
    let mut unquoted = instrument();
    unquoted.instrument_id =
        InstrumentId::try_new("instrument:fixture:CME:CLZ6").expect("unquoted instrument id");
    let mut unquoted_route = market_feed::route();
    unquoted_route.instrument_id = unquoted.instrument_id.clone();
    unquoted_route.provider_symbol = "CLZ6".into();
    unquoted_route.display_symbol = "CLZ6".into();
    service
        .register_instrument(priced.clone())
        .expect("priced instrument registers");
    service
        .register_instrument(unquoted.clone())
        .expect("unquoted instrument registers");
    bootstrap_live_feed(&feed, &priced, market_feed::route());
    service.status().expect("feed drains");
    service
        .place_order(market_order("priced-entry", OrderSide::Buy, 1, 1_000))
        .expect("priced order accepted");
    push_live_quote(&feed, &priced, 2, 9_975, 10_000, 2);
    service.status().expect("priced entry fills");
    // The unpriced instrument has verified Terms and a Live session, but its only quote
    // later goes stale, so the owner has no verified quote for it and flatten must report
    // it instead of guessing.
    bootstrap_live_feed(&feed, &unquoted, unquoted_route);
    service.status().expect("unquoted terms drain");
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: {
                let mut entry = market_order("unpriced-entry", OrderSide::Buy, 3, 1_002);
                entry.instrument_id = unquoted.instrument_id.clone();
                entry
            },
            template: BracketStrategyTemplate {
                template_id: "unpriced-bracket".to_string(),
                revision: 1,
                name: "Unpriced protection".to_string(),
                stop_offset_ticks: 8,
                targets: vec![BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 100,
                }],
                trailing_stop: None,
                break_even: None,
                enabled: true,
            },
        })
        .expect("bracket accepted");
    push_live_quote(&feed, &unquoted, 4, 7_975, 8_000, 2);
    service.status().expect("unquoted entry fills");
    assert_eq!(service.snapshot().expect("snapshot").fills.len(), 2);

    // Both quotes age past the freshness window; only the priced instrument receives a
    // fresh one before the flatten.
    clock.advance(Duration::from_secs(61));
    push_live_quote(&feed, &priced, 61_001, 9_975, 10_000, 2);
    service.status().expect("fresh priced quote");

    let outcome = service
        .flatten(FlattenScope::default())
        .expect("flatten succeeds with an unpriced report");
    assert_eq!(outcome.fills.len(), 1);
    assert_eq!(outcome.unpriced.len(), 1);
    assert_eq!(outcome.unpriced[0].instrument_id, unquoted.instrument_id);
    assert_eq!(outcome.unpriced[0].net_quantity.units(), 1);
    let snapshot = service.snapshot().expect("snapshot");
    let retained = snapshot
        .positions
        .iter()
        .find(|position| position.instrument_id == unquoted.instrument_id)
        .expect("unpriced position retained");
    assert_eq!(retained.net_quantity.units(), 1);
    assert!(
        snapshot.orders.iter().any(|order| {
            order.instrument_id == unquoted.instrument_id && order.status == OrderStatus::Working
        }),
        "unpriced position keeps its protection working"
    );
    assert!(
        snapshot
            .positions
            .iter()
            .find(|position| position.instrument_id == priced.instrument_id)
            .is_some_and(|position| position.net_quantity.units() == 0)
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn the_kill_switch_flattens_priced_instruments_and_keeps_stale_protection() {
    // VAL-TRD-061: one account with a bracketed long, a protected short, a
    // non-protective working limit and a protected position whose quote went stale.
    let directory = TestDirectory::new("kill-switch-flatten");
    let (service, clock, feed) = live_feed_service(&directory);
    let [bracketed_long, protected_short, working_only, stale] = kill_switch_world(&service, &feed);
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");

    // The fourth instrument's quote goes stale while the first two stay current.
    clock.advance(Duration::from_secs(61));
    push_live_quote(&feed, &bracketed_long, 61_001, 9_975, 10_000, 2);
    push_live_quote(&feed, &protected_short, 61_002, 7_975, 8_000, 2);
    service.status().expect("fresh quotes");

    let outcome = service
        .kill_switch(
            Some(account.clone()),
            "test kill".to_string(),
            1_061_000_000_000,
        )
        .expect("kill switch commits");
    assert_eq!(outcome.accounts_locked, 1);
    assert_eq!(outcome.fills.len(), 2);
    assert_eq!(outcome.unpriced.len(), 1);
    assert_eq!(outcome.unpriced[0].instrument_id, stale.instrument_id);
    assert_eq!(outcome.unpriced[0].net_quantity.units(), 1);
    assert_eq!(outcome.incomplete, None);
    assert_eq!(outcome.cancelled.len(), 1);
    assert_eq!(
        outcome.cancelled[0].client_order_id,
        ClientOrderId::try_new("kill-resting").expect("client id")
    );

    let snapshot = service.snapshot().expect("snapshot");
    assert_leg_event(
        &snapshot,
        &outcome.cancelled[0].id,
        OrderEventKind::Cancelled,
        aeris_trading::OrderEventReason::KillSwitch,
    );
    assert_eq!(
        net_position_on(&snapshot, &bracketed_long.instrument_id),
        Some(0)
    );
    assert_eq!(
        net_position_on(&snapshot, &protected_short.instrument_id),
        Some(0)
    );
    assert_eq!(net_position_on(&snapshot, &stale.instrument_id), Some(1));
    assert_eq!(
        working_orders_on(&snapshot, &bracketed_long.instrument_id),
        0,
        "the flattened long retires its protection"
    );
    assert_eq!(
        working_orders_on(&snapshot, &protected_short.instrument_id),
        0,
        "the flattened short retires its protection"
    );
    assert_eq!(working_orders_on(&snapshot, &working_only.instrument_id), 0);
    assert_eq!(
        working_orders_on(&snapshot, &stale.instrument_id),
        2,
        "the unpriced position keeps its stop and target working"
    );
    assert!(
        snapshot
            .risk_locks
            .iter()
            .any(|lock| lock.account_id == account),
        "the account is locked"
    );
    let refused = service
        .place_order(market_order("after-kill", OrderSide::Buy, 1, 1_004))
        .expect_err("a locked account accepts no new order");
    assert!(refused.contains("AccountLocked"), "{refused}");
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    assert_kill_switch_replay(&directory, clock, &snapshot, &stale.instrument_id);
}

/// One instrument's net position in a snapshot, if present.
fn net_position_on(snapshot: &TradingSnapshot, instrument_id: &InstrumentId) -> Option<i64> {
    snapshot
        .positions
        .iter()
        .find(|position| position.instrument_id == *instrument_id)
        .map(|position| position.net_quantity.units())
}

/// The number of working orders on one instrument in a snapshot.
fn working_orders_on(snapshot: &TradingSnapshot, instrument_id: &InstrumentId) -> usize {
    snapshot
        .orders
        .iter()
        .filter(|order| order.instrument_id == *instrument_id && order.status.is_open())
        .count()
}

/// The four-instrument kill-switch world with entries filled: a bracketed long on the
/// first instrument, a protected short on the second, a non-protective resting limit on
/// the third and a bracketed position on the fourth. Returns the instruments in that
/// order.
fn kill_switch_world(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
) -> [TradingInstrument; 4] {
    let bracketed_long = instrument();
    let mut protected_short = instrument();
    protected_short.instrument_id =
        InstrumentId::try_new("instrument:fixture:CME:CLZ6").expect("second instrument id");
    let mut working_only = instrument();
    working_only.instrument_id =
        InstrumentId::try_new("instrument:fixture:CME:NQZ6").expect("third instrument id");
    let mut stale = instrument();
    stale.instrument_id =
        InstrumentId::try_new("instrument:fixture:CME:RTYZ6").expect("fourth instrument id");
    let instruments = [bracketed_long, protected_short, working_only, stale];
    let symbols = ["ESZ6", "CLZ6", "NQZ6", "RTYZ6"];
    let routes = instruments
        .iter()
        .zip(symbols)
        .map(|(instrument, symbol)| aeris_trading::market::MarketRoute {
            provider_id: "fixture".into(),
            instrument_id: instrument.instrument_id.clone(),
            provider_symbol: symbol.into(),
            display_symbol: symbol.into(),
            venue_id: "CME".into(),
            entitlement_id: None,
            price_scale: 2,
            quantity_scale: 0,
        })
        .collect::<Vec<_>>();
    for (registered, route) in instruments.iter().zip(&routes) {
        service
            .register_instrument(registered.clone())
            .expect("instrument registers");
        bootstrap_live_feed(feed, registered, route.clone());
    }
    // Every instrument carries ticket demand, as with open charts; otherwise the interest
    // recompute evicts an idle instrument's quote-book entry on the next command.
    service
        .set_ticket_interest(routes)
        .expect("ticket interest");
    service.status().expect("feed drains");
    let bracket =
        |client: &str, side: OrderSide, quantity: u64, at: i64, on: &TradingInstrument| {
            let mut entry = market_order(client, side, quantity, at);
            entry.instrument_id = on.instrument_id.clone();
            PlaceInlineBracket {
                entry,
                template: BracketStrategyTemplate {
                    template_id: format!("{client}-bracket"),
                    revision: 1,
                    name: "Kill bracket".to_string(),
                    stop_offset_ticks: 8,
                    targets: vec![BracketTarget {
                        offset_ticks: 8,
                        quantity_percent: 100,
                    }],
                    trailing_stop: None,
                    break_even: None,
                    enabled: true,
                },
            }
        };
    service
        .place_inline_bracket(bracket(
            "kill-long",
            OrderSide::Buy,
            2,
            1_000,
            &instruments[0],
        ))
        .expect("long bracket accepted");
    service
        .place_inline_bracket(bracket(
            "kill-short",
            OrderSide::Sell,
            1,
            1_001,
            &instruments[1],
        ))
        .expect("short bracket accepted");
    service
        .place_inline_bracket(bracket(
            "kill-stale",
            OrderSide::Buy,
            1,
            1_002,
            &instruments[3],
        ))
        .expect("stale bracket accepted");
    let mut resting = market_order("kill-resting", OrderSide::Buy, 4, 1_003);
    resting.instrument_id = instruments[2].instrument_id.clone();
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(FixedPoint::try_new(5_000, 2).expect("limit"));
    push_live_quote(feed, &instruments[2], 1, 9_975, 10_000, 2);
    service.status().expect("resting quote");
    service
        .place_order(resting)
        .expect("resting limit accepted");
    push_live_quote(feed, &instruments[0], 1, 9_975, 10_000, 2);
    push_live_quote(feed, &instruments[1], 1, 7_975, 8_000, 2);
    push_live_quote(feed, &instruments[3], 1, 4_975, 5_000, 2);
    service.status().expect("bracket entries fill");
    assert_eq!(service.snapshot().expect("snapshot").fills.len(), 3);
    instruments
}

/// Reopens the store and proves the kill-switch outcome replayed exactly: identical
/// orders, locks and fills, and the stale instrument's protection still working.
fn assert_kill_switch_replay(
    directory: &TestDirectory,
    clock: std::sync::Arc<crate::clock::ManualClock>,
    snapshot: &TradingSnapshot,
    stale: &InstrumentId,
) {
    let mut configuration = config(directory);
    configuration.clock = clock;
    let restarted = TradingService::start(configuration).expect("service reopens");
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.orders, snapshot.orders);
    assert_eq!(restored.risk_locks, snapshot.risk_locks);
    // Fill list order is commit order in memory and id order after reload; compare as sets.
    let mut committed_fills = snapshot.fills.clone();
    committed_fills.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut restored_fills = restored.fills.clone();
    restored_fills.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    assert_eq!(restored_fills, committed_fills);
    assert_eq!(net_position_on(&restored, stale), Some(1));
    assert_eq!(
        working_orders_on(&restored, stale),
        2,
        "the unpriced position's protection survives the restart"
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

#[test]
fn reverse_rejected_by_risk_leaves_the_position_untouched() {
    let directory = TestDirectory::new("reverse-risk");
    let (service, _clock, feed) = live_feed_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("feed drains");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut entry = market_order("reverse-risk-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    service.place_order(entry).expect("entry accepted");
    push_live_quote(&feed, &instrument(), 2, 9_975, 10_000, 2);
    service.status().expect("entry fills");
    service
        .register_risk_profile(RiskProfile {
            account_id: account.clone(),
            profile_id: "tight-contracts".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("daily"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(1, 0).expect("contracts"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("risk profile registers");
    push_live_quote(&feed, &instrument(), 3, 10_100, 10_125, 2);
    service.status().expect("reverse quote");
    let before = service.snapshot().expect("before reverse");
    let error = service
        .reverse_position_at(account.clone(), instrument().instrument_id)
        .unwrap_err();
    assert!(
        error.contains("maximum-contract"),
        "risk rejection names the rule: {error}"
    );
    let after = service.snapshot().expect("after rejection");
    assert_eq!(after.orders, before.orders);
    assert_eq!(after.fills, before.fills);
    assert_eq!(after.positions, before.positions);
    assert_eq!(after.revision, before.revision);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn reverse_on_a_locked_account_returns_account_locked_without_a_fill() {
    // VAL-TRD-063: the reverse admission runs inside the same step as the close, so a
    // locked account refuses the whole operation and keeps the position.
    let directory = TestDirectory::new("reverse-locked");
    let (service, _clock, feed) = live_feed_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("feed drains");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .place_order(market_order(
            "reverse-locked-entry",
            OrderSide::Buy,
            2,
            1_000,
        ))
        .expect("entry accepted");
    push_live_quote(&feed, &instrument(), 2, 9_975, 10_000, 2);
    service.status().expect("entry fills");
    service
        .lock_account(account.clone(), "manual lock".to_string(), 1_500)
        .expect("account locks");
    push_live_quote(&feed, &instrument(), 3, 10_100, 10_125, 2);
    service.status().expect("reverse quote");
    let before = service.snapshot().expect("before reverse");
    let error = service
        .reverse_position_at(account, instrument().instrument_id)
        .expect_err("a locked account cannot reverse");
    assert!(error.contains("AccountLocked"), "{error}");
    assert_eq!(
        service.snapshot().expect("after refusal"),
        before,
        "the refused reverse leaves the position and every fact untouched"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

use crate::clock::Clock as _;

fn rollover_wall_nanos(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("timestamp")
        .timestamp_nanos_opt()
        .expect("timestamp in range")
}

/// Moves the manual clock so the owner timer fires at `tick_wall` and waits out the pass.
fn tick_timer_at(
    service: &TradingService,
    clock: &std::sync::Arc<crate::clock::ManualClock>,
    tick_wall: i64,
) {
    clock.set_wall_unix_nanos(tick_wall - 61_000_000_000);
    clock.advance(Duration::from_secs(61));
    service.status().expect("timer tick");
    service.status().expect("timer barrier");
}

fn risk_profile_for(account: &TradingAccountId, session_start_unix_nanos: i64) -> RiskProfile {
    RiskProfile {
        account_id: account.clone(),
        profile_id: "daily".to_string(),
        version: 1,
        session_start_unix_nanos,
        session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
        daily_loss_limit: FixedPoint::try_new(1_000, 2).expect("daily limit"),
        trailing_drawdown: None,
        trailing_mode: TrailingDrawdownMode::Intraday,
        max_contracts: FixedPoint::try_new(10, 0).expect("contract limit"),
        consistency_max_single_trade_percent: None,
        restricted_until_unix_nanos: None,
        economic_event_rule: None,
        enabled: true,
    }
}

/// Pushes a Live quote whose receive timestamp matches the manual clock's current wall, so
/// freshness checks hold at real calendar instants.
fn push_quote_at_clock(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    clock: &std::sync::Arc<crate::clock::ManualClock>,
    sequence: u64,
    bid: i64,
    ask: i64,
) {
    feed.push(MarketFeedEvent::Quote(TradingQuote {
        instrument_id: instrument.instrument_id.clone(),
        bid: FixedPoint::try_new(bid, 2).expect("bid"),
        ask: FixedPoint::try_new(ask, 2).expect("ask"),
        bid_size: None,
        ask_size: None,
        last: None,
        provider_id: "fixture".to_string(),
        provider_generation: 1,
        source_sequence: Some(sequence),
        exchange_unix_nanos: None,
        received_unix_nanos: clock.wall_unix_nanos(),
    }))
    .expect("quote");
}

/// Publishes an open week-long session and the Live feed state for the instrument.
fn open_session_feed(
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
) {
    feed.push(MarketFeedEvent::Session(
        aeris_trading::market::InstrumentSession {
            instrument_id: instrument.instrument_id.clone(),
            provider_generation: 1,
            phase: aeris_instruments::SessionPhase::Open,
            source: aeris_instruments::CalendarSourceKind::ProviderCalendar,
            session_end_unix_nanos: Some(rollover_wall_nanos("2026-10-23T00:00:00Z")),
            next_open_unix_nanos: None,
        },
    ))
    .expect("session");
    feed.push(market_feed::feed_state(
        instrument.instrument_id.clone(),
        1,
        FeedState::Live,
    ))
    .expect("live");
}

/// Drives a $50 losing round trip, registers the UTC-midnight risk profile and locks the
/// account at its daily loss limit on the next admission.
fn daily_loss_locked_world(
    directory: &TestDirectory,
) -> (
    TradingService,
    std::sync::Arc<crate::clock::ManualClock>,
    std::sync::Arc<crate::core::market::feed::MarketFeedInbox>,
    TradingAccountId,
) {
    let (service, clock, feed) = live_feed_service(directory);
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-16T12:00:00Z"));
    let instrument = instrument();
    service
        .register_instrument(instrument.clone())
        .expect("instrument registers");
    feed.push(MarketFeedEvent::Terms(InstrumentTerms {
        route: market_feed::route(),
        instrument_class: instrument.contract.instrument_class,
        price_grid: instrument.contract.price_grid,
        point_value: instrument.contract.point_value,
        quantity_increment: instrument.contract.order_quantity_increment,
        currency: instrument.contract.currency.clone(),
        expiry: instrument.contract.expiry,
        first_notice: instrument.contract.first_notice,
        last_trade: instrument.contract.last_trade,
        session_hours: instrument.contract.session_hours.clone(),
        session_generation: 1,
    }))
    .expect("terms");
    open_session_feed(&feed, &instrument);
    service.status().expect("feed drains");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .place_order(market_order(
            "roll-entry",
            OrderSide::Buy,
            1,
            rollover_wall_nanos("2026-10-16T12:00:01Z"),
        ))
        .expect("entry accepted");
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-16T12:00:02Z"));
    push_quote_at_clock(&feed, &instrument, &clock, 2, 9_975, 10_000);
    service.status().expect("entry fills");
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-16T12:00:03Z"));
    push_quote_at_clock(&feed, &instrument, &clock, 3, 9_900, 9_925);
    service.status().expect("exit quote");
    service
        .place_order(market_order(
            "roll-exit",
            OrderSide::Sell,
            1,
            rollover_wall_nanos("2026-10-16T12:00:04Z"),
        ))
        .expect("exit accepted");
    service.status().expect("exit fills");
    service
        .register_risk_profile(risk_profile_for(
            &account,
            rollover_wall_nanos("2026-10-16T12:00:00Z"),
        ))
        .expect("profile registers");
    // The next admission observes the realized loss beyond the limit and locks the account.
    // A flat account with no working orders drops its quote interest, so the session and a
    // fresh quote are republished first.
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-16T12:00:05Z"));
    open_session_feed(&feed, &instrument);
    push_quote_at_clock(&feed, &instrument, &clock, 4, 9_900, 9_925);
    service.status().expect("live check");
    let refused = service
        .place_order(market_order(
            "roll-blocked",
            OrderSide::Buy,
            1,
            rollover_wall_nanos("2026-10-16T12:00:05Z"),
        ))
        .expect_err("the daily loss limit refuses new exposure");
    assert!(refused.contains("daily loss limit"), "{refused}");
    (service, clock, feed, account)
}

#[test]
fn risk_sessions_roll_over_at_the_utc_midnight_boundary() {
    // VAL-TRD-065: the default UTC-midnight boundary resets the daily meter, releases the
    // daily-loss lock and persists the rolled session row with the realized baseline.
    let directory = TestDirectory::new("risk-session-rollover");
    let (service, clock, feed, account) = daily_loss_locked_world(&directory);
    let seeded = service.snapshot().expect("seeded snapshot");
    assert_eq!(
        seeded.risk_sessions,
        [RiskSession {
            account_id: account.clone(),
            boundary: RiskSessionBoundary::UtcMidnight,
            session_start_unix_nanos: rollover_wall_nanos("2026-10-16T12:00:00Z"),
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
        }]
    );
    let locked = service.snapshot().expect("locked snapshot");
    assert_eq!(locked.risk_locks.len(), 1);
    assert_eq!(locked.risk_locks[0].kind, RiskLockKind::DailyLossLimit);

    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-10-16T23:59:59Z"),
    );
    let unchanged = service.snapshot().expect("same-day snapshot");
    assert_eq!(
        unchanged.risk_sessions, seeded.risk_sessions,
        "no roll before the boundary"
    );
    assert_eq!(
        unchanged.risk_locks, locked.risk_locks,
        "the daily lock stands until the boundary"
    );

    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-10-17T00:00:01Z"),
    );
    let rolled = service.snapshot().expect("rolled snapshot");
    assert_eq!(
        rolled.risk_sessions,
        [RiskSession {
            account_id: account.clone(),
            boundary: RiskSessionBoundary::UtcMidnight,
            session_start_unix_nanos: rollover_wall_nanos("2026-10-17T00:00:00Z"),
            session_start_realized_pnl: FixedPoint::try_new(-5_000, 2).expect("rolled baseline"),
        }],
        "the rolled session is durable with the realized baseline"
    );
    assert!(
        rolled.risk_locks.is_empty(),
        "the daily-loss lock released at the boundary"
    );
    let meter = &rolled.risk_meters[0];
    assert_eq!(
        meter.current_realized_pnl,
        FixedPoint::try_new(0, 2).expect("zero")
    );
    assert_eq!(
        meter.daily_loss_remaining,
        FixedPoint::try_new(1_000, 2).expect("limit")
    );
    // The account trades again immediately after the roll.
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-17T00:00:02Z"));
    let instrument = instrument();
    push_quote_at_clock(&feed, &instrument, &clock, 5, 9_900, 9_925);
    service.status().expect("post-roll quote");
    let mut resting = market_order(
        "post-roll",
        OrderSide::Buy,
        1,
        rollover_wall_nanos("2026-10-17T00:00:02Z"),
    );
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(FixedPoint::try_new(9_800, 2).expect("limit"));
    let accepted = service.place_order(resting).expect("post-roll admission");
    service
        .cancel_order(accepted.client_order_id)
        .expect("resting order cancels");
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn a_manual_lock_survives_the_session_rollover() {
    // VAL-TRD-065: only daily-meter locks release at the boundary; a manual lock stands.
    let directory = TestDirectory::new("risk-session-manual-lock");
    let (service, clock, _feed, account) = daily_loss_locked_world(&directory);
    service
        .unlock_account(account.clone())
        .expect("daily lock clears");
    service
        .lock_account(
            account.clone(),
            "manual hold".to_string(),
            rollover_wall_nanos("2026-10-16T12:00:07Z"),
        )
        .expect("manual lock");
    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-10-18T00:00:01Z"),
    );
    let next = service.snapshot().expect("next-day snapshot");
    assert_eq!(next.risk_locks.len(), 1);
    assert_eq!(next.risk_locks[0].kind, RiskLockKind::Manual);
    assert_eq!(
        next.risk_sessions[0].session_start_unix_nanos,
        rollover_wall_nanos("2026-10-18T00:00:00Z")
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn risk_sessions_survive_restart_within_a_session_and_roll_across_one() {
    // VAL-TRD-065: reopening inside the same session changes nothing; reopening after the
    // boundary passed rolls the session on the first owner tick.
    let directory = TestDirectory::new("risk-session-restart");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(rollover_wall_nanos(
        "2026-10-16T12:00:00Z",
    )));
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let service = start_service_with_config(configuration);
    service
        .register_risk_profile(risk_profile_for(
            &account,
            rollover_wall_nanos("2026-10-16T12:00:00Z"),
        ))
        .expect("profile registers");
    service
        .shutdown(Duration::from_secs(2))
        .expect("first stop");

    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-16T18:00:00Z"));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let reopened = start_service_with_config(configuration);
    tick_timer_at(
        &reopened,
        &clock,
        rollover_wall_nanos("2026-10-16T18:01:00Z"),
    );
    let within = reopened.snapshot().expect("within-session snapshot");
    assert_eq!(
        within.risk_sessions[0].session_start_unix_nanos,
        rollover_wall_nanos("2026-10-16T12:00:00Z"),
        "a restart inside the session keeps the running session"
    );
    reopened
        .shutdown(Duration::from_secs(2))
        .expect("second stop");

    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-17T01:00:00Z"));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let reopened = start_service_with_config(configuration);
    tick_timer_at(
        &reopened,
        &clock,
        rollover_wall_nanos("2026-10-17T01:01:00Z"),
    );
    let across = reopened.snapshot().expect("crossed snapshot");
    assert_eq!(
        across.risk_sessions[0].session_start_unix_nanos,
        rollover_wall_nanos("2026-10-17T00:00:00Z"),
        "the first tick after a restart across the boundary rolls the session"
    );
    reopened
        .shutdown(Duration::from_secs(2))
        .expect("final stop");
}

#[test]
fn risk_sessions_roll_over_at_the_configured_chicago_close() {
    // VAL-TRD-065: a futures-style 17:00 America/Chicago boundary rolls at 22:00Z in
    // daylight time and at 23:00Z in standard time.
    let directory = TestDirectory::new("risk-session-chicago");
    let (service, clock, _feed) = live_feed_service(&directory);
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-07-01T12:00:00Z"));
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .register_risk_profile(risk_profile_for(
            &account,
            rollover_wall_nanos("2026-07-01T12:00:00Z"),
        ))
        .expect("profile registers");
    service
        .set_risk_session_boundary(account.clone(), RiskSessionBoundary::ChicagoClose)
        .expect("boundary updates");
    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-07-01T21:59:59Z"),
    );
    assert_eq!(
        service
            .snapshot()
            .expect("pre-close snapshot")
            .risk_sessions[0]
            .session_start_unix_nanos,
        rollover_wall_nanos("2026-07-01T12:00:00Z"),
        "no roll before the 17:00 CT close"
    );
    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-07-01T22:00:01Z"),
    );
    assert_eq!(
        service.snapshot().expect("cdt snapshot").risk_sessions[0].session_start_unix_nanos,
        rollover_wall_nanos("2026-07-01T22:00:00Z"),
        "the CDT boundary rolls at 22:00Z"
    );
    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2027-01-15T22:59:59Z"),
    );
    assert_eq!(
        service.snapshot().expect("cst snapshot").risk_sessions[0].session_start_unix_nanos,
        rollover_wall_nanos("2027-01-14T23:00:00Z"),
        "the CST boundary rolls at 23:00Z"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn a_bracket_entry_counts_its_template_stop_as_attached() {
    // VAL-TRD-065: the no-attached-stop warning fires for a plain entry but never for a
    // bracket entry, whose template stop is attached by construction.
    let directory = TestDirectory::new("bracket-stop-warning");
    let (service, clock, feed) = live_feed_service(&directory);
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-16T12:00:00Z"));
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    bootstrap_always_open_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("verified terms drain");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut profile = risk_profile_for(&account, rollover_wall_nanos("2026-10-16T00:00:00Z"));
    profile.daily_loss_limit = FixedPoint::try_new(500_000, 2).expect("daily limit");
    service
        .register_risk_profile(profile)
        .expect("profile registers");
    service
        .place_order(market_order(
            "plain-entry",
            OrderSide::Buy,
            1,
            rollover_wall_nanos("2026-10-16T12:00:01Z"),
        ))
        .expect("plain entry accepted");
    let accepted_detail = |client_order_id: &str| {
        let snapshot = service.snapshot().expect("snapshot");
        let order = snapshot
            .orders
            .iter()
            .find(|order| order.client_order_id.as_str() == client_order_id)
            .expect("order");
        snapshot
            .order_events
            .iter()
            .find(|event| event.order_id == order.id && event.kind == OrderEventKind::Accepted)
            .expect("accepted event")
            .detail
            .clone()
    };
    assert!(
        accepted_detail("plain-entry").is_some_and(|detail| detail.contains("no attached stop")),
        "the plain entry carries the warning"
    );
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order(
                "bracket-entry",
                OrderSide::Buy,
                1,
                rollover_wall_nanos("2026-10-16T12:00:02Z"),
            ),
            template: bracket_template(),
        })
        .expect("bracket accepted");
    assert_eq!(
        accepted_detail("bracket-entry").as_deref(),
        Some("accepted by local simulated venue"),
        "the bracket entry attaches its template stop"
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}
#[test]
fn invalid_json_never_enters_the_store() {
    let directory = TestDirectory::new("invalid-json");
    let service = start_service(&directory);
    let error = service
        .put_user_record(UserRecord {
            id: "note-1".to_string(),
            kind: UserRecordKind::Note,
            revision: 1,
            updated_unix_nanos: 1,
            json: "not-json".to_string(),
        })
        .expect_err("invalid JSON rejected");
    assert!(error.contains("valid JSON"));
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn trade_copier_is_durable_bounded_and_checks_each_target_independently() {
    let directory = TestDirectory::new("copier");
    let (service, _clock, _feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    for id in ["aeris-sim-2", "aeris-sim-3"] {
        service
            .register_account(TradingAccount {
                id: TradingAccountId::try_new(id).expect("account id"),
                display_name: format!("SIM {id}"),
                environment: AccountEnvironment::Simulated,
                venue_id: "aeris-sim".to_string(),
                broker_ref: None,
                currency: "USD".to_string(),
                currency_scale: 2,
                starting_equity: None,
            })
            .expect("account registers");
    }
    let blocked = TradingAccountId::try_new("aeris-sim-2").expect("blocked account");
    service
        .lock_account(blocked.clone(), "target kill switch".to_string(), 1)
        .expect("target locks");
    service
        .register_trade_copier(TradeCopierConfig {
            source_account_id: TradingAccountId::try_new("aeris-sim-1").expect("source"),
            revision: 1,
            enabled: true,
            targets: vec![
                TradeCopierTarget {
                    account_id: blocked,
                    quantity_multiplier: FixedPoint::try_new(1, 0).expect("multiplier"),
                    enabled: true,
                },
                TradeCopierTarget {
                    account_id: TradingAccountId::try_new("aeris-sim-3").expect("target"),
                    quantity_multiplier: FixedPoint::try_new(2, 0).expect("multiplier"),
                    enabled: true,
                },
            ],
        })
        .expect("copier registers");

    let source = market_order("copier-source", OrderSide::Buy, 1, 1_000);
    service.place_order(source.clone()).expect("source accepts");
    service
        .place_order(source)
        .expect("source retry is idempotent");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.orders.len(), 2);
    assert_eq!(snapshot.copy_dispatches.len(), 2);
    assert_eq!(
        snapshot
            .orders
            .iter()
            .find(|order| order.account_id.as_str() == "aeris-sim-3")
            .expect("accepted copy")
            .quantity,
        FixedPoint::try_new(2, 0).expect("copied quantity")
    );
    assert!(snapshot.copy_dispatches.iter().any(|dispatch| {
        dispatch.target_account_id.as_str() == "aeris-sim-2" && !dispatch.accepted
    }));
    assert!(snapshot.copy_dispatches.iter().any(|dispatch| {
        dispatch.target_account_id.as_str() == "aeris-sim-3" && dispatch.accepted
    }));
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = start_service(&directory);
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.trade_copiers.len(), 1);
    assert_eq!(restored.trade_copiers[0].targets.len(), 2);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

#[test]
fn bracket_strategy_templates_are_revisioned_and_restart_safe() {
    let directory = TestDirectory::new("strategy-template");
    let service = start_service(&directory);
    let template = BracketStrategyTemplate {
        template_id: "scalp-two-target".to_string(),
        revision: 1,
        name: "Scalp two target".to_string(),
        stop_offset_ticks: 8,
        targets: vec![
            BracketTarget {
                offset_ticks: 8,
                quantity_percent: 50,
            },
            BracketTarget {
                offset_ticks: 16,
                quantity_percent: 50,
            },
        ],
        trailing_stop: Some(TrailingStopRule {
            activation_ticks: 12,
            distance_ticks: 6,
        }),
        break_even: Some(BreakEvenRule {
            activation_ticks: 8,
            offset_ticks: 1,
        }),
        enabled: true,
    };
    service
        .register_strategy_template(template.clone())
        .expect("template registers");
    assert!(
        service
            .register_strategy_template(template.clone())
            .expect_err("stale revision rejected")
            .contains("revision must advance")
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = start_service(&directory);
    assert!(
        restarted
            .snapshot()
            .expect("snapshot")
            .strategy_templates
            .contains(&template)
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

#[test]
#[ignore = "manual release-mode D3 storage measurement"]
fn measured_store_workload_is_bounded_and_restartable() {
    const EXECUTIONS: u64 = 500;
    const USER_RECORDS: u64 = 2_000;
    let directory = TestDirectory::new("measurement");
    let measured_config = TradingServiceConfig {
        database_path: directory.0.join("trading.sqlite3"),
        retention: TradingRetention {
            maximum_orders: 4_096,
            maximum_fills: usize::try_from(EXECUTIONS).expect("execution count"),
            maximum_order_events: usize::try_from(EXECUTIONS * 2).expect("event count"),
            maximum_user_records_per_kind: usize::try_from(USER_RECORDS).expect("record count"),
        },
        clock: Arc::new(crate::clock::SystemClock::default()),
        execution: ExecutionPolicy::default(),
    };
    let service = TradingService::start(measured_config.clone()).expect("service starts");
    service
        .register_account(TradingAccount {
            id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            display_name: "SIM • Measurement".to_string(),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".to_string(),
            broker_ref: None,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: Some(FixedPoint::try_new(5_000_000, 2).expect("equity")),
        })
        .expect("measurement account registers");
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let write_started = std::time::Instant::now();
    for index in 0..EXECUTIONS {
        let sequence = index * 3 + 1;
        let time = i64::try_from(sequence + 1).expect("time");
        service
            .place_order(market_order(
                &format!("measurement-{index}"),
                OrderSide::Buy,
                sequence,
                time,
            ))
            .expect("measurement order");
        service
            .observe_market(observation(9_975, 10_000, sequence + 1, time + 1))
            .expect("measurement fill");
    }
    for index in 0..USER_RECORDS {
        service
            .put_user_record(UserRecord {
                id: format!("measurement-note-{index}"),
                kind: UserRecordKind::Note,
                revision: 1,
                updated_unix_nanos: i64::try_from(10_000 + index).expect("time"),
                json: format!(r#"{{"index":{index}}}"#),
            })
            .expect("measurement record");
    }
    let write_elapsed = write_started.elapsed();
    service
        .shutdown(Duration::from_secs(10))
        .expect("service stops");
    let database_bytes = fs::metadata(&measured_config.database_path)
        .expect("database metadata")
        .len();
    let quick_check_started = std::time::Instant::now();
    super::store::preflight_existing_database(&measured_config.database_path)
        .expect("quick_check passes");
    let quick_check_elapsed = quick_check_started.elapsed();
    let reopen_started = std::time::Instant::now();
    let reopened = TradingService::start(measured_config).expect("service reopens");
    let snapshot = reopened.snapshot().expect("snapshot reloads");
    let reopen_elapsed = reopen_started.elapsed();
    assert_eq!(
        snapshot.fills.len(),
        usize::try_from(EXECUTIONS).expect("count")
    );
    println!(
        "D3 sqlite measurement: {EXECUTIONS} executions + {USER_RECORDS} user records in {write_elapsed:?}; quick_check {quick_check_elapsed:?}; reopen {reopen_elapsed:?}; database {database_bytes} bytes"
    );
    reopened
        .shutdown(Duration::from_secs(10))
        .expect("reopened service stops");
}

#[test]
fn cancelling_a_protective_stop_leaves_it_cancelled() {
    let directory = TestDirectory::new("protective-cancel");
    let (service, _clock, feed) =
        awaiting_quote_fixture(&directory, &instrument(), market_feed::route());
    service
        .register_instrument(instrument())
        .expect("instrument");
    service
        .place_order(market_order("entry", OrderSide::Buy, 1, 1_000))
        .expect("entry");
    assert_eq!(
        deliver_quote(&service, &feed, &instrument(), 2, 500_000, 500_025).len(),
        1,
        "fill"
    );
    let mut stop = market_order("stop", OrderSide::Sell, 3, 3_000);
    stop.order_type = OrderType::Stop;
    stop.stop_price = Some(FixedPoint::try_new(499_000, 2).expect("stop"));
    service
        .place_protective_order(PlaceProtection {
            order: stop,
            role: ProtectionRole::StopLoss,
        })
        .expect("protective stop");
    service
        .cancel_order(ClientOrderId::try_new("stop").expect("id"))
        .expect("cancel");
    deliver_quote(&service, &feed, &instrument(), 4, 500_050, 500_075);
    let snapshot = service.snapshot().expect("snapshot");
    let open = snapshot
        .orders
        .iter()
        .filter(|order| order.status.is_open())
        .map(|order| order.client_order_id.as_str().to_string())
        .collect::<Vec<_>>();
    assert!(open.is_empty(), "open orders after cancel: {open:?}");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}
