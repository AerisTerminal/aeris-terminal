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
    assert_eq!(
        service.snapshot().expect("snapshot").positions[0].unrealized_pnl,
        FixedPoint::try_new(-1_250, 2).expect("unrealized pnl")
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
    service
        .export(directory.0.join("export"))
        .expect("export succeeds");
    service
        .shutdown(Duration::from_secs(2))
        .expect("service stops");

    let restarted = TradingService::start(config(&directory)).expect("service restarts");
    let restored = restarted.snapshot().expect("restored snapshot");
    assert_eq!(restored.orders.len(), 2);
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
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("loss limit"),
            trailing_drawdown: Some(FixedPoint::try_new(200_000, 2).expect("drawdown")),
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(1, 0).expect("contracts"),
            consistency_max_single_trade_percent: Some(50),
            restricted_until_unix_nanos: None,
            enabled: true,
        })
        .expect("risk profile stores");

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
