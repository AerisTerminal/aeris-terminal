use super::{StepTx, TradingAccountId, TradingStore, database_error, params, usize_to_i64};

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
        let transaction = self.connection.transaction().map_err(database_error)?;
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

impl TradingStore {
    pub(crate) fn enforce_retention(&mut self) -> Result<(), String> {
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
            self.retention.maximum_orders.saturating_sub(
                usize::try_from(working_orders)
                    .map_err(|_| "stored working-order count is invalid".to_string())?,
            ),
        )?;
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
                params![
                    usize_to_i64(self.retention.maximum_fills)?,
                    retained_closed_orders
                ],
            )
            .and_then(|_| {
                transaction.execute(
                    "DELETE FROM order_events WHERE id IN (
                        SELECT id FROM order_events WHERE id IN (
                            SELECT id FROM order_events ORDER BY sequence DESC LIMIT -1 OFFSET ?1
                        ) OR order_id IN (
                            SELECT id FROM orders WHERE status NOT IN
                                ('pending', 'working', 'pending_modify', 'partially_filled',
                                 'pending_cancel')
                            ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?2
                        ) ORDER BY sequence LIMIT 5000
                     )",
                    params![
                        usize_to_i64(self.retention.maximum_order_events).unwrap_or(i64::MAX),
                        retained_closed_orders
                    ],
                )
            })
            .map_err(database_error)?;
        delete_old_user_records(&transaction, self.retention.maximum_user_records_per_kind)?;
        transaction
            .execute(
                "DELETE FROM economic_event_risk_actions WHERE rowid IN (
                    SELECT rowid FROM economic_event_risk_actions
                    ORDER BY applied_unix_nanos DESC LIMIT -1 OFFSET ?1
                ) AND rowid IN (
                    SELECT rowid FROM economic_event_risk_actions
                    ORDER BY applied_unix_nanos LIMIT 5000
                 )",
                [usize_to_i64(self.retention.maximum_user_records_per_kind)?],
            )
            .map_err(database_error)?;
        transaction
            .execute(
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
                 )",
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
            store.enforce_retention().expect("hourly retention");
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
