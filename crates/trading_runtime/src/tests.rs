use super::*;
use aeris_instruments::{
    ContractDate, InstrumentDecimal, InstrumentMetadataProvenance, SessionHours,
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
mod market_feed;
mod oco;

#[test]
fn simulated_currency_mismatch_is_typed_before_an_order_fact() {
    let directory = TestDirectory::new("currency-admission");
    let service = start_service(&directory);
    let mut instrument = instrument();
    instrument.contract.currency = "EUR".into();
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
    let service = start_service(&directory);
    let mut instrument = instrument();
    instrument.contract.price_grid = aeris_instruments::PriceGrid::Unknown;
    service.register_instrument(instrument).expect("instrument");
    let broker_id = TradingAccountId::try_new("warning-demo").expect("id");
    service
        .register_account(TradingAccount {
            id: broker_id.clone(),
            display_name: "test demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: "ctrader".into(),
            broker_ref: Some("fixture-demo".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("broker account");
    let (_generation, _receiver) = service.attach_demo_venue().expect("venue");
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
    let service = start_service(&directory);
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
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let service = TradingService::start(configuration).expect("service starts");
    service
        .register_account(TradingAccount {
            id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            display_name: "SIM test".into(),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".into(),
            broker_ref: None,
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: Some(FixedPoint::try_new(5_000_000, 2).expect("equity")),
        })
        .expect("account");
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
    let service = start_service(&directory);
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let accepted = service
        .place_order(market_order("filled-stale", OrderSide::Buy, 1, 1_000))
        .expect("accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("filled");
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
    let service = start_service(&directory);
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
    let service = start_service(&directory);
    let mut crypto = high_precision_crypto_instrument();
    crypto.contract.tick_size = None;
    crypto.contract.price_grid = aeris_instruments::PriceGrid::SignificantFigures {
        figures: 5,
        max_decimals: 6,
    };
    service
        .register_instrument(crypto.clone())
        .expect("terms register");
    assert_eq!(
        instrument_tick_units(
            &crypto,
            FixedPoint::try_new(8_210_000_000_000, 8).expect("mark"),
        ),
        Ok(100_000_000)
    );
    let quote = SimulatedMarketObservation {
        instrument_id: crypto.instrument_id.clone(),
        bid: FixedPoint::try_new(8_209_000_000_000, 8).expect("bid"),
        ask: FixedPoint::try_new(8_210_000_000_000, 8).expect("ask"),
        provenance: provenance(1, 1_000),
    };
    assert_eq!(service.observe_market(quote).expect("quote accepted"), []);
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
    let next = SimulatedMarketObservation {
        instrument_id: crypto.instrument_id,
        bid: FixedPoint::try_new(8_209_000_000_000, 8).expect("bid"),
        ask: FixedPoint::try_new(8_210_000_000_000, 8).expect("ask"),
        provenance: provenance(3, 3_000),
    };
    assert_eq!(service.observe_market(next).expect("market fills").len(), 1);
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
    let service = start_service(&directory);
    let mut contract = instrument();
    contract.quantity_scale = 8;
    contract.contract.order_quantity_increment =
        Some(InstrumentDecimal::try_new(100_000_000, 8).expect("one contract"));
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
    let service = start_service(&directory);
    let first = instrument();
    service
        .register_instrument(first.clone())
        .expect("initial registration");
    service
        .place_order(market_order("before-refresh", OrderSide::Buy, 1, 1_000))
        .expect("entry accepted");
    service
        .observe_market(observation(9_999, 10_000, 2, 2_000))
        .expect("entry fills");

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
    let service = start_service(&directory);
    let mut non_usd = instrument();
    non_usd.contract.currency = "EUR".into();
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
    let service = start_service(&directory);
    let future = instrument();
    assert_eq!(
        tick_value(&future, 2).expect("ES tick value"),
        FixedPoint::try_new(1_250, 2).expect("$12.50")
    );
    service
        .register_instrument(future.clone())
        .expect("ES registers");
    let mut equity = future.clone();
    equity.instrument_id = InstrumentId::try_new("tastytrade:Equity:AAPL").expect("equity id");
    equity.contract.point_value = Some(InstrumentDecimal::try_new(1, 0).expect("one per share"));
    equity.contract.tick_size = Some(InstrumentDecimal::try_new(1, 2).expect("cent"));
    equity.contract.provenance.display_symbol = "AAPL".into();
    service
        .register_instrument(equity.clone())
        .expect("AAPL registers");

    let es_buy = market_order("es-buy", OrderSide::Buy, 1, 1_000);
    service.place_order(es_buy).expect("ES buy");
    service
        .observe_market(observation(9_999, 10_000, 2, 2_000))
        .expect("ES entry fill");
    service
        .place_order(market_order("es-sell", OrderSide::Sell, 3, 3_000))
        .expect("ES sell");
    service
        .observe_market(observation(10_400, 10_401, 4, 4_000))
        .expect("ES exit fill");

    let mut stock_buy = market_order("aapl-buy", OrderSide::Buy, 1, 1_000);
    stock_buy.instrument_id = equity.instrument_id.clone();
    stock_buy.quantity = FixedPoint::try_new(10, 0).expect("ten shares");
    service.place_order(stock_buy).expect("AAPL buy");
    let mut stock_entry = observation(33_099, 33_100, 2, 2_000);
    stock_entry.instrument_id = equity.instrument_id.clone();
    service
        .observe_market(stock_entry)
        .expect("AAPL entry fill");
    let mut stock_sell = market_order("aapl-sell", OrderSide::Sell, 3, 3_000);
    stock_sell.instrument_id = equity.instrument_id.clone();
    stock_sell.quantity = FixedPoint::try_new(10, 0).expect("ten shares");
    service.place_order(stock_sell).expect("AAPL sell");
    let mut stock_exit = observation(33_200, 33_201, 4, 4_000);
    stock_exit.instrument_id = equity.instrument_id.clone();
    service.observe_market(stock_exit).expect("AAPL exit fill");

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
    let service = start_service(&directory);
    let instrument = high_precision_crypto_instrument();
    service
        .register_instrument(instrument.clone())
        .expect("crypto instrument registers");
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
    let observation = |bid: i64, ask: i64, sequence: u64, time: i64| SimulatedMarketObservation {
        instrument_id: instrument.instrument_id.clone(),
        bid: FixedPoint::try_new(bid, 8).expect("bid"),
        ask: FixedPoint::try_new(ask, 8).expect("ask"),
        provenance: TradingProvenance {
            venue_id: "aeris-sim".to_string(),
            provider_id: "hyperliquid".to_string(),
            session_generation: 1,
            source_sequence: sequence,
            observed_unix_nanos: time,
        },
    };
    service
        .observe_market(observation(8_399_900_000_000, 8_400_000_000_000, 2, 2_000))
        .expect("entry fills");
    service
        .observe_market(observation(8_410_000_000_000, 8_410_100_000_000, 3, 3_000))
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
        .flatten_account(
            account_id,
            observation(8_410_000_000_000, 8_410_100_000_000, 4, 4_000),
        )
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
fn startup_repairs_legacy_nonflat_position_with_missing_average_from_fill_ledger() {
    let directory = TestDirectory::new("position-repair");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");

    let mut first = market_order("repair-first", OrderSide::Buy, 1, 1_000);
    first.quantity = FixedPoint::try_new(3, 0).expect("quantity");
    service.place_order(first).expect("first order accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("first fill");

    let mut second = market_order("repair-second", OrderSide::Buy, 3, 3_000);
    second.quantity = FixedPoint::try_new(4, 0).expect("quantity");
    service.place_order(second).expect("second order accepted");
    service
        .observe_market(observation(10_000, 10_001, 4, 4_000))
        .expect("second fill");

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

    let reopened = TradingService::start(config(&directory)).expect("service reopens and repairs");
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
    let fills = reopened
        .observe_market(observation(10_024, 10_025, 6, 6_000))
        .expect("new market update succeeds after repair");
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .place_order(market_order("legacy-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let connection =
        rusqlite::Connection::open(directory.0.join("trading.sqlite3")).expect("database opens");
    connection
        .execute("DELETE FROM fills", [])
        .expect("retained fill ledger removed");
    connection
        .execute(
            "UPDATE positions SET average_units = NULL, average_scale = NULL
             WHERE account_id = 'aeris-sim-1'",
            [],
        )
        .expect("legacy corruption injected");
    drop(connection);

    let reopened = TradingService::start(config(&directory)).expect("service reopens");
    reopened
        .place_order(market_order("must-not-fill", OrderSide::Buy, 3, 3_000))
        .expect("order itself is accepted");
    let error = reopened
        .observe_market(observation(10_024, 10_025, 4, 4_000))
        .expect_err("invalid existing position blocks before fill commit");
    assert!(error.contains("position average is unavailable"));
    let snapshot = reopened.snapshot().expect("snapshot");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    service.place_order(market).expect("market order accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("market order fills");
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
fn managed_bracket_activates_after_entry_and_oco_survives_restart() {
    let directory = TestDirectory::new("managed-bracket");
    let service = start_service(&directory);
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
    let BracketPlacement::Managed(awaiting) = awaiting else {
        panic!("a simulated bracket is managed locally");
    };
    assert_eq!(awaiting.status, ManagedBracketStatus::AwaitingEntry);
    assert!(awaiting.stop_client_order_id.is_none());

    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    let active = service.snapshot().expect("active snapshot");
    let bracket = &active.managed_brackets[0];
    assert_eq!(bracket.status, ManagedBracketStatus::Active);
    assert_eq!(bracket.target_client_order_ids.len(), 2);
    assert_eq!(ManagedBracket::MANAGEMENT_LABEL, "LOCAL-MANAGED");
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

    service
        .observe_market(observation(10_200, 10_225, 3, 3_000))
        .expect("first target fills");
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
    assert_eq!(
        stop.stop_price,
        Some(FixedPoint::try_new(10_025, 2).expect("break even stop"))
    );

    service
        .observe_market(observation(10_000, 10_025, 4, 4_000))
        .expect("stop fills");
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
        ManagedBracketStatus::Completed
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
    let service = start_service(&directory);
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

    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("market below the entry");
    let resting = service.snapshot().expect("resting snapshot");
    assert_eq!(
        resting.managed_brackets[0].status,
        ManagedBracketStatus::AwaitingEntry
    );
    assert!(resting.fills.is_empty(), "the entry waits for its price");

    service
        .observe_market(observation(10_075, 10_100, 3, 3_000))
        .expect("market reaches the entry");
    let armed = service.snapshot().expect("armed snapshot");
    assert_eq!(
        armed.managed_brackets[0].status,
        ManagedBracketStatus::Active
    );
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
    let service = start_service(&directory);
    service.register_instrument(instrument()).expect("terms");
    let place_limit = |id, side, limit| {
        let mut order = market_order(id, side, 1, 1_000);
        order.order_type = OrderType::Limit;
        order.time_in_force = TimeInForce::GoodTillCancelled;
        order.limit_price = Some(price(limit));
        service.place_order(order).expect("limit accepted")
    };
    let buy = place_limit("resting-buy", OrderSide::Buy, 9_900);
    service
        .observe_market(observation(9_775, 9_800, 2, 2_000))
        .expect("ask crosses the buy limit");
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
    service
        .observe_market(observation(10_300, 10_325, 3, 3_000))
        .expect("bid crosses the sell limit");
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
    let service = start_service(&directory);
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
    let fills = service
        .observe_market(observation(9_775, 9_800, 2, 2_000))
        .expect("quote crosses all three");
    assert_eq!(
        fills.iter().map(|fill| &fill.order_id).collect::<Vec<_>>(),
        vec![&placed[1].id, &placed[2].id, &placed[0].id]
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn crossed_stops_precede_limits_in_trigger_price_order() {
    let directory = TestDirectory::new("stop-priority");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut placed = Vec::new();
    for (id, kind, level) in [
        ("limit-first", OrderType::Limit, 10_500),
        ("high-stop", OrderType::Stop, 10_200),
        ("low-stop", OrderType::Stop, 10_100),
    ] {
        let mut request = market_order(id, OrderSide::Buy, 1, 1_000);
        request.order_type = kind;
        request.time_in_force = TimeInForce::GoodTillCancelled;
        match kind {
            OrderType::Limit => request.limit_price = Some(price(level)),
            OrderType::Stop => request.stop_price = Some(price(level)),
            _ => unreachable!("fixture"),
        }
        placed.push(service.place_order(request).expect("order"));
    }
    let fills = service
        .observe_market(observation(10_275, 10_300, 2, 2_000))
        .expect("quote crosses all three");
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
        let service = start_service(&directory);
        let mut terms = instrument();
        if label == "crypto" {
            terms.instrument_id = InstrumentId::try_new("hyperliquid:perp:BTC").expect("id");
            terms.contract.instrument_class = aeris_instruments::InstrumentClass::CryptoPerpetual;
            terms.contract.expiry = None;
            terms.contract.last_trade = None;
            terms.contract.session_hours.clear();
            terms.contract.provenance.provider_id = "hyperliquid".into();
            terms.contract.provenance.provider_symbol = "BTC".into();
            terms.contract.provenance.display_symbol = "BTC".into();
        }
        terms.contract.point_value =
            Some(InstrumentDecimal::try_new(point_value, 0).expect("point value"));
        service
            .register_instrument(terms.clone())
            .expect("instrument");
        let mut buy = market_order("multiplier-buy", OrderSide::Buy, 1, 1_000);
        buy.instrument_id = terms.instrument_id.clone();
        buy.quantity = contracts(2);
        service.place_order(buy).expect("buy");
        let mut entry = observation(499_975, 500_000, 2, 2_000);
        entry.instrument_id = terms.instrument_id.clone();
        service.observe_market(entry).expect("entry fill");
        let mut mark = observation(500_050, 500_075, 3, 3_000);
        mark.instrument_id = terms.instrument_id.clone();
        service.observe_market(mark).expect("mark");
        assert_eq!(
            service.snapshot().expect("marked").positions[0]
                .unrealized_pnl
                .units(),
            expected_mark
        );
        let mut sell = market_order("multiplier-sell", OrderSide::Sell, 4, 4_000);
        sell.instrument_id = terms.instrument_id.clone();
        sell.quantity = contracts(2);
        service.place_order(sell).expect("sell");
        let mut exit = observation(500_125, 500_150, 5, 5_000);
        exit.instrument_id = terms.instrument_id;
        service.observe_market(exit).expect("exit fill");
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
fn a_pending_bracket_carries_its_planned_legs_and_arms_them_where_they_were_moved() {
    let directory = TestDirectory::new("planned-bracket-legs");
    let service = start_service(&directory);
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
        service.move_planned_bracket_leg(MovePlannedBracketLeg {
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

    let restarted = start_service(&directory);
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(
        held_bracket_legs(&restored),
        moved_legs,
        "the held legs survive a restart at their moved prices"
    );
    restarted
        .observe_market(observation(10_100, 10_125, 3, 3_000))
        .expect("market reaches the entry");
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
        .planned_bracket_legs
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
    let service = start_service(&directory);
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
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills and arms exits");
    let armed = service.snapshot().expect("armed");
    assert_eq!(armed.fills.len(), 1);
    assert_eq!(
        armed
            .orders
            .iter()
            .filter(|order| order.status.is_open())
            .count(),
        3
    );

    let fills = service
        .observe_market(observation(9_775, 10_225, 3, 3_000))
        .expect("wide quote");
    assert_eq!(
        fills.len(),
        1,
        "a sibling must not execute from a stale candidate"
    );
    let settled = service.snapshot().expect("settled");
    assert_eq!(settled.positions[0].net_quantity.units(), 0);
    assert_eq!(
        settled.managed_brackets[0].status,
        ManagedBracketStatus::Completed
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
    assert!(matches!(placement, BracketPlacement::Managed(_)));
}

fn quantity(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 0).expect("quantity")
}

#[test]
fn bracket_entry_that_reverses_a_position_protects_only_the_reversed_exposure() {
    let directory = TestDirectory::new("bracket-reversal");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .place_order(market_order("existing-long", OrderSide::Buy, 1, 1_000))
        .expect("long accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("long fills");
    let mut entry = market_order("reversing-entry", OrderSide::Sell, 3, 3_000);
    entry.quantity = quantity(3);
    place_managed_bracket(&service, entry);

    service
        .observe_market(observation(9_975, 10_000, 4, 4_000))
        .expect("reversing entry fills and arms its bracket");
    let armed = service.snapshot().expect("armed snapshot");
    assert_eq!(armed.market_observation_error, None);
    assert_eq!(
        armed.positions[0].net_quantity,
        FixedPoint::try_new(-2, 0).expect("short")
    );
    assert_eq!(
        armed.managed_brackets[0].status,
        ManagedBracketStatus::Active
    );
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
        service
            .observe_market(observation(10_175, 10_200, 5, 5_000))
            .expect("stop fills")
            .len(),
        1
    );
    let stopped = service.snapshot().expect("stopped snapshot");
    assert_eq!(stopped.positions[0].net_quantity.units(), 0);
    assert_eq!(
        stopped.managed_brackets[0].status,
        ManagedBracketStatus::Completed
    );
    assert!(stopped.orders.iter().all(|order| !order.status.is_open()));
}

#[test]
fn bracket_entry_that_only_reduces_a_position_completes_without_children() {
    let directory = TestDirectory::new("bracket-reduction");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut long = market_order("existing-long", OrderSide::Buy, 1, 1_000);
    long.quantity = quantity(2);
    service.place_order(long).expect("long accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("long fills");
    place_managed_bracket(
        &service,
        market_order("reducing-entry", OrderSide::Sell, 3, 3_000),
    );

    service
        .observe_market(observation(9_975, 10_000, 4, 4_000))
        .expect("reducing entry fills");
    let reduced = service.snapshot().expect("reduced snapshot");
    assert_eq!(reduced.market_observation_error, None);
    assert_eq!(reduced.positions[0].net_quantity, quantity(1));
    let bracket = &reduced.managed_brackets[0];
    assert_eq!(bracket.status, ManagedBracketStatus::Completed);
    assert_eq!(bracket.stop_client_order_id, None);
    assert_eq!(bracket.target_client_order_ids, []);
    assert!(reduced.orders.iter().all(|order| !order.status.is_open()));
}

#[test]
fn bracket_that_cannot_be_armed_is_rejected_once_and_other_orders_still_fill() {
    let directory = TestDirectory::new("bracket-arming-failure");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    place_managed_bracket(
        &service,
        market_order("unprotectable-entry", OrderSide::Buy, 1, 1_000),
    );
    // Filled at 0.50, the 8-tick stop would sit at -1.50.
    service
        .observe_market(observation(25, 50, 2, 2_000))
        .expect("entry fills even though its stop cannot be placed");
    let rejected = service.snapshot().expect("rejected snapshot");
    assert_eq!(rejected.market_observation_error, None);
    assert_eq!(rejected.positions[0].net_quantity, quantity(1));
    assert_eq!(
        rejected.managed_brackets[0].status,
        ManagedBracketStatus::Cancelled
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

    let mut stop = market_order("manual-stop", OrderSide::Sell, 3, 3_000);
    stop.order_type = OrderType::Stop;
    stop.time_in_force = TimeInForce::GoodTillCancelled;
    stop.stop_price = Some(FixedPoint::try_new(50, 2).expect("on-grid stop"));
    service.place_order(stop).expect("manual stop accepted");
    assert_eq!(
        service
            .observe_market(observation(30, 55, 4, 4_000))
            .expect("the rejected bracket is not retried")
            .len(),
        1,
        "the manual stop fills"
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("protected-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    service.place_order(entry).expect("entry accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
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
        .place_protective_order(PlaceProtectiveOrder {
            order: protective,
            role: ProtectiveOrderRole::StopLoss,
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
            .place_protective_order(PlaceProtectiveOrder {
                order: oversized,
                role: ProtectiveOrderRole::StopLoss,
            })
            .expect_err("oversized protection rejected")
            .contains("must reduce")
    );
    assert_eq!(
        service.snapshot().expect("snapshot").protective_orders,
        vec![ProtectiveOrder {
            client_order_id: ClientOrderId::try_new("protective-stop").expect("client id"),
            role: ProtectiveOrderRole::StopLoss,
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("protected-long", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    service.place_order(entry).expect("entry accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    let protect = |client_id: &str, sequence, role, price_units| {
        let mut order = market_order(client_id, OrderSide::Sell, sequence, 3_000);
        order.quantity = FixedPoint::try_new(1, 0).expect("quantity");
        let price = Some(FixedPoint::try_new(price_units, 2).expect("price"));
        match role {
            ProtectiveOrderRole::StopLoss => {
                order.order_type = OrderType::Stop;
                order.stop_price = price;
            }
            ProtectiveOrderRole::TakeProfit => {
                order.order_type = OrderType::Limit;
                order.limit_price = price;
            }
        }
        service
            .place_protective_order(PlaceProtectiveOrder { order, role })
            .expect("protection accepted");
    };
    protect("stop", 3, ProtectiveOrderRole::StopLoss, 9_800);
    protect("far-target", 4, ProtectiveOrderRole::TakeProfit, 10_300);
    protect("near-target", 5, ProtectiveOrderRole::TakeProfit, 10_200);

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
fn protected_long(service: &TradingService, quantity: i64) {
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    service.place_order(entry).expect("entry accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    for (client_id, role, price) in [
        ("stop", ProtectiveOrderRole::StopLoss, 9_800),
        ("target", ProtectiveOrderRole::TakeProfit, 10_300),
    ] {
        let mut order = market_order(client_id, OrderSide::Sell, 3, 3_000);
        order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
        let price = Some(FixedPoint::try_new(price, 2).expect("price"));
        if role == ProtectiveOrderRole::StopLoss {
            order.order_type = OrderType::Stop;
            order.stop_price = price;
        } else {
            order.order_type = OrderType::Limit;
            order.limit_price = price;
        }
        service
            .place_protective_order(PlaceProtectiveOrder { order, role })
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

fn market(service: &TradingService, client_id: &str, side: OrderSide, quantity: i64, step: u64) {
    let time = i64::try_from(step).expect("step") * 1_000;
    let mut order = market_order(client_id, side, step, time);
    order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    service.place_order(order).expect("order accepted");
    service
        .observe_market(observation(9_975, 10_000, step + 1, time + 500))
        .expect("order fills");
}

#[test]
fn protection_follows_the_position_as_it_grows_shrinks_and_closes() {
    let directory = TestDirectory::new("protection-follows-position");
    let service = start_service(&directory);
    protected_long(&service, 2);

    market(&service, "add", OrderSide::Buy, 4, 4);
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

    let service = start_service(&directory);
    let mut protection = open_protection(&service);
    protection.sort();
    assert_eq!(protection, grown, "the grown protection survives a restart");

    market(&service, "trim", OrderSide::Sell, 2, 6);
    let mut protection = open_protection(&service);
    protection.sort();
    assert_eq!(
        protection,
        vec![("stop".to_string(), 4), ("target".to_string(), 4)],
        "a partial close shrinks the protection"
    );

    market(&service, "exit", OrderSide::Sell, 4, 8);
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
    let service = start_service(&directory);
    protected_long(&service, 2);
    service
        .observe_market(observation(9_775, 9_800, 4, 4_000))
        .expect("stop fills");
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
    let service = start_service(&directory);
    protected_long(&service, 2);
    market(&service, "reverse", OrderSide::Sell, 5, 4);
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
    let directory = TestDirectory::new("intraday-drawdown");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "intraday-100".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
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
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    service
        .observe_market(observation(10_300, 10_325, 3, 3_000))
        .expect("peak updates");
    let peak = service.snapshot().expect("peak snapshot");
    assert_eq!(
        peak.risk_rule_states[0].peak_session_pnl,
        FixedPoint::try_new(15_000, 2).expect("peak")
    );
    assert_eq!(
        peak.risk_meters[0].trailing_drawdown_remaining,
        Some(FixedPoint::try_new(10_000, 2).expect("remaining"))
    );
    service
        .observe_market(observation(10_100, 10_125, 4, 4_000))
        .expect("drawdown observation applies");
    let locked = service.snapshot().expect("locked snapshot");
    assert!(locked.risk_locks[0].reason.contains("intraday trailing"));
    assert_eq!(
        locked.risk_meters[0].trailing_drawdown_remaining,
        Some(FixedPoint::try_new(0, 2).expect("remaining"))
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let restarted = start_service(&directory);
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
    let service = start_service(&directory);
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
    let service = start_service(&directory);
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
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
        service
            .place_order(market_order(
                entry_id,
                OrderSide::Buy,
                sequence,
                1_000 * sequence_time,
            ))
            .expect("entry accepted");
        service
            .observe_market(observation(
                9_975,
                10_000,
                sequence + 1,
                1_000 * (sequence_time + 1),
            ))
            .expect("entry fills");
        service
            .place_order(market_order(
                exit_id,
                OrderSide::Sell,
                sequence + 2,
                1_000 * (sequence_time + 2),
            ))
            .expect("exit accepted");
        service
            .observe_market(observation(
                10_100,
                10_125,
                sequence + 3,
                1_000 * (sequence_time + 3),
            ))
            .expect("exit fills");

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
    let service = start_service(&directory);
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
    service
        .observe_market(observation(9_975, 10_000, 4, 2_000))
        .expect("entry fills");
    service
        .place_order(market_order("planned-loss", OrderSide::Sell, 5, 3_000))
        .expect("planned exit accepted");
    service
        .observe_market(observation(9_900, 9_925, 6, 4_000))
        .expect("loss fills and plan lock evaluates");
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");

    let complete_losing_trade = |service: &TradingService, prefix: &str, sequence: u64| {
        service
            .place_order(market_order(
                &format!("{prefix}-entry"),
                OrderSide::Buy,
                sequence,
                i64::try_from(sequence).expect("sequence fits"),
            ))
            .expect("entry accepted");
        service
            .observe_market(observation(
                9_975,
                10_000,
                sequence + 1,
                i64::try_from(sequence + 1).expect("sequence fits"),
            ))
            .expect("entry fills");
        service
            .place_order(market_order(
                &format!("{prefix}-exit"),
                OrderSide::Sell,
                sequence + 2,
                i64::try_from(sequence + 2).expect("sequence fits"),
            ))
            .expect("exit accepted");
        service
            .observe_market(observation(
                9_900,
                9_925,
                sequence + 3,
                i64::try_from(sequence + 3).expect("sequence fits"),
            ))
            .expect("loss fills");
    };
    complete_losing_trade(&service, "loss-one", 1);
    let mut larger = market_order("larger-after-loss", OrderSide::Buy, 5, 5);
    larger.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    assert!(
        service
            .place_order(larger)
            .expect_err("size increase after a loss is reduced")
            .contains("tilt size reduction")
    );
    complete_losing_trade(&service, "loss-two", 6);
    let locked = service.snapshot().expect("snapshot");
    assert_eq!(locked.discipline_states[0].rapid_loss_count, 2);
    let cooldown_until = locked.discipline_states[0]
        .cooldown_until_unix_nanos
        .expect("cooldown");
    assert!(locked.risk_locks[0].reason.contains("rapid-loss cooldown"));
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = start_service(&directory);
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
    let service = start_service(&directory);
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
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000_000_000))
        .expect("entry fills");
    service
        .observe_market(observation(9_775, 9_800, 3, 3_000_000_000))
        .expect("stop fills");
    let evaluation = service
        .evaluate_risk(market_order(
            "fast-reentry",
            OrderSide::Buy,
            4,
            4_000_000_000,
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
    let service = start_service(&directory);
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
    let fills = service
        .observe_market(observation(9_975, 10_000, 3, 2_000))
        .expect("buy fills");
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
    let service = start_service(&directory);
    service
        .place_order(market_order("client-close", OrderSide::Sell, 4, 3_000))
        .expect("sell accepted");
    service
        .observe_market(observation(10_100, 10_125, 5, 4_000))
        .expect("sell fills");
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
    step: u64,
    side: OrderSide,
    quantity: i64,
    (bid, ask): (i64, i64),
) {
    let sequence = step * 2 + 1;
    let time = i64::try_from(sequence).expect("time") * 1_000;
    let mut order = market_order(&format!("round-trip-{step}"), side, sequence, time);
    order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    service.place_order(order).expect("order accepted");
    let fills = service
        .observe_market(observation(bid, ask, sequence + 1, time + 500))
        .expect("market observed");
    assert_eq!(fills.len(), 1, "the market order fills");
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    execute_market_order(&service, 1, OrderSide::Buy, 1, (9_975, 10_000));
    let open = service.snapshot().expect("open snapshot");
    assert_eq!(open.round_trips.len(), 1);
    assert!(!open.round_trips[0].closed);
    assert_eq!(open.round_trips[0].final_pnl, None);
    assert_eq!(open.round_trips[0].exit, None);

    execute_market_order(&service, 2, OrderSide::Sell, 1, (10_100, 10_125));
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");

    execute_market_order(&service, 1, OrderSide::Buy, 2, (9_975, 10_000));
    let snapshot = service.snapshot().expect("open snapshot");
    assert_eq!(snapshot.round_trips[0].current_pnl(), Some(price(-2_500)));
    assert_eq!(snapshot.round_trips[0].final_pnl, None);

    service
        .observe_market(observation(10_100, 10_125, 5, 4_000))
        .expect("live mark");
    let snapshot = service.snapshot().expect("marked snapshot");
    assert_eq!(snapshot.round_trips[0].current_pnl(), Some(price(10_000)));

    execute_market_order(&service, 2, OrderSide::Sell, 1, (10_100, 10_125));
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

    service
        .observe_market(observation(10_200, 10_225, 7, 6_000))
        .expect("next live mark");
    let snapshot = service.snapshot().expect("updated mark snapshot");
    assert_eq!(
        open_trade(&snapshot.round_trips).current_pnl(),
        Some(price(15_000))
    );

    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
    let restarted = start_service(&directory);
    let snapshot = restarted.snapshot().expect("restored open snapshot");
    // The last quote mark is process-local; a new feed quote refreshes it after restart.
    assert_eq!(snapshot.positions[0].net_quantity, contracts(1));
    restarted
        .observe_market(observation(10_200, 10_225, 8, 7_000))
        .expect("fresh mark after restart");
    let snapshot = restarted.snapshot().expect("refreshed open snapshot");
    assert_eq!(
        open_trade(&snapshot.round_trips).current_pnl(),
        Some(price(15_000))
    );

    execute_market_order(&restarted, 3, OrderSide::Sell, 1, (10_200, 10_225));
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    execute_market_order(&service, 1, OrderSide::Buy, 1, (9_975, 10_000));
    execute_market_order(&service, 2, OrderSide::Buy, 1, (10_075, 10_100));
    // Closes the two-lot long and opens a one-lot short in the same fill.
    execute_market_order(&service, 3, OrderSide::Sell, 3, (10_200, 10_225));
    execute_market_order(&service, 4, OrderSide::Buy, 1, (10_075, 10_100));
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
    let directory = TestDirectory::new("v21-fill-accounting");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "accounting-agreement".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
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
        execute_market_order(&service, step, side, quantity, quote);
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
    let restarted = start_service(&directory);
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    execute_market_order(&service, 1, OrderSide::Buy, 1, (9_975, 10_000));
    for step in 0..4 {
        execute_market_order(&service, 2 + step * 2, OrderSide::Buy, 1, (9_975, 10_000));
        execute_market_order(&service, 3 + step * 2, OrderSide::Sell, 1, (9_975, 10_000));
    }
    execute_market_order(&service, 10, OrderSide::Sell, 1, (9_975, 10_000));
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
    let service = start_service(&directory);
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
    let service = start_service(&directory);
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .place_order(market_order("flatten-open", OrderSide::Buy, 1, 1_000))
        .expect("buy accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("buy fills");
    let fills = service
        .flatten_account(
            TradingAccountId::try_new("aeris-sim-1").expect("account"),
            observation(10_100, 10_125, 3, 3_000),
        )
        .expect("flatten succeeds");
    assert_eq!(fills.fills.len(), 1);
    assert_eq!(fills.pending_close_requests.len(), 0);
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

    let restarted = start_service(&directory);
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
        .apply_economic_event_risk(event.clone(), None)
        .expect("event rule applies");
    assert_eq!(first.locked_accounts, 1);
    assert_eq!(
        service
            .apply_economic_event_risk(event, None)
            .expect("duplicate is fenced")
            .locked_accounts,
        0
    );
    for index in 0..5 {
        service
            .apply_economic_event_risk(
                EconomicEventRiskTrigger {
                    event_id: format!("retention-event-{index}"),
                    title: "Scheduled release".to_string(),
                    source: "Official fixture".to_string(),
                    importance: EconomicEventRiskImportance::High,
                    scheduled_unix_nanos: scheduled,
                    source_release_unix_nanos: observed - 1,
                    observed_unix_nanos: observed + index,
                },
                None,
            )
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
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
    assert_eq!(
        service
            .observe_market(observation(9_975, 10_000, 3, 2_000))
            .expect("orders fill")
            .len(),
        2
    );
    assert_eq!(
        service
            .flatten_all(observation(10_100, 10_125, 4, 3_000))
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
        .place_order(market_order("reverse-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry accepts");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    let fills = service
        .reverse_position(
            TradingAccountId::try_new("aeris-sim-1").expect("account"),
            observation(10_100, 10_125, 3, 3_000),
        )
        .expect("position reverses");
    assert_eq!(fills.fills.len(), 2);
    assert_eq!(fills.pending_close_requests.len(), 0);
    let position = service.snapshot().expect("snapshot").positions[0].clone();
    assert_eq!(position.net_quantity.units(), -1);
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
    let service = start_service(&directory);
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
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    service
        .place_order(market_order("entry", OrderSide::Buy, 1, 1_000))
        .expect("entry");
    service
        .observe_market(observation(500_000, 500_025, 2, 2_000))
        .expect("fill");
    let mut stop = market_order("stop", OrderSide::Sell, 3, 3_000);
    stop.order_type = OrderType::Stop;
    stop.stop_price = Some(FixedPoint::try_new(499_000, 2).expect("stop"));
    service
        .place_protective_order(PlaceProtectiveOrder {
            order: stop,
            role: ProtectiveOrderRole::StopLoss,
        })
        .expect("protective stop");
    service
        .cancel_order(ClientOrderId::try_new("stop").expect("id"))
        .expect("cancel");
    service
        .observe_market(observation(500_050, 500_075, 4, 4_000))
        .expect("observe");
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
