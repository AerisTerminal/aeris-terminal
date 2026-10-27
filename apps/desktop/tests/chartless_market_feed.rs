use aeris_contracts::{
    InstallProviderInstrument, ProviderContractMetadata, ProviderInstrumentClass,
};
use aeris_instruments::InstrumentId;
use aeris_market_runtime::market_service::{MarketService, ScriptedQuoteProvider, TradingFeedLink};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, FixedPoint, OrderSide, OrderStatus, OrderType, TimeInForce,
    TradingAccount, TradingAccountId, TradingProvenance,
};
use aeris_trading_runtime::{
    ExecutionPolicy, HistoryQuery, PlaceOrder, TradingRetention, TradingService,
    TradingServiceConfig, clock::ManualClock,
};
use std::{sync::Arc, time::Duration};

fn current(trading: &TradingService) -> aeris_trading_runtime::WorkingSet {
    let subscription = trading.subscribe().unwrap();
    *subscription
        .recv_timeout(Duration::from_secs(2))
        .expect("initial owner publication")
        .event
        .working_set
}

fn now() -> i64 {
    static AT: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap()
    })
}

fn instrument() -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "hyperliquid".into(),
        instrument_id: "hyperliquid:perp:BTC".into(),
        provider_symbol: "BTC".into(),
        display_symbol: "BTC-USDC".into(),
        venue_id: "Hyperliquid".into(),
        entitlement_id: "hyperliquid-public".into(),
        session_generation: 1,
        selection_generation: 1,
        price_scale: 2,
        quantity_scale: 0,
        price_increment: Some(25),
        contract_metadata: Some(Box::new(ProviderContractMetadata {
            currency: Some("USD".into()),
            point_value: Some(1),
            point_value_scale: Some(0),
            instrument_class: Some(ProviderInstrumentClass::CryptoPerpetual as i32),
            order_quantity_increment: Some(1),
            ..Default::default()
        })),
    }
}

fn order(id: &str, side: OrderSide, kind: OrderType, stop: Option<i64>) -> PlaceOrder {
    PlaceOrder {
        client_order_id: ClientOrderId::try_new(id).unwrap(),
        account_id: TradingAccountId::try_new("aeris-sim-chartless").unwrap(),
        instrument_id: InstrumentId::try_new("hyperliquid:perp:BTC").unwrap(),
        side,
        order_type: kind,
        time_in_force: TimeInForce::GoodTillCancelled,
        quantity: FixedPoint::try_new(1, 0).unwrap(),
        limit_price: None,
        stop_price: stop.map(|price| FixedPoint::try_new(price, 2).unwrap()),
        submitted_unix_nanos: now(),
        provenance: TradingProvenance {
            venue_id: "aeris-sim".into(),
            provider_id: "hyperliquid".into(),
            session_generation: 1,
            source_sequence: 1,
            observed_unix_nanos: now(),
        },
    }
}

fn start_fixture() -> (
    std::path::PathBuf,
    Arc<ManualClock>,
    TradingService,
    MarketService,
    ScriptedQuoteProvider,
) {
    let path = std::env::temp_dir().join(format!(
        "aeris-chartless-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&path).unwrap();
    let clock = Arc::new(ManualClock::new(now()));
    let trading = TradingService::start(TradingServiceConfig {
        database_path: path.join("trading.sqlite3"),
        retention: TradingRetention::default(),
        clock: clock.clone(),
        execution: ExecutionPolicy::default(),
    })
    .unwrap();
    let (market, provider) = MarketService::start_scripted_quotes(instrument()).unwrap();
    let inbox = trading
        .attach_market_feed(market.trading_quote_interest_sink())
        .unwrap();
    market
        .attach_trading_feed(TradingFeedLink {
            events: Box::new(move |event| {
                inbox
                    .push(event)
                    .expect("bounded trading feed accepts event");
            }),
        })
        .unwrap();
    trading
        .register_account(TradingAccount {
            id: TradingAccountId::try_new("aeris-sim-chartless").unwrap(),
            display_name: "SIM chartless fixture".into(),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".into(),
            broker_ref: None,
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: Some(FixedPoint::try_new(1_000_000, 2).unwrap()),
        })
        .unwrap();
    let route = aeris_market_runtime::market_service::instrument_terms_from_install(&instrument())
        .unwrap()
        .route;
    trading.set_ticket_interest(vec![route]).unwrap();
    assert_eq!(market.status().unwrap().retained_series, 0);
    (path, clock, trading, market, provider)
}

fn await_entry(trading: &TradingService, market: &MarketService, deadline: std::time::Instant) {
    loop {
        match trading.place_order(
            order("chartless-buy", OrderSide::Buy, OrderType::Market, None),
            false,
        ) {
            Ok(_) => break,
            Err(error) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "entry not admitted: {error}; {:?}",
                    market.status()
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

fn await_released_lease(provider: &ScriptedQuoteProvider, deadline: std::time::Instant) {
    while provider.subscriptions().unwrap() != (0, 0) {
        assert!(
            std::time::Instant::now() < deadline,
            "terminal lease retained"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        provider.quote(10_075, 10_100, 6, now() + 6_000_000),
        Err("scripted quote has no active instrument lease".into())
    );
}

#[test]
fn chartless_scripted_provider_marks_and_executes_stop_through_both_attachments() {
    let (path, clock, trading, market, provider) = start_fixture();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match provider.quote(10_000, 10_025, 1, now()) {
            Ok(()) => break,
            Err(error)
                if error == "scripted quote has no active instrument lease"
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!(
                "quote lease was not established: {error}; market={:?}; trading={:?}",
                market.status(),
                current(&trading)
            ),
        }
    }
    assert_eq!(provider.subscriptions().unwrap(), (1, 0));
    // Terms, session and feed state must arrive through the market's
    // catalog/session/provider lanes before trading can admit the entry.
    await_entry(&trading, &market, deadline);
    clock.advance(Duration::from_millis(1));
    provider
        .quote(10_025, 10_050, 2, now() + 1_000_000)
        .unwrap();
    while current(&trading).fills.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "entry not filled: {:?}",
            current(&trading)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    trading.set_ticket_interest(vec![]).unwrap();
    assert_eq!(provider.subscriptions().unwrap(), (1, 0));
    let baseline = current(&trading);
    assert_eq!(baseline.positions.len(), 1);
    assert_eq!(baseline.positions[0].net_quantity.units(), 1);
    assert_eq!(market.status().unwrap().retained_series, 0);

    // The entry now takes the first ask (100.25) at placement, not the next ask (100.50).
    for (sequence, bid, expected) in [(3, 10_075, 50), (4, 10_100, 75)] {
        provider
            .quote(
                bid,
                bid + 25,
                sequence,
                now() + i64::try_from(sequence).unwrap() * 1_000_000,
            )
            .unwrap();
        while current(&trading).positions[0].unrealized_pnl.units() != expected {
            assert!(std::time::Instant::now() < deadline, "mark not updated");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(market.status().unwrap().retained_series, 0);
        assert_eq!(provider.subscriptions().unwrap(), (1, 0));
    }
    let stop = trading
        .place_order(
            order(
                "chartless-stop",
                OrderSide::Sell,
                OrderType::Stop,
                Some(10_050),
            ),
            false,
        )
        .unwrap();
    assert_eq!(stop.status, OrderStatus::Working);
    clock.advance(Duration::from_millis(1));
    provider
        .quote(10_050, 10_075, 5, now() + 5_000_000)
        .unwrap();
    while !current(&trading).positions.is_empty() {
        assert!(std::time::Instant::now() < deadline, "stop did not flatten");
        std::thread::sleep(Duration::from_millis(2));
    }
    let final_state = current(&trading);
    assert_eq!(final_state.fills.len(), 2);
    assert_eq!(final_state.positions, []);
    assert!(
        final_state
            .orders
            .iter()
            .all(|order| !order.status.is_open())
    );
    assert_eq!(
        trading
            .history(HistoryQuery::default())
            .unwrap()
            .orders
            .iter()
            .find(|order| order.client_order_id == stop.client_order_id)
            .unwrap()
            .status,
        OrderStatus::Filled
    );
    assert_eq!(market.status().unwrap().retained_series, 0);
    await_released_lease(&provider, deadline);
    trading.shutdown(Duration::from_secs(2)).unwrap();
    market.shutdown(Duration::from_secs(2)).unwrap();
    std::fs::remove_dir_all(path).unwrap();
}
