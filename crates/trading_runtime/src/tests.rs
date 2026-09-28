use super::*;
use aeris_instruments::{
    ContractDate, InstrumentDecimal, InstrumentMetadataProvenance, SessionHours,
};
use std::{fs, path::PathBuf};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
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

fn config(directory: &TestDirectory) -> TradingServiceConfig {
    TradingServiceConfig {
        database_path: directory.0.join("trading.sqlite3"),
        retention: TradingRetention {
            maximum_orders: 4_096,
            maximum_fills: 8,
            maximum_order_events: 16,
            maximum_user_records_per_kind: 4,
        },
    }
}

fn instrument() -> TradingInstrument {
    TradingInstrument {
        instrument_id: InstrumentId::try_new("instrument:fixture:CME:ESZ6").expect("instrument"),
        price_scale: 2,
        quantity_scale: 0,
        contract: ContractMetadata {
            tick_size: Some(InstrumentDecimal::try_new(25, 2).expect("tick")),
            point_value: Some(InstrumentDecimal::try_new(5_000, 2).expect("point value")),
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
                session_generation: 1,
            },
        },
    }
}

fn provenance(sequence: u64, time: i64) -> TradingProvenance {
    TradingProvenance {
        venue_id: "aeris-sim".to_string(),
        provider_id: "fixture".to_string(),
        session_generation: 1,
        source_sequence: sequence,
        observed_unix_nanos: time,
    }
}

fn market_order(client_id: &str, side: OrderSide, sequence: u64, time: i64) -> PlaceOrder {
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

fn observation(bid: i64, ask: i64, sequence: u64, time: i64) -> SimulatedMarketObservation {
    SimulatedMarketObservation {
        instrument_id: instrument().instrument_id,
        bid: FixedPoint::try_new(bid, 2).expect("bid"),
        ask: FixedPoint::try_new(ask, 2).expect("ask"),
        provenance: provenance(sequence, time),
    }
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    assert_eq!(
        complete.managed_brackets[0].status,
        ManagedBracketStatus::Completed
    );
    assert!(complete.orders.iter().all(|order| !order.status.is_open()));
    assert_eq!(complete.positions[0].net_quantity.units(), 0);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.managed_brackets, complete.managed_brackets);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn protective_orders_reduce_locked_positions_without_reopening_risk() {
    let directory = TestDirectory::new("protective-order");
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    assert!(
        service
            .place_order(market_order("locked-entry", OrderSide::Buy, 4, 4_000))
            .expect_err("risk-increasing order remains locked")
            .contains("risk-locked")
    );
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
    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
fn intraday_trailing_drawdown_tracks_peak_on_every_market_observation() {
    let directory = TestDirectory::new("intraday-drawdown");
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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
            .all(|event| event
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("RISK WARNING")))
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
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn bracket_is_blocked_when_its_stop_would_reach_a_loss_limit() {
    let directory = TestDirectory::new("bracket-stop-risk");
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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
            .iter()
            .any(|warning| warning.contains("fast re-entry"))
    );
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");
}

#[test]
fn simulated_execution_and_records_survive_restart_and_export() {
    let directory = TestDirectory::new("restart");
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    assert_eq!(open_snapshot.orders[0].status, OrderStatus::Filled);
    assert_eq!(
        open_snapshot.orders[0].filled_quantity,
        open_snapshot.orders[0].quantity
    );
    assert_eq!(
        open_snapshot.positions[0].unrealized_pnl,
        FixedPoint::try_new(-1_250, 2).expect("unrealized pnl")
    );
    assert_eq!(
        open_snapshot.position_pnl[0].unrealized_ticks,
        Some(FixedPoint::try_new(-1, 0).expect("unrealized ticks"))
    );
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

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.orders.len(), 2);
    assert!(
        restored
            .orders
            .iter()
            .all(|order| order.filled_quantity == order.quantity)
    );
    assert_eq!(restored.fills.len(), 2);
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

#[test]
fn simulated_account_bootstrap_exposes_distinct_multi_account_targets() {
    let directory = TestDirectory::new("simulated-accounts");
    let service = TradingService::start(config(&directory)).expect("service starts");
    let snapshot = service.snapshot().expect("snapshot");

    assert_eq!(snapshot.accounts.len(), 3);
    assert!(snapshot.accounts.iter().all(|account| {
        account.environment == AccountEnvironment::Simulated
            && account.display_name.to_ascii_uppercase().contains("SIM")
    }));
    assert_eq!(
        snapshot
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );

    service
        .shutdown(std::time::Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn risk_profile_cancel_and_lock_state_are_authoritative_and_restart_safe() {
    let directory = TestDirectory::new("risk");
    let service = TradingService::start(config(&directory)).expect("service starts");
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

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    assert!(
        restarted
            .snapshot()
            .expect("snapshot")
            .risk_locks
            .is_empty()
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

#[test]
fn news_time_restriction_blocks_and_persists_a_hard_lock() {
    let directory = TestDirectory::new("news-restriction");
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    assert_eq!(fills.len(), 1);
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.positions[0].net_quantity.units(), 0);
    assert_eq!(snapshot.fills.len(), 2);
    assert_eq!(snapshot.orders.len(), 2);
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.positions[0].net_quantity.units(), 0);
    assert_eq!(restored.fills.len(), 2);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("restarted service stops");
}

#[test]
fn economic_event_lock_is_profile_driven_durable_and_idempotent() {
    let directory = TestDirectory::new("economic-event-lock");
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let secondary = TradingAccountId::try_new("aeris-sim-2").expect("secondary account");
    service
        .register_account(TradingAccount {
            id: secondary.clone(),
            display_name: "Secondary SIM".to_string(),
            environment: AccountEnvironment::Simulated,
            currency: "USD".to_string(),
            currency_scale: 2,
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
fn invalid_json_never_enters_the_store() {
    let directory = TestDirectory::new("invalid-json");
    let service = TradingService::start(config(&directory)).expect("service starts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    for id in ["aeris-sim-2", "aeris-sim-3"] {
        service
            .register_account(TradingAccount {
                id: TradingAccountId::try_new(id).expect("account id"),
                display_name: format!("SIM {id}"),
                environment: AccountEnvironment::Simulated,
                currency: "USD".to_string(),
                currency_scale: 2,
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

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    let service = TradingService::start(config(&directory)).expect("service starts");
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

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
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
    };
    let service = TradingService::start(measured_config.clone()).expect("service starts");
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
    let reopen_started = std::time::Instant::now();
    let reopened = TradingService::start(measured_config).expect("service reopens");
    let snapshot = reopened.snapshot().expect("snapshot reloads");
    let reopen_elapsed = reopen_started.elapsed();
    assert_eq!(
        snapshot.fills.len(),
        usize::try_from(EXECUTIONS).expect("count")
    );
    println!(
        "D3 sqlite measurement: {EXECUTIONS} executions + {USER_RECORDS} user records in {write_elapsed:?}; reopen {reopen_elapsed:?}; database {database_bytes} bytes"
    );
    reopened
        .shutdown(Duration::from_secs(10))
        .expect("reopened service stops");
}

#[test]
fn published_observations_fill_resting_orders_once_and_never_regress() {
    let directory = TestDirectory::new("published-observations");
    let service = TradingService::start(config(&directory)).expect("service");
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut buy_limit = market_order("resting-buy", OrderSide::Buy, 1, 1_000);
    buy_limit.order_type = OrderType::Limit;
    buy_limit.limit_price = Some(FixedPoint::try_new(500_000, 2).expect("limit"));
    service.place_order(buy_limit).expect("resting limit");

    service
        .publish_market_observation(observation(500_025, 500_050, 3, 2_000))
        .expect("newer quote");
    // An older provider revision that would cross the limit must not execute.
    service
        .publish_market_observation(observation(499_950, 499_975, 2, 3_000))
        .expect("stale quote is offered");
    let snapshot = service.snapshot().expect("snapshot");
    assert!(snapshot.fills.is_empty());
    assert_eq!(snapshot.market_observation_error, None);

    service
        .publish_market_observation(observation(499_950, 499_975, 4, 4_000))
        .expect("crossing quote");
    // Another pane republishing the same revision is applied once.
    service
        .publish_market_observation(observation(499_950, 499_975, 4, 4_000))
        .expect("duplicate quote");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.fills[0].price.units(), 499_975);
    assert_eq!(snapshot.position_pnl.len(), 1);
    assert_eq!(snapshot.position_pnl[0].position.net_quantity.units(), 1);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn published_observations_for_unregistered_instruments_are_ignored() {
    let directory = TestDirectory::new("unregistered-observation");
    let service = TradingService::start(config(&directory)).expect("service");
    service
        .publish_market_observation(observation(500_000, 500_025, 1, 1_000))
        .expect("offered");
    assert_eq!(
        service
            .snapshot()
            .expect("snapshot")
            .market_observation_error,
        None
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn cancelling_a_protective_stop_leaves_it_cancelled() {
    let directory = TestDirectory::new("protective-cancel");
    let service = TradingService::start(config(&directory)).expect("service");
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
