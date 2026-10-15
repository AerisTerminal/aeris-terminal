use super::{TradingAccountId, TradingStore, database_error, params, usize_to_i64};

impl TradingStore {
    pub(crate) fn delete_practice_account(
        &mut self,
        account_id: &TradingAccountId,
        managed_bracket_ids: &[String],
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        for bracket_id in managed_bracket_ids {
            transaction
                .execute(
                    "DELETE FROM managed_brackets WHERE bracket_id = ?1",
                    [bracket_id],
                )
                .map_err(database_error)?;
        }
        for statement in [
            "DELETE FROM trade_copier_targets WHERE source_account_id = ?1 OR target_account_id = ?1",
            "DELETE FROM economic_event_risk_actions WHERE account_id = ?1",
            "DELETE FROM discipline_states WHERE account_id = ?1",
            "DELETE FROM session_plans WHERE account_id = ?1",
            "DELETE FROM trade_pnl_cycles WHERE account_id = ?1",
            "DELETE FROM risk_trade_cycles WHERE account_id = ?1",
            "DELETE FROM risk_rule_states WHERE account_id = ?1",
            "DELETE FROM risk_locks WHERE account_id = ?1",
            "DELETE FROM risk_profiles WHERE account_id = ?1",
            "DELETE FROM protective_orders WHERE client_order_id IN (
                SELECT client_order_id FROM orders WHERE account_id = ?1
             )",
            "DELETE FROM order_events WHERE order_id IN (
                SELECT id FROM orders WHERE account_id = ?1
             )",
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
        transaction.commit().map_err(database_error)
    }
}

impl TradingStore {
    pub(crate) fn enforce_retention(&mut self) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM fills WHERE id IN (
                    SELECT id FROM fills ORDER BY execution_unix_nanos DESC LIMIT -1 OFFSET ?1
                 )",
                [usize_to_i64(self.retention.maximum_fills)?],
            )
            .and_then(|_| {
                transaction.execute(
                    "DELETE FROM order_events WHERE id IN (
                        SELECT id FROM order_events ORDER BY sequence DESC LIMIT -1 OFFSET ?1
                     )",
                    [usize_to_i64(self.retention.maximum_order_events).unwrap_or(i64::MAX)],
                )
            })
            .map_err(database_error)?;
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
            transaction
                .execute(
                    "DELETE FROM user_records WHERE kind = ?1 AND id IN (
                        SELECT id FROM user_records WHERE kind = ?1
                        ORDER BY updated_unix_nanos DESC LIMIT -1 OFFSET ?2
                     )",
                    params![
                        kind,
                        usize_to_i64(self.retention.maximum_user_records_per_kind)?
                    ],
                )
                .map_err(database_error)?;
        }
        transaction
            .execute(
                "DELETE FROM economic_event_risk_actions WHERE rowid IN (
                    SELECT rowid FROM economic_event_risk_actions
                    ORDER BY applied_unix_nanos DESC LIMIT -1 OFFSET ?1
                 )",
                [usize_to_i64(self.retention.maximum_user_records_per_kind)?],
            )
            .map_err(database_error)?;
        let working_orders = transaction
            .query_row(
                "SELECT COUNT(*) FROM orders WHERE status IN
                    ('pending', 'working', 'pending_modify', 'partially_filled', 'pending_cancel')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(database_error)?;
        let working_orders = usize::try_from(working_orders)
            .map_err(|_| "stored working-order count is invalid".to_string())?;
        let retained_closed_orders = self.retention.maximum_orders.saturating_sub(working_orders);
        let retained_closed_orders = usize_to_i64(retained_closed_orders)?;
        for table in ["fills", "order_events"] {
            transaction
                .execute(
                    &format!(
                        "DELETE FROM {table} WHERE order_id IN (
                            SELECT id FROM orders WHERE status NOT IN
                                ('pending', 'working', 'pending_modify', 'partially_filled',
                                 'pending_cancel')
                            ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?1
                         )"
                    ),
                    [retained_closed_orders],
                )
                .map_err(database_error)?;
        }
        transaction
            .execute(
                "DELETE FROM orders WHERE id IN (
                    SELECT id FROM orders WHERE status NOT IN
                        ('pending', 'working', 'pending_modify', 'partially_filled',
                         'pending_cancel')
                    ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?1
                 )",
                [retained_closed_orders],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }
}
