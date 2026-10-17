use std::collections::{BTreeMap, BTreeSet};

use super::ledger::{
    load_ledger_checkpoints, load_position_checkpoints, upsert_ledger_checkpoint,
    upsert_position_checkpoint,
};
use super::{
    Fill, InstrumentId, StepTx, TradingAccountId, TradingStore, database_error, fixed, params,
    usize_to_i64,
};
use crate::FixedPoint;
use crate::core::ledger::{LedgerCheckpoint, PositionCheckpoint};

/// Fills of an open order or a leg of a live order group are never pruned.
const GROUP_PIN_ORDER_ID: &str = "order_id NOT IN (
    SELECT order_id FROM order_group_legs WHERE group_id IN (
        SELECT id FROM order_groups WHERE status IN ('pending', 'active')
    )
 )";

/// The same live-group protection for the orders table itself.
const GROUP_PIN_ID: &str = "id NOT IN (
    SELECT order_id FROM order_group_legs WHERE group_id IN (
        SELECT id FROM order_groups WHERE status IN ('pending', 'active')
    )
 )";

/// Replays the fills retention is about to prune onto the previous checkpoint so recovery can
/// rebuild the exact projection from checkpoint plus the retained suffix. Returning `None`
/// keeps the fills: a position whose account or instrument is gone cannot be checkpointed.
pub(crate) type CheckpointFold<'a> = dyn FnMut(
        &(TradingAccountId, InstrumentId),
        Option<&PositionCheckpoint>,
        &[Fill],
    ) -> Result<Option<PositionCheckpoint>, String>
    + 'a;

fn delete_old_user_records(
    transaction: &rusqlite::Transaction<'_>,
    maximum_per_kind: usize,
) -> Result<(), String> {
    let mut deleted = 0_usize;
    for kind in [
        "journal_entry",
        "tag",
        "note",
        "screenshot",
        "rule_profile",
        "rule_evaluation",
        "session_plan",
        "analytics_cache",
    ] {
        deleted += transaction
            .execute(
                "DELETE FROM user_records WHERE kind = ?1 AND id IN (
                    SELECT id FROM user_records WHERE kind = ?1
                    ORDER BY updated_unix_nanos DESC LIMIT -1 OFFSET ?2
                 ) AND rowid IN (
                    SELECT rowid FROM user_records WHERE kind = ?1
                    ORDER BY updated_unix_nanos LIMIT ?3
                 )",
                params![
                    kind,
                    usize_to_i64(maximum_per_kind)?,
                    usize_to_i64(5_000_usize.saturating_sub(deleted))?
                ],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

impl TradingStore {
    pub(crate) fn delete_practice_account(
        &mut self,
        account_id: &TradingAccountId,
    ) -> Result<(), String> {
        let checkpoint_tables = self.checkpoint_tables_present()?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        if checkpoint_tables {
            for statement in [
                "DELETE FROM equity_samples WHERE account_id = ?1",
                "DELETE FROM position_checkpoints WHERE account_id = ?1",
                "DELETE FROM ledger_checkpoints WHERE account_id = ?1",
            ] {
                transaction
                    .execute(statement, [account_id.as_str()])
                    .map_err(database_error)?;
            }
        }
        for statement in [
            "DELETE FROM order_groups WHERE account_id = ?1",
            "DELETE FROM trade_copier_targets WHERE source_account_id = ?1 OR target_account_id = ?1",
            "DELETE FROM economic_event_risk_actions WHERE account_id = ?1",
            "DELETE FROM discipline_states WHERE account_id = ?1",
            "DELETE FROM session_plans WHERE account_id = ?1",
            "DELETE FROM trade_cycles WHERE account_id = ?1",
            "DELETE FROM trade_pnl_cycles WHERE account_id = ?1",
            "DELETE FROM risk_trade_cycles WHERE account_id = ?1",
            "DELETE FROM risk_rule_states WHERE account_id = ?1",
            "DELETE FROM risk_locks WHERE account_id = ?1",
            "DELETE FROM risk_profiles WHERE account_id = ?1",
            "DELETE FROM order_events WHERE order_id IN (
                SELECT id FROM orders WHERE account_id = ?1
             )",
            "DELETE FROM fill_accounting WHERE account_id = ?1",
            "DELETE FROM ledger_entries WHERE account_id = ?1",
            "DELETE FROM fee_schedules WHERE account_id = ?1",
            "DELETE FROM fills WHERE account_id = ?1",
            "DELETE FROM positions WHERE account_id = ?1",
            "DELETE FROM orders WHERE account_id = ?1",
            "DELETE FROM trade_copiers WHERE source_account_id = ?1",
        ] {
            transaction
                .execute(statement, [account_id.as_str()])
                .map_err(database_error)?;
        }
        transaction
            .execute(
                "DELETE FROM trade_copiers
                 WHERE NOT EXISTS (
                    SELECT 1 FROM trade_copier_targets
                    WHERE trade_copier_targets.source_account_id = trade_copiers.source_account_id
                 )",
                [],
            )
            .map_err(database_error)?;
        let deleted = transaction
            .execute("DELETE FROM accounts WHERE id = ?1", [account_id.as_str()])
            .map_err(database_error)?;
        if deleted != 1 {
            return Err("practice account disappeared before deletion".to_string());
        }
        StepTx::commit_revision(transaction).map_err(database_error)
    }
}

/// The oldest prunable fills, excluding every fill an open order or live group still
/// references, capped so one run deletes at most 5,000 fills.
fn candidate_fill_ids(
    transaction: &rusqlite::Transaction<'_>,
    maximum_fills: usize,
    retained_closed_orders: i64,
) -> Result<BTreeSet<String>, String> {
    let mut statement = transaction
        .prepare(
            "SELECT id FROM fills WHERE order_id NOT IN (
                SELECT id FROM orders WHERE status IN ('pending', 'working',
                    'pending_modify', 'partially_filled', 'pending_cancel')
             ) AND order_id NOT IN (
                SELECT order_id FROM order_group_legs WHERE group_id IN (
                    SELECT id FROM order_groups WHERE status IN ('pending', 'active')
                )
             ) AND (
                id IN (
                    SELECT id FROM fills ORDER BY execution_unix_nanos DESC LIMIT -1 OFFSET ?1
                ) OR order_id IN (
                    SELECT id FROM orders WHERE status NOT IN ('pending', 'working',
                        'pending_modify', 'partially_filled', 'pending_cancel')
                    ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?2
                )
             ) ORDER BY execution_unix_nanos, rowid LIMIT 5000",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(
            params![usize_to_i64(maximum_fills)?, retained_closed_orders],
            |row| row.get::<_, String>(0),
        )
        .map_err(database_error)?;
    rows.collect::<rusqlite::Result<BTreeSet<_>>>()
        .map_err(database_error)
}

/// Folds the deleted fill prefix of each affected instrument into its position checkpoint,
/// then deletes the fills, their accounting and their ledger entries. Every key's delete set
/// is a prefix of its fills in replay order, so checkpoint plus the retained suffix
/// reproduces the pre-retention projection exactly. All of it commits in the caller's one
/// transaction: a crash leaves neither checkpoint nor deletion behind.
fn checkpoint_and_prune_fills(
    transaction: &rusqlite::Transaction<'_>,
    maximum_fills: usize,
    retained_closed_orders: i64,
    fold: &mut CheckpointFold<'_>,
) -> Result<(), String> {
    let candidates = candidate_fill_ids(transaction, maximum_fills, retained_closed_orders)?;
    if candidates.is_empty() {
        return Ok(());
    }
    let mut prefix_open: BTreeMap<(String, String), bool> = BTreeMap::new();
    let mut deleted_ids = Vec::new();
    let mut statement = transaction
        .prepare(
            "SELECT id, account_id, instrument_id FROM fills
             ORDER BY execution_unix_nanos, rowid",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(database_error)?;
    for row in rows {
        let (id, account_id, instrument_id) = row.map_err(database_error)?;
        let open = prefix_open
            .entry((account_id, instrument_id))
            .or_insert(true);
        if *open && candidates.contains(&id) {
            deleted_ids.push(id);
        } else {
            *open = false;
        }
    }
    drop(statement);
    if deleted_ids.is_empty() {
        return Ok(());
    }
    let mut checkpoints = load_position_checkpoints(transaction)?;
    let fills = TradingStore::load_fills_by_ids(transaction, &deleted_ids)?;
    let mut fills_by_key: BTreeMap<(TradingAccountId, InstrumentId), Vec<Fill>> = BTreeMap::new();
    for fill in fills {
        fills_by_key
            .entry((fill.account_id.clone(), fill.instrument_id.clone()))
            .or_default()
            .push(fill);
    }
    let mut kept_ids = BTreeSet::new();
    for (key, key_fills) in fills_by_key {
        let key_ids: Vec<String> = key_fills
            .iter()
            .map(|fill| fill.id.as_str().to_string())
            .collect();
        let base = checkpoints.remove(&key);
        let Some(checkpoint) = fold(&key, base.as_ref(), &key_fills)? else {
            // The position cannot be replayed; its fills stay durable for another attempt.
            kept_ids.extend(key_ids);
            continue;
        };
        upsert_position_checkpoint(transaction, &checkpoint)?;
    }
    if !kept_ids.is_empty() {
        deleted_ids.retain(|id| !kept_ids.contains(id));
        if deleted_ids.is_empty() {
            return Ok(());
        }
    }
    prune_ledger_entries(transaction, &deleted_ids)?;
    let placeholders = (1..=deleted_ids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    transaction
        .execute(
            &format!("DELETE FROM fill_accounting WHERE fill_id IN ({placeholders})"),
            rusqlite::params_from_iter(deleted_ids.iter()),
        )
        .map_err(database_error)?;
    transaction
        .execute(
            &format!("DELETE FROM fills WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(deleted_ids.iter()),
        )
        .map_err(database_error)?;
    Ok(())
}

/// Folds the ledger entries of pruned (or previously orphaned) fills into each account's
/// ledger checkpoint, then deletes at most 5,000 entries. A ledgered account's balance stays
/// the checkpoint plus the exact sum of its remaining entries.
fn prune_ledger_entries(
    transaction: &rusqlite::Transaction<'_>,
    deleted_fill_ids: &[String],
) -> Result<(), String> {
    let placeholders = (1..=deleted_fill_ids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = transaction
        .prepare(&format!(
            "SELECT rowid, account_id, amount_units, amount_scale, at_unix_nanos
             FROM ledger_entries
             WHERE fill_id IN ({placeholders})
                OR (fill_id IS NOT NULL AND fill_id NOT IN (SELECT id FROM fills))
             ORDER BY rowid LIMIT 5000"
        ))
        .map_err(database_error)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(deleted_fill_ids.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, u8>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(database_error)?;
    let mut pruned = Vec::new();
    for row in rows {
        pruned.push(row.map_err(database_error)?);
    }
    drop(statement);
    if pruned.is_empty() {
        return Ok(());
    }
    let mut checkpoints = load_ledger_checkpoints(transaction)?;
    let mut rowids = Vec::with_capacity(pruned.len());
    for (rowid, account, units, scale, at) in pruned {
        rowids.push(rowid);
        let account_id =
            TradingAccountId::try_new(account.as_str()).map_err(|error| error.to_string())?;
        let amount = fixed(units, scale)?;
        let checkpoint = checkpoints
            .entry(account_id.clone())
            .or_insert(LedgerCheckpoint {
                account_id,
                balance: FixedPoint::try_new(0, scale).map_err(|error| error.to_string())?,
                entry_count: 0,
                at_unix_nanos: at,
            });
        checkpoint.balance = checkpoint
            .balance
            .checked_add(
                amount
                    .exact_rescale(checkpoint.balance.scale())
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        checkpoint.entry_count = checkpoint.entry_count.saturating_add(1);
        checkpoint.at_unix_nanos = checkpoint.at_unix_nanos.max(at);
    }
    for checkpoint in checkpoints.values() {
        upsert_ledger_checkpoint(transaction, checkpoint)?;
    }
    let rowid_placeholders = (1..=rowids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    transaction
        .execute(
            &format!("DELETE FROM ledger_entries WHERE rowid IN ({rowid_placeholders})"),
            rusqlite::params_from_iter(rowids.iter()),
        )
        .map_err(database_error)?;
    Ok(())
}

/// The pre-V26 path for a gated store awaiting group conversion: no checkpoint tables exist,
/// so retention keeps the legacy fill pruning until the conversion completes the chain.
fn prune_fills_legacy(
    transaction: &rusqlite::Transaction<'_>,
    maximum_fills: usize,
    retained_closed_orders: i64,
) -> Result<(), String> {
    transaction
        .execute(
            "DELETE FROM fills WHERE id IN (
                SELECT id FROM fills WHERE id IN (
                    SELECT id FROM fills ORDER BY execution_unix_nanos DESC LIMIT -1 OFFSET ?1
                ) OR order_id IN (
                    SELECT id FROM orders WHERE status NOT IN
                        ('pending', 'working', 'pending_modify', 'partially_filled',
                         'pending_cancel')
                    ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?2
                ) ORDER BY execution_unix_nanos LIMIT 5000
             )",
            params![usize_to_i64(maximum_fills)?, retained_closed_orders],
        )
        .map_err(database_error)?;
    Ok(())
}

impl TradingStore {
    pub(crate) fn enforce_retention(
        &mut self,
        fold: &mut CheckpointFold<'_>,
    ) -> Result<(), String> {
        let checkpointed = self.checkpoint_tables_present()?;
        let maximum_fills = self.retention.maximum_fills;
        let maximum_orders = self.retention.maximum_orders;
        let maximum_order_events = self.retention.maximum_order_events;
        let maximum_user_records = self.retention.maximum_user_records_per_kind;
        let transaction = self.connection.transaction().map_err(database_error)?;
        let working_orders: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM orders WHERE status IN
                 ('pending', 'working', 'pending_modify', 'partially_filled', 'pending_cancel')",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        let retained_closed_orders = usize_to_i64(
            maximum_orders.saturating_sub(
                usize::try_from(working_orders)
                    .map_err(|_| "stored working-order count is invalid".to_string())?,
            ),
        )?;
        if checkpointed {
            checkpoint_and_prune_fills(&transaction, maximum_fills, retained_closed_orders, fold)?;
        } else {
            prune_fills_legacy(&transaction, maximum_fills, retained_closed_orders)?;
        }
        let group_pin_events = if checkpointed {
            format!("AND {GROUP_PIN_ORDER_ID}")
        } else {
            String::new()
        };
        transaction
            .execute(
                &format!(
                    "DELETE FROM order_events WHERE id IN (
                        SELECT id FROM order_events WHERE id IN (
                            SELECT id FROM order_events ORDER BY sequence DESC LIMIT -1 OFFSET ?1
                        ) OR order_id IN (
                            SELECT id FROM orders WHERE status NOT IN
                                ('pending', 'working', 'pending_modify', 'partially_filled',
                                 'pending_cancel')
                            ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?2
                        ) {group_pin_events} ORDER BY sequence LIMIT 5000
                     )"
                ),
                params![
                    usize_to_i64(maximum_order_events).unwrap_or(i64::MAX),
                    retained_closed_orders
                ],
            )
            .map_err(database_error)?;
        delete_old_user_records(&transaction, maximum_user_records)?;
        transaction
            .execute(
                "DELETE FROM economic_event_risk_actions WHERE rowid IN (
                    SELECT rowid FROM economic_event_risk_actions
                    ORDER BY applied_unix_nanos DESC LIMIT -1 OFFSET ?1
                ) AND rowid IN (
                    SELECT rowid FROM economic_event_risk_actions
                    ORDER BY applied_unix_nanos LIMIT 5000
                 )",
                [usize_to_i64(maximum_user_records)?],
            )
            .map_err(database_error)?;
        let group_pin_orders = if checkpointed {
            format!("AND {GROUP_PIN_ID}")
        } else {
            String::new()
        };
        transaction
            .execute(
                &format!(
                    "DELETE FROM orders WHERE id IN (
                        SELECT id FROM orders WHERE status NOT IN
                            ('pending', 'working', 'pending_modify', 'partially_filled',
                             'pending_cancel')
                        ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?1
                     ) AND id NOT IN (
                        SELECT order_id FROM fills UNION SELECT order_id FROM order_events
                     ) AND id IN (
                        SELECT id FROM orders WHERE status NOT IN
                            ('pending', 'working', 'pending_modify', 'partially_filled',
                             'pending_cancel')
                        ORDER BY submitted_unix_nanos LIMIT 5000
                     ) {group_pin_orders}"
                ),
                [retained_closed_orders],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_deletes_no_more_than_five_thousand_rows_per_table_per_hour() {
        let retention = super::super::TradingRetention {
            maximum_user_records_per_kind: 1,
            ..Default::default()
        };
        let mut store = TradingStore::open(std::path::Path::new(":memory:"), retention)
            .expect("in-memory store");
        let transaction = store.connection.transaction().expect("seed transaction");
        for index in 0..12_001 {
            transaction
                .execute(
                    "INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                     VALUES ('note', ?1, 1, ?2, '{}')",
                    params![format!("note-{index}"), index],
                )
                .expect("seed record");
        }
        transaction.commit().expect("seed");
        for remaining in [7_001, 2_001, 1] {
            // No fills are seeded, so the checkpoint fold never fires.
            store
                .enforce_retention(&mut |_, _, _| Ok(None))
                .expect("hourly retention");
            let count: i64 = store
                .connection
                .query_row(
                    "SELECT count(*) FROM user_records WHERE kind='note'",
                    [],
                    |row| row.get(0),
                )
                .expect("count");
            assert_eq!(count, remaining);
        }
    }
}
