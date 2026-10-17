//! V19 awaiting brackets convert to group authority at startup: a priceable row gains real
//! Inactive legs at exactly the prices the legacy planned-leg projection showed, an
//! unpriceable row becomes a cancelled group with a recovery notice, and a failed conversion
//! rolls back for the next start.

use super::*;
use crate::clock::ManualClock;
use crate::store::TradingStore;
use rusqlite::Connection;
use std::sync::{Arc, Weak, atomic::AtomicBool, mpsc};
use std::time::Duration;

fn contract_json(tick: &serde_json::Value, grid: &serde_json::Value) -> String {
    serde_json::json!({
        "tick_size": tick,
        "point_value": {"units": 5_000, "scale": 2},
        "price_grid": grid,
        "instrument_class": "future",
        "order_quantity_increment": {"units": 1, "scale": 0},
        "currency": "USD",
        "expiry": null,
        "first_notice": null,
        "last_trade": null,
        "session_hours": [],
        "provenance": {
            "provider_id": "fixture",
            "provider_symbol": "ESZ6",
            "display_symbol": "ESZ6",
            "session_generation": 1
        }
    })
    .to_string()
}

fn template_json(id: &str, stop_ticks: u32, target_ticks: u32) -> String {
    serde_json::json!({
        "template_id": id,
        "revision": 3,
        "name": "Legacy bracket",
        "stop_offset_ticks": stop_ticks,
        "targets": [{"offset_ticks": target_ticks, "quantity_percent": 100}],
        "trailing_stop": null,
        "break_even": null,
        "enabled": true
    })
    .to_string()
}

/// The full V19 fixture of VAL-TRD-049: an awaiting-entry bracket, an unpriceable awaiting
/// bracket, an active bracket, a completed bracket, a cancelled bracket and a standalone
/// protective order.
fn write_v19_bracket_fixture(path: &std::path::Path, only_awaiting: bool) {
    let connection = Connection::open(path).expect("fixture database");
    crate::store::create_schema_at(&connection, 19);
    let priced = contract_json(
        &serde_json::json!({"units": 25, "scale": 2}),
        &serde_json::json!({"kind": "fixed", "tick": {"units": 25, "scale": 2}}),
    );
    let unpriced = contract_json(
        &serde_json::Value::Null,
        &serde_json::json!({"kind": "unknown"}),
    );
    let provenance = r#"{"venue_id":"aeris-sim","provider_id":"fixture","session_generation":1,"source_sequence":1,"observed_unix_nanos":1}"#;
    let batch = format!(
        "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
         VALUES ('sim','SIM • Legacy','simulated','USD',2,'aeris-sim');
         INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
         VALUES ('BTC-USDC',2,0,'{priced}');
         INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
             time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
             limit_units,limit_scale,status,submitted_unix_nanos,provenance_json)
         VALUES ('ord-awaiting','awaiting-entry','sim','BTC-USDC','buy','limit','gtc',
                 2,0,0,0,10000,2,'working',1,'{provenance}');
         INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
             stop_client_order_id,target_client_order_ids_json,status)
         VALUES ('awaiting-entry','{}','awaiting-entry',NULL,'[]','awaiting_entry');",
        template_json("legacy-awaiting", 8, 8),
    );
    let mut batches = vec![batch];
    if !only_awaiting {
        batches.push(format!(
            "INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('NOGRID',2,0,'{unpriced}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 limit_units,limit_scale,status,submitted_unix_nanos,provenance_json)
             VALUES ('ord-nogrid','nogrid-entry','sim','NOGRID','buy','limit','gtc',
                     1,0,0,0,500,2,'working',1,'{provenance}');
             INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
                 stop_client_order_id,target_client_order_ids_json,status)
             VALUES ('nogrid-entry','{}','nogrid-entry',NULL,'[]','awaiting_entry');",
            template_json("legacy-nogrid", 4, 4),
        ));
        batches.push(format!(
            "INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 limit_units,limit_scale,stop_units,stop_scale,status,submitted_unix_nanos,
                 provenance_json)
             VALUES ('ord-active-entry','active-entry','sim','BTC-USDC','buy','limit','gtc',
                     2,0,2,0,10000,2,NULL,NULL,'filled',1,'{provenance}'),
                    ('ord-active-stop','active-stop','sim','BTC-USDC','sell','stop','gtc',
                     2,0,0,0,NULL,NULL,9800,2,'working',2,'{provenance}'),
                    ('ord-active-target','active-target','sim','BTC-USDC','sell','limit','gtc',
                     2,0,0,0,10200,2,NULL,NULL,'working',2,'{provenance}'),
                    ('ord-done-entry','done-entry','sim','BTC-USDC','buy','limit','gtc',
                     1,0,1,0,10000,2,NULL,NULL,'filled',1,'{provenance}'),
                    ('ord-done-stop','done-stop','sim','BTC-USDC','sell','stop','gtc',
                     1,0,1,0,NULL,NULL,9800,2,'filled',2,'{provenance}'),
                    ('ord-dead-entry','dead-entry','sim','BTC-USDC','buy','limit','gtc',
                     1,0,0,0,10000,2,NULL,NULL,'cancelled',1,'{provenance}'),
                    ('ord-protect','protect-client','sim','BTC-USDC','sell','stop','gtc',
                     1,0,0,0,NULL,NULL,9900,2,'working',1,'{provenance}');
             INSERT INTO protective_orders(client_order_id,role)
             VALUES ('protect-client','stop_loss');
             INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
                 stop_client_order_id,target_client_order_ids_json,status,
                 entry_price_units,entry_price_scale)
             VALUES ('active-entry','{}','active-entry','active-stop','[\"active-target\"]','active',
                     10000,2);
             INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
                 stop_client_order_id,target_client_order_ids_json,status)
             VALUES ('done-entry','{}','done-entry','done-stop','[]','completed'),
                    ('dead-entry','{}','dead-entry',NULL,'[]','cancelled');",
            template_json("legacy-active", 8, 8),
            template_json("legacy-done", 8, 8),
            template_json("legacy-dead", 8, 8),
        ));
    }
    connection
        .execute_batch(&batches.concat())
        .expect("fixture rows");
}

fn rows(connection: &Connection, sql: &str) -> Vec<(String, String)> {
    connection
        .prepare(sql)
        .expect("query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("row values")
}

/// Asserts the converted awaiting bracket's held stop and target in the snapshot, at exactly
/// the prices the legacy planned-leg projection showed (stop 100.00 - 8 x 0.25, target
/// 100.00 + 8 x 0.25), and that the legacy reader views keep serving the desktop.
fn assert_converted_held_legs(snapshot: &TradingSnapshot) {
    let entry_client = ClientOrderId::try_new("awaiting-entry").expect("client id");
    let stop_client = managed_child_client_order_id(&entry_client, "stop", 0).expect("stop id");
    let target_client =
        managed_child_client_order_id(&entry_client, "target", 0).expect("target id");
    let leg = |client: &ClientOrderId| {
        snapshot
            .orders
            .iter()
            .find(|order| order.client_order_id == *client)
            .unwrap_or_else(|| panic!("held leg {client:?} is an order"))
    };
    let stop = leg(&stop_client);
    assert_eq!(
        (
            stop.status,
            stop.side,
            stop.order_type,
            stop.stop_price,
            stop.quantity
        ),
        (
            OrderStatus::Inactive,
            OrderSide::Sell,
            OrderType::Stop,
            FixedPoint::try_new(9_800, 2).ok(),
            FixedPoint::try_new(2, 0).expect("quantity")
        ),
        "the held stop keeps the legacy planned price and full entry quantity"
    );
    let target = leg(&target_client);
    assert_eq!(
        (
            target.status,
            target.side,
            target.order_type,
            target.limit_price,
            target.quantity
        ),
        (
            OrderStatus::Inactive,
            OrderSide::Sell,
            OrderType::Limit,
            FixedPoint::try_new(10_200, 2).ok(),
            FixedPoint::try_new(2, 0).expect("quantity")
        ),
        "the held target keeps the legacy planned price and allocated quantity"
    );
    let mut planned = snapshot
        .planned_bracket_legs
        .iter()
        .map(|leg| leg.client_order_id.as_str().to_string())
        .collect::<Vec<_>>();
    planned.sort_unstable();
    let mut expected = [
        stop_client.as_str().to_string(),
        target_client.as_str().to_string(),
    ];
    expected.sort_unstable();
    assert_eq!(planned, expected, "held legs project as planned legs");
    assert!(
        snapshot.managed_brackets.iter().any(|bracket| {
            bracket.bracket_id == "awaiting-entry"
                && bracket.status == ManagedBracketStatus::AwaitingEntry
        }),
        "the converted bracket keeps its legacy reader view"
    );
    assert!(
        snapshot.managed_brackets.iter().any(|bracket| {
            bracket.bracket_id == "active-entry" && bracket.status == ManagedBracketStatus::Active
        }),
        "the active legacy bracket stays legacy-owned"
    );
}

/// Asserts the per-row group, legacy-table and leg-offset facts persisted in the migrated
/// database.
fn assert_migrated_rows(path: &std::path::Path) {
    let connection = Connection::open(path).expect("migrated database");
    let mut groups = rows(
        &connection,
        "SELECT group_id,status FROM order_groups ORDER BY group_id",
    );
    groups.sort();
    assert_eq!(
        groups,
        [
            ("active-entry".to_string(), "active".to_string()),
            ("awaiting-entry".to_string(), "pending".to_string()),
            ("dead-entry".to_string(), "cancelled".to_string()),
            ("done-entry".to_string(), "completed".to_string()),
            ("nogrid-entry".to_string(), "cancelled".to_string()),
            ("protection/ord-protect".to_string(), "active".to_string()),
        ],
        "every fixture row becomes exactly one group"
    );
    let mut legacy = rows(
        &connection,
        "SELECT bracket_id,status FROM managed_brackets ORDER BY bracket_id",
    );
    legacy.sort();
    assert_eq!(
        legacy,
        [
            ("active-entry".to_string(), "active".to_string()),
            ("dead-entry".to_string(), "cancelled".to_string()),
            ("done-entry".to_string(), "completed".to_string()),
        ],
        "only the converted awaiting rows leave the legacy table"
    );
    assert_eq!(
        rows(
            &connection,
            "SELECT role,CAST(offset_ticks AS TEXT) FROM order_group_legs
             WHERE group_id='awaiting-entry' ORDER BY role DESC",
        ),
        [
            ("take_profit".to_string(), "8".to_string()),
            ("stop_loss".to_string(), "8".to_string()),
        ],
        "converted legs keep their template offsets"
    );
}

#[test]
fn v19_awaiting_brackets_convert_exactly_at_startup() {
    let directory = TestDirectory::new("v19-bracket-conversion");
    let path = directory.0.join("trading.sqlite3");
    write_v19_bracket_fixture(&path, false);
    let service = start_service(&directory);
    let snapshot = service.snapshot().expect("snapshot");
    assert_converted_held_legs(&snapshot);

    // The unpriceable bracket becomes a cancelled group; its entry keeps working unmanaged.
    assert_eq!(
        snapshot.recovery_notices,
        [RecoveryNotice {
            kind: RecoveryNoticeKind::UnpriceableBracket,
            subject: "nogrid-entry".to_string(),
            detail: snapshot.recovery_notices[0].detail.clone(),
        }],
        "the unpriceable conversion raises one typed notice"
    );
    assert!(
        snapshot.recovery_notices[0]
            .detail
            .contains("could not be priced"),
        "the notice explains the repair"
    );
    let nogrid_entry = snapshot
        .orders
        .iter()
        .find(|order| order.client_order_id.as_str() == "nogrid-entry")
        .expect("unpriceable entry");
    assert_eq!(
        nogrid_entry.status,
        OrderStatus::Working,
        "an unpriceable bracket never changes its entry"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");

    assert_migrated_rows(&path);

    // Reopening the migrated database changes nothing and raises no further notice: the
    // notice belongs to the start that performed the repair.
    let restarted = start_service(&directory);
    let restart_snapshot = restarted.snapshot().expect("restart snapshot");
    assert_eq!(
        restart_snapshot.recovery_notices,
        [],
        "the notice is raised at the repairing start only"
    );
    assert_eq!(
        restart_snapshot,
        TradingSnapshot {
            recovery_notices: Vec::new(),
            ..snapshot
        },
        "reopening the converted database changes nothing"
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn failed_awaiting_conversion_rolls_back_and_retries_on_next_start() {
    let directory = TestDirectory::new("awaiting-conversion-rollback");
    let path = directory.0.join("trading.sqlite3");
    write_v19_bracket_fixture(&path, true);
    let configuration = config(&directory);
    let store = TradingStore::open(&configuration.database_path, configuration.retention)
        .expect("store opens");
    store.crash_during_commit(1);
    let (sender, _receiver) = mpsc::sync_channel(1);
    let opened = TradingCore::open_with_store(
        store,
        configuration.retention,
        Arc::new(ManualClock::new(1_000_000_000_000)),
        configuration.execution,
        Weak::new(),
        Arc::new(MarketFeedInbox::new(
            sender,
            Arc::new(AtomicBool::new(false)),
        )),
    );
    assert!(opened.is_err(), "the injected failure aborts startup");

    let connection = Connection::open(&path).expect("database after failure");
    assert_eq!(
        rows(
            &connection,
            "SELECT bracket_id,status FROM managed_brackets"
        ),
        [("awaiting-entry".to_string(), "awaiting_entry".to_string())],
        "a failed conversion keeps the legacy row"
    );
    assert!(
        rows(&connection, "SELECT group_id,status FROM order_groups").is_empty(),
        "a failed conversion writes no group"
    );
    assert_eq!(
        rows(&connection, "SELECT id,status FROM orders"),
        [("ord-awaiting".to_string(), "working".to_string())],
        "a failed conversion writes no leg order"
    );
    drop(connection);

    let service = start_service(&directory);
    let snapshot = service.snapshot().expect("snapshot after retry");
    assert_eq!(
        snapshot
            .orders
            .iter()
            .filter(|order| order.status == OrderStatus::Inactive)
            .count(),
        2,
        "the retry converts the bracket"
    );
    assert_eq!(snapshot.recovery_notices, []);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}
