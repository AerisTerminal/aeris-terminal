//! Frozen simulated-venue behavior, recorded before broker venue routing was introduced.
//! Do not update the expected ledger to accommodate a simulated-path refactor.

use super::*;
use std::fmt::Write;

fn point(value: FixedPoint) -> String {
    format!("{}@{}", value.units(), value.scale())
}

fn optional_point(value: Option<FixedPoint>) -> String {
    value.map_or_else(|| "-".to_string(), point)
}

fn origin(value: &TradingProvenance) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        value.venue_id,
        value.provider_id,
        value.session_generation,
        value.source_sequence,
        value.observed_unix_nanos
    )
}

fn record(stage: &str, service: &TradingService, trace: &mut String) {
    let snapshot = service.snapshot().expect("stage snapshot");
    let orders = snapshot
        .orders
        .iter()
        .map(|order| format!("{}:{}", order.id.as_str(), order.status.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let events = snapshot
        .order_events
        .iter()
        .map(|event| event.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let fills = snapshot
        .fills
        .iter()
        .map(|fill| fill.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let pnl = &snapshot.account_pnl[0];
    writeln!(
        trace,
        "{stage} revision={} orders=[{orders}] events=[{events}] fills=[{fills}] pnl={}/{}/{}",
        snapshot.revision,
        point(pnl.realized),
        point(pnl.unrealized),
        optional_point(pnl.equity)
    )
    .expect("write stage");
}

fn ledger(snapshot: &TradingSnapshot) -> String {
    let mut result = String::new();
    writeln!(result, "revision={}", snapshot.revision).expect("write revision");
    for order in &snapshot.orders {
        writeln!(
            result,
            "order {} {} {} {} {} {} {} {} {} {} {} {}",
            order.id.as_str(),
            order.client_order_id.as_str(),
            order.side.as_str(),
            order.order_type.as_str(),
            order.time_in_force.as_str(),
            point(order.quantity),
            point(order.filled_quantity),
            optional_point(order.limit_price),
            optional_point(order.stop_price),
            order.status.as_str(),
            order.submitted_unix_nanos,
            origin(&order.provenance)
        )
        .expect("write order");
    }
    for event in &snapshot.order_events {
        writeln!(
            result,
            "event {} {} {} {} {} {:?} {}",
            event.id.as_str(),
            event.order_id.as_str(),
            event.sequence,
            event.kind.as_str(),
            event.event_unix_nanos,
            event.detail,
            origin(&event.provenance)
        )
        .expect("write event");
    }
    for fill in &snapshot.fills {
        writeln!(
            result,
            "fill {} {} {} {} {} {} {} {} {}",
            fill.id.as_str(),
            fill.order_id.as_str(),
            fill.account_id.as_str(),
            fill.instrument_id.as_str(),
            fill.side.as_str(),
            point(fill.price),
            point(fill.quantity),
            fill.execution_unix_nanos,
            origin(&fill.provenance)
        )
        .expect("write fill");
    }
    for position in &snapshot.positions {
        writeln!(
            result,
            "position {} {} {} {} {} {} {}",
            position.account_id.as_str(),
            position.instrument_id.as_str(),
            point(position.net_quantity),
            optional_point(position.average_entry_price),
            point(position.realized_pnl),
            point(position.unrealized_pnl),
            position.last_fill_unix_nanos
        )
        .expect("write position");
    }
    for pnl in &snapshot.account_pnl {
        writeln!(
            result,
            "pnl {} {} {} {} {}",
            pnl.account_id.as_str(),
            pnl.currency,
            point(pnl.realized),
            point(pnl.unrealized),
            optional_point(pnl.equity)
        )
        .expect("write pnl");
    }
    for (id, value) in &snapshot.fill_realized_pnl {
        writeln!(result, "fill_pnl {} {}", id.as_str(), point(*value)).expect("write fill pnl");
    }
    for (id, value) in &snapshot.completed_trade_pnl {
        writeln!(result, "trade_pnl {} {}", id.as_str(), point(*value))
            .expect("write completed pnl");
    }
    writeln!(result, "managed_brackets={:?}", snapshot.managed_brackets).expect("write brackets");
    writeln!(result, "risk_locks={:?}", snapshot.risk_locks).expect("write locks");
    result
}

fn start_golden_service(directory: &TestDirectory) -> TradingService {
    let mut configuration = config(directory);
    configuration.retention.maximum_fills = 64;
    configuration.retention.maximum_order_events = 64;
    let service = TradingService::start(configuration).expect("service starts");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .register_account(TradingAccount {
            id: account.clone(),
            display_name: "SIM • Test".to_string(),
            environment: AccountEnvironment::Simulated,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: Some(FixedPoint::try_new(5_000_000, 2).expect("equity")),
        })
        .expect("account registers");
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
}

fn script_place_touch_modify_cancel_flatten(
    service: &TradingService,
    account: &TradingAccountId,
    trace: &mut String,
) {
    let mut limit = market_order("touch-limit", OrderSide::Buy, 1, 1_000);
    limit.order_type = OrderType::Limit;
    limit.time_in_force = TimeInForce::GoodTillCancelled;
    limit.limit_price = Some(FixedPoint::try_new(10_000, 2).expect("limit price"));
    service.place_order(limit).expect("limit accepted");
    record("place", service, trace);
    assert_eq!(
        service
            .observe_market(observation(10_000, 10_025, 2, 2_000))
            .expect("above limit"),
        Vec::<Fill>::new()
    );
    record("no touch", service, trace);
    service
        .observe_market(observation(9_975, 10_000, 3, 3_000))
        .expect("limit touched");
    record("touch fill", service, trace);

    let mut resting = market_order("modify-cancel", OrderSide::Sell, 4, 4_000);
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(FixedPoint::try_new(10_500, 2).expect("resting price"));
    service.place_order(resting).expect("resting order");
    service
        .modify_order(ModifyOrder {
            client_order_id: ClientOrderId::try_new("modify-cancel").expect("client id"),
            time_in_force: TimeInForce::GoodTillCancelled,
            limit_price: Some(FixedPoint::try_new(10_400, 2).expect("modified price")),
            stop_price: None,
            modified_unix_nanos: 5_000,
            provenance: provenance(5, 5_000),
        })
        .expect("modify resting order");
    record("modify", service, trace);
    service
        .cancel_order(ClientOrderId::try_new("modify-cancel").expect("client id"))
        .expect("cancel resting order");
    record("cancel", service, trace);
    service
        .flatten_account(account.clone(), observation(10_100, 10_125, 6, 6_000))
        .expect("flatten first position");
    record("flatten", service, trace);
}

#[test]
fn simulated_place_touch_modify_cancel_bracket_flatten_reverse_kill_golden() {
    let directory = TestDirectory::new("simulated-golden");
    let service = start_golden_service(&directory);
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut trace = String::new();
    script_place_touch_modify_cancel_flatten(&service, &account, &mut trace);

    let template = BracketStrategyTemplate {
        template_id: "golden-bracket".to_string(),
        revision: 1,
        name: "Golden stop and target".to_string(),
        stop_offset_ticks: 8,
        targets: vec![BracketTarget {
            offset_ticks: 8,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    };
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("bracket-entry", OrderSide::Buy, 7, 7_000),
            template,
        })
        .expect("bracket accepted");
    service
        .observe_market(observation(10_000, 10_025, 8, 8_000))
        .expect("bracket entry fills");
    record("bracket active", &service, &mut trace);
    service
        .flatten_account(account.clone(), observation(10_100, 10_125, 9, 9_000))
        .expect("flatten bracket position");
    record("bracket flatten", &service, &mut trace);

    service
        .place_order(market_order("reverse-entry", OrderSide::Buy, 10, 10_000))
        .expect("reverse entry accepted");
    service
        .observe_market(observation(10_000, 10_025, 11, 11_000))
        .expect("reverse entry fills");
    service
        .reverse_position(account.clone(), observation(10_100, 10_125, 12, 12_000))
        .expect("reverse position");
    record("reverse", &service, &mut trace);

    let mut pending = market_order("kill-pending", OrderSide::Buy, 13, 13_000);
    pending.order_type = OrderType::Limit;
    pending.time_in_force = TimeInForce::GoodTillCancelled;
    pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("pending price"));
    service.place_order(pending).expect("pending order");
    assert_eq!(
        service
            .kill_switch(Some(account), "golden kill".to_string(), 14_000)
            .expect("kill switch"),
        1
    );
    record("kill switch", &service, &mut trace);

    let actual = format!(
        "{trace}\n{}",
        ledger(&service.snapshot().expect("final snapshot"))
    );
    assert_eq!(
        actual.trim_end(),
        include_str!("simulated_golden.txt").trim_end()
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn simulated_bracket_stop_touch_cancels_target_golden() {
    let directory = TestDirectory::new("simulated-stop-golden");
    let service = start_golden_service(&directory);
    let mut trace = String::new();
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("stop-entry", OrderSide::Buy, 1, 1_000),
            template: BracketStrategyTemplate {
                template_id: "stop-golden".to_string(),
                revision: 1,
                name: "Stop touch fixture".to_string(),
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
    record("bracket placed", &service, &mut trace);
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry touch");
    record("entry touched", &service, &mut trace);
    service
        .observe_market(observation(9_750, 9_775, 3, 3_000))
        .expect("stop touch");
    record("stop touched", &service, &mut trace);
    let actual = format!(
        "{trace}\n{}",
        ledger(&service.snapshot().expect("final snapshot"))
    );
    assert_eq!(
        actual.trim_end(),
        include_str!("simulated_stop_golden.txt").trim_end()
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}
