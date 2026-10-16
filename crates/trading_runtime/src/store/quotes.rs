//! Persists the complete simulated quote projection in a single rollback-on-drop transaction.
//! The owner computes `after` on private speculative state before calling this function.

use super::fills::{insert_fill_accounting, upsert_trade_cycle};
use super::groups::upsert_managed_bracket;
use super::{
    FixedPoint, StepTx, StoredState, TradingStore, database_error, encode_provenance,
    insert_event_row, insert_order_row, params, update_next_sequence, upsert_discipline_state,
    upsert_position, upsert_risk_rule_state,
};

impl TradingStore {
    pub(crate) fn commit_quote_step(
        &mut self,
        before: &StoredState,
        after: &StoredState,
    ) -> Result<(), String> {
        let tx = StepTx::begin(self).map_err(database_error)?;
        persist_quote_orders_and_fills(&tx, before, after)?;
        for (key, position) in &after.positions {
            if before.positions.get(key).is_none_or(|previous| {
                let mut prior = previous.clone();
                prior.unrealized_pnl = position.unrealized_pnl;
                prior != *position
            }) {
                upsert_position(&tx, position)?;
            }
        }
        for (key, cycle) in &after.trade_cycles {
            if before.trade_cycles.get(key) != Some(cycle) {
                upsert_trade_cycle(&tx, cycle)?;
            }
        }
        for (key, state) in &after.risk_rule_states {
            if before.risk_rule_states.get(key) != Some(state) {
                upsert_risk_rule_state(&tx, state)?;
            }
        }
        for (key, state) in &after.discipline_states {
            if before.discipline_states.get(key) != Some(state) {
                upsert_discipline_state(&tx, state)?;
            }
        }
        for (key, lock) in &after.risk_locks {
            if before.risk_locks.get(key) != Some(lock) {
                tx.execute(
                    "INSERT INTO risk_locks(account_id, reason, locked_at_unix_nanos, profile_id,
                     profile_version) VALUES (?1,?2,?3,?4,?5)
                     ON CONFLICT(account_id) DO UPDATE SET reason=excluded.reason,
                     locked_at_unix_nanos=excluded.locked_at_unix_nanos,
                     profile_id=excluded.profile_id, profile_version=excluded.profile_version",
                    params![
                        key.as_str(),
                        lock.reason,
                        lock.locked_at_unix_nanos,
                        lock.profile_id,
                        lock.profile_version
                    ],
                )
                .map_err(database_error)?;
            }
        }
        for (id, bracket) in &after.managed_brackets {
            if before.managed_brackets.get(id) != Some(bracket) {
                upsert_managed_bracket(&tx, bracket)?;
            }
        }
        update_next_sequence(&tx, after.next_sequence)?;
        tx.execute(
            "UPDATE metadata SET value=?1 WHERE key='revision'",
            [i64::try_from(after.revision).map_err(|error| error.to_string())?],
        )
        .map_err(database_error)?;
        tx.commit_without_revision().map_err(database_error)
    }
}

fn persist_quote_orders_and_fills(
    tx: &StepTx<'_>,
    before: &StoredState,
    after: &StoredState,
) -> Result<(), String> {
    for (id, order) in after.orders.iter() {
        match before.orders.get(id) {
            None => insert_order_row(tx, order)?,
            Some(old) if old != order => {
                tx.execute(
                    "UPDATE orders SET status=?1, time_in_force=?2, quantity_units=?3,
                         quantity_scale=?4, filled_units=?5, filled_scale=?6, limit_units=?7,
                         limit_scale=?8, stop_units=?9, stop_scale=?10, provenance_json=?11,
                         terminal_unix_nanos=?12, revision=?13 WHERE id=?14",
                    params![
                        order.status.as_str(),
                        order.time_in_force.as_str(),
                        order.quantity.units(),
                        order.quantity.scale(),
                        order.filled_quantity.units(),
                        order.filled_quantity.scale(),
                        order.limit_price.map(FixedPoint::units),
                        order.limit_price.map(FixedPoint::scale),
                        order.stop_price.map(FixedPoint::units),
                        order.stop_price.map(FixedPoint::scale),
                        encode_provenance(&order.provenance),
                        order.terminal_unix_nanos,
                        order.revision,
                        id.as_str(),
                    ],
                )
                .map_err(database_error)?;
            }
            Some(_) => {}
        }
    }
    for event in after
        .order_events
        .iter()
        .filter(|event| event.sequence >= before.next_sequence)
    {
        insert_event_row(tx, event)?;
    }
    for fill in after
        .fills
        .iter()
        .filter(|fill| !before.fill_accounting.contains_key(&fill.id))
    {
        tx.execute(
            "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                 price_scale, quantity_units, quantity_scale, execution_unix_nanos, provenance_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                fill.id.as_str(),
                fill.order_id.as_str(),
                fill.account_id.as_str(),
                fill.instrument_id.as_str(),
                fill.side.as_str(),
                fill.price.units(),
                fill.price.scale(),
                fill.quantity.units(),
                fill.quantity.scale(),
                fill.execution_unix_nanos,
                encode_provenance(&fill.provenance),
            ],
        )
        .map_err(database_error)?;
        let accounting = after
            .fill_accounting
            .get(&fill.id)
            .ok_or("quote fill accounting is missing")?;
        insert_fill_accounting(tx, fill, *accounting)?;
    }
    Ok(())
}
