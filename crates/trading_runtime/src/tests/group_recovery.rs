//! Recovered groups are verified at startup: a group missing a member is cancelled with a
//! typed notice, a member that filled before the shutdown settles, a coverage mismatch is
//! reported before the loaded-state alignment repairs it, and a verified store reopens
//! without further changes or notices. Migrated standalone protection groups retire when
//! their order fills or is cancelled.

use super::*;
use rusqlite::Connection;
use std::time::Duration;

/// Places a two-unit bracket whose market entry fills on the next quote, leaving an active
/// group with a full-size stop and two 50/50 targets.
fn start_active_bracket(directory: &TestDirectory, entry_client: &str) -> TradingService {
    let service = start_service(directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order(entry_client, OrderSide::Buy, 1, 1_000);
    entry.quantity = quantity(2);
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: fixed_stop_bracket_template(),
        })
        .expect("bracket placed");
    let fills = service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    assert_eq!(fills.len(), 1, "the market entry fills on the quote");
    let snapshot = service.snapshot().expect("active snapshot");
    assert_eq!(
        snapshot.recovery_notices,
        [],
        "a clean placement needs no repair"
    );
    assert_eq!(snapshot.positions[0].net_quantity, quantity(2));
    assert_eq!(
        snapshot
            .orders
            .iter()
            .filter(|order| order.status.is_open())
            .count(),
        3,
        "the stop and both targets work"
    );
    service
}

/// Applies fixture surgery the store's own foreign-key checks would forbid, such as a leg
/// row whose order was torn away.
fn corrupt(directory: &TestDirectory, sql: &str) {
    let connection = Connection::open(directory.0.join("trading.sqlite3")).expect("fixture");
    connection
        .execute_batch(&format!("PRAGMA foreign_keys=OFF; {sql}"))
        .expect("corruption applies");
}

fn group_status(directory: &TestDirectory, group_id: &str) -> String {
    let connection = Connection::open(directory.0.join("trading.sqlite3")).expect("reader");
    connection
        .query_row(
            "SELECT status FROM order_groups WHERE group_id=?1",
            [group_id],
            |row| row.get(0),
        )
        .expect("group row")
}

fn protection_group_status(directory: &TestDirectory) -> String {
    let connection = Connection::open(directory.0.join("trading.sqlite3")).expect("reader");
    connection
        .query_row(
            "SELECT status FROM order_groups WHERE kind='protection'",
            [],
            |row| row.get(0),
        )
        .expect("protection group row")
}

/// Reopening the repaired store changes nothing and raises no further notice.
fn assert_stable_reopen(directory: &TestDirectory, repaired: &TradingSnapshot) {
    let restarted = start_service(directory);
    let snapshot = restarted.snapshot().expect("restart snapshot");
    assert_eq!(
        snapshot.recovery_notices,
        [],
        "a verified store reopens without notices"
    );
    assert_eq!(
        snapshot,
        TradingSnapshot {
            recovery_notices: Vec::new(),
            ..repaired.clone()
        },
        "a verified store reopens unchanged"
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn recovered_group_missing_a_leg_is_cancelled_once_with_a_notice() {
    let directory = TestDirectory::new("group-recovery-missing-leg");
    let service = start_active_bracket(&directory, "recover-entry");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    corrupt(
        &directory,
        "DELETE FROM orders WHERE id = (SELECT order_id FROM order_group_legs
         WHERE group_id='recover-entry' AND role='stop_loss')",
    );

    let repaired = start_service(&directory);
    let snapshot = repaired.snapshot().expect("repaired snapshot");
    assert_eq!(
        snapshot.recovery_notices.len(),
        1,
        "the broken group raises exactly one notice"
    );
    let notice = &snapshot.recovery_notices[0];
    assert_eq!(notice.kind, RecoveryNoticeKind::BrokenGroup);
    assert_eq!(notice.subject, "recover-entry");
    assert_eq!(
        group_status(&directory, "recover-entry"),
        "cancelled",
        "the broken group is cancelled"
    );
    let entry_client = ClientOrderId::try_new("recover-entry").expect("client id");
    for role in ["target", "stop"] {
        for index in [0, 1] {
            let Ok(client) = managed_child_client_order_id(&entry_client, role, index) else {
                continue;
            };
            let Some(order) = snapshot
                .orders
                .iter()
                .find(|order| order.client_order_id == client)
            else {
                continue;
            };
            assert_eq!(
                order.status,
                OrderStatus::Cancelled,
                "the broken group's remaining leg {client:?} is cancelled"
            );
        }
    }
    let entry = snapshot
        .orders
        .iter()
        .find(|order| order.client_order_id == entry_client)
        .expect("entry order");
    assert_eq!(
        entry.status,
        OrderStatus::Filled,
        "the filled entry is untouched"
    );
    repaired.shutdown(Duration::from_secs(2)).expect("shutdown");
    assert_stable_reopen(&directory, &snapshot);
}

#[test]
fn recovered_coverage_mismatch_is_reported_and_realigned_once() {
    let directory = TestDirectory::new("group-recovery-coverage");
    let service = start_active_bracket(&directory, "coverage-entry");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    corrupt(
        &directory,
        "UPDATE orders SET quantity_units=1 WHERE id = (SELECT order_id FROM order_group_legs
         WHERE group_id='coverage-entry' AND role='stop_loss')",
    );

    let repaired = start_service(&directory);
    let snapshot = repaired.snapshot().expect("repaired snapshot");
    assert_eq!(
        snapshot.recovery_notices.len(),
        1,
        "the coverage repair raises exactly one notice"
    );
    let notice = &snapshot.recovery_notices[0];
    assert_eq!(notice.kind, RecoveryNoticeKind::CoverageRepair);
    assert_eq!(notice.subject, "coverage-entry");
    assert_eq!(
        group_status(&directory, "coverage-entry"),
        "active",
        "a realigned group keeps protecting the position"
    );
    let entry_client = ClientOrderId::try_new("coverage-entry").expect("client id");
    let stop_client = managed_child_client_order_id(&entry_client, "stop", 0).expect("stop id");
    let stop = snapshot
        .orders
        .iter()
        .find(|order| order.client_order_id == stop_client)
        .expect("stop order");
    assert_eq!(
        (stop.status, stop.quantity),
        (OrderStatus::Working, quantity(2)),
        "the alignment grows the stop back to the position"
    );
    assert_leg_event(
        &snapshot,
        &stop.id,
        OrderEventKind::Resized,
        aeris_trading::OrderEventReason::CoverageGrow,
    );
    repaired.shutdown(Duration::from_secs(2)).expect("shutdown");
    assert_stable_reopen(&directory, &snapshot);
}

#[test]
fn recovered_group_with_a_filled_stop_settles_once_with_a_notice() {
    let directory = TestDirectory::new("group-recovery-settle");
    let service = start_active_bracket(&directory, "settle-entry");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    corrupt(
        &directory,
        "UPDATE orders SET status='filled', filled_units=quantity_units,
            terminal_unix_nanos=3000, updated_unix_nanos=3000
         WHERE id = (SELECT order_id FROM order_group_legs
         WHERE group_id='settle-entry' AND role='stop_loss')",
    );

    let repaired = start_service(&directory);
    let snapshot = repaired.snapshot().expect("repaired snapshot");
    assert_eq!(
        snapshot.recovery_notices.len(),
        1,
        "the interrupted settle raises exactly one notice"
    );
    let notice = &snapshot.recovery_notices[0];
    assert_eq!(notice.kind, RecoveryNoticeKind::InterruptedGroupFill);
    assert_eq!(notice.subject, "settle-entry");
    assert_eq!(
        group_status(&directory, "settle-entry"),
        "completed",
        "the stopped-out group completes"
    );
    let entry_client = ClientOrderId::try_new("settle-entry").expect("client id");
    for index in [0, 1] {
        let client =
            managed_child_client_order_id(&entry_client, "target", index).expect("target id");
        let target = snapshot
            .orders
            .iter()
            .find(|order| order.client_order_id == client)
            .expect("target order");
        assert_eq!(
            target.status,
            OrderStatus::Cancelled,
            "the settled group's target is cancelled"
        );
    }
    repaired.shutdown(Duration::from_secs(2)).expect("shutdown");
    assert_stable_reopen(&directory, &snapshot);
}

/// A standalone protective order placed before the group model becomes a protection group at
/// the next startup; the group retires when the order fills or is cancelled.
fn start_migrated_protection(label: &str) -> (TestDirectory, TradingService) {
    let directory = TestDirectory::new(label);
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    let mut entry = market_order("protect-entry", OrderSide::Buy, 1, 1_000);
    entry.quantity = quantity(2);
    service.place_order(entry).expect("entry accepted");
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry fills");
    let mut protective = market_order("protect-stop", OrderSide::Sell, 3, 3_000);
    protective.order_type = OrderType::Stop;
    protective.time_in_force = TimeInForce::GoodTillCancelled;
    protective.quantity = quantity(2);
    protective.stop_price = Some(FixedPoint::try_new(9_800, 2).expect("stop"));
    service
        .place_protective_order(PlaceProtectiveOrder {
            order: protective,
            role: ProtectiveOrderRole::StopLoss,
        })
        .expect("protective stop placed");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let migrated = start_service(&directory);
    let snapshot = migrated.snapshot().expect("migrated snapshot");
    assert_eq!(
        snapshot.recovery_notices,
        [],
        "a sound migrated group raises no notice"
    );
    assert_eq!(
        protection_group_status(&directory),
        "active",
        "the legacy protective order projects as an active group"
    );
    (directory, migrated)
}

#[test]
fn migrated_protection_group_completes_when_its_order_fills() {
    let (directory, service) = start_migrated_protection("group-recovery-protection-fill");
    let fills = service
        .observe_market(observation(9_750, 9_775, 4, 4_000))
        .expect("stop fills");
    assert_eq!(fills.len(), 1, "the protective stop fills at the touch");
    let snapshot = service.snapshot().expect("flat snapshot");
    assert_eq!(snapshot.positions[0].net_quantity.units(), 0);
    assert_eq!(
        protection_group_status(&directory),
        "completed",
        "the protection group retires with its fill"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    assert_stable_reopen(&directory, &snapshot);
}

#[test]
fn migrated_protection_group_cancels_with_its_order() {
    let (directory, service) = start_migrated_protection("group-recovery-protection-cancel");
    service
        .cancel_order(ClientOrderId::try_new("protect-stop").expect("client id"))
        .expect("protective cancel");
    assert_eq!(
        protection_group_status(&directory),
        "cancelled",
        "the protection group retires with the cancelled order"
    );
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.recovery_notices, []);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    assert_stable_reopen(&directory, &snapshot);
}
