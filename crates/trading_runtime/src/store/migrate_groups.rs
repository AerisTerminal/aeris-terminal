//! Startup conversion of legacy protection facts. Until the M5.5 owner switch,
//! legacy rows remain authoritative; these rows are a durable migration projection.

use super::{StepTx, TradingStore, database_error};

// A historical group can claim its old child orders only when all template
// members survived retention. Never publish an incomplete active protection set.
const COMPLETE_LINKED_CHILDREN: &str = "b.stop_client_order_id IS NOT NULL
    AND EXISTS (SELECT 1 FROM orders s WHERE s.client_order_id=b.stop_client_order_id)
    AND json_array_length(b.target_client_order_ids_json) > 0
    AND json_array_length(b.target_client_order_ids_json)
        = json_array_length(b.template_json,'$.targets')
    AND (SELECT count(*) FROM json_each(b.target_client_order_ids_json) j
         JOIN orders t ON t.client_order_id=j.value)
        = json_array_length(b.target_client_order_ids_json)";

impl TradingStore {
    pub(super) fn backfill_groups(&self) -> Result<(), String> {
        let tx = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        Self::project_protection_groups(&tx)?;
        Self::project_terminal_brackets(&tx)?;
        Self::project_active_brackets(&tx)?;
        tx.commit_without_revision().map_err(database_error)
    }

    fn project_protection_groups(tx: &StepTx<'_>) -> Result<(), String> {
        tx.execute(
            "INSERT INTO order_groups(group_id, account_id, instrument_id, kind, status,
                entry_order_id, management_json, template_id, template_revision, revision)
             SELECT 'protection/' || o.id, o.account_id, o.instrument_id, 'protection',
                CASE WHEN o.status IN ('working','partially_filled','pending','inactive')
                     THEN 'active' ELSE 'completed' END,
                NULL, '{}', NULL, NULL, 1
             FROM protective_orders p JOIN orders o ON o.client_order_id=p.client_order_id
             WHERE NOT EXISTS (SELECT 1 FROM order_groups g
                 WHERE g.group_id='protection/' || o.id)",
            [],
        )
        .map_err(database_error)?;
        let incompatible: i64 = tx
            .query_row(
                "SELECT count(*) FROM protective_orders p
                 JOIN orders o ON o.client_order_id=p.client_order_id
                 JOIN order_groups g ON g.group_id='protection/' || o.id
                 WHERE g.kind!='protection' OR g.account_id!=o.account_id
                    OR g.instrument_id!=o.instrument_id OR g.entry_order_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if incompatible != 0 {
            return Err("stored protection group collides with a different group".into());
        }
        tx.execute(
            "INSERT INTO order_group_legs(group_id,order_id,role,target_index,
                offset_ticks,allocation_percent)
             SELECT 'protection/' || o.id, o.id, p.role,
                CASE WHEN p.role='take_profit' THEN 0 ELSE NULL END, NULL, NULL
             FROM protective_orders p JOIN orders o ON o.client_order_id=p.client_order_id
             WHERE NOT EXISTS (SELECT 1 FROM order_group_legs l
                 WHERE l.order_id=o.id AND l.group_id='protection/' || o.id)",
            [],
        )
        .map_err(database_error)?;
        Ok(())
    }

    /// Terminal brackets cannot execute again. Preserve their original orders and link
    /// retained children only when the complete legacy child set is still present.
    fn project_terminal_brackets(tx: &StepTx<'_>) -> Result<(), String> {
        tx.execute(
            "INSERT INTO order_groups(group_id,account_id,instrument_id,kind,status,
                entry_order_id,management_json,template_id,template_revision,revision)
             SELECT b.bracket_id,e.account_id,e.instrument_id,'bracket',b.status,e.id,
                json_object('trailing',json_extract(b.template_json,'$.trailing_stop'),
                    'break_even',json_extract(b.template_json,'$.break_even'),
                    'entry_price',CASE WHEN b.entry_price_units IS NOT NULL
                        THEN json_object('units',b.entry_price_units,'scale',b.entry_price_scale)
                        ELSE NULL END),
                json_extract(b.template_json,'$.template_id'),
                json_extract(b.template_json,'$.revision'),1
             FROM managed_brackets b JOIN orders e
                ON e.client_order_id=b.entry_client_order_id
             WHERE b.status IN ('completed','cancelled')
                AND NOT EXISTS (SELECT 1 FROM order_groups g WHERE g.group_id=b.bracket_id)",
            [],
        )
        .map_err(database_error)?;
        let incompatible: i64 = tx
            .query_row(
                "SELECT count(*) FROM managed_brackets b
                 JOIN orders e ON e.client_order_id=b.entry_client_order_id
                 JOIN order_groups g ON g.group_id=b.bracket_id
                 WHERE b.status IN ('completed','cancelled') AND
                    (g.kind!='bracket' OR g.status NOT IN (b.status,'active')
                     OR g.entry_order_id!=e.id
                     OR g.account_id!=e.account_id OR g.instrument_id!=e.instrument_id)",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if incompatible != 0 {
            return Err("stored terminal bracket collides with a different group".into());
        }
        // The old bracket writer still owns transitions during the migration
        // interval; reconcile its terminal state on the next store open.
        tx.execute(
            "UPDATE order_groups SET status=(
                SELECT b.status FROM managed_brackets b WHERE b.bracket_id=order_groups.group_id)
             WHERE kind='bracket' AND status='active'
               AND EXISTS (SELECT 1 FROM managed_brackets b
                   WHERE b.bracket_id=order_groups.group_id
                     AND b.status IN ('completed','cancelled'))",
            [],
        )
        .map_err(database_error)?;
        // A terminal legacy row can outlive retained child orders. Partial membership
        // would imply protection that never existed; record the terminal parent alone.
        let complete = COMPLETE_LINKED_CHILDREN;
        tx.execute(
            &format!(
                "INSERT INTO order_group_legs(group_id,order_id,role,target_index,
                    offset_ticks,allocation_percent)
                 SELECT b.bracket_id,s.id,'stop_loss',NULL,
                    json_extract(b.template_json,'$.stop_offset_ticks'),NULL
                 FROM managed_brackets b JOIN orders s
                    ON s.client_order_id=b.stop_client_order_id
                 JOIN order_groups g ON g.group_id=b.bracket_id
                 WHERE b.status IN ('completed','cancelled') AND {complete}
                    AND NOT EXISTS (SELECT 1 FROM order_group_legs l
                        WHERE l.order_id=s.id AND l.group_id=b.bracket_id)"
            ),
            [],
        )
        .map_err(database_error)?;
        tx.execute(
            &format!(
                "INSERT INTO order_group_legs(group_id,order_id,role,target_index,
                    offset_ticks,allocation_percent)
                 SELECT b.bracket_id,t.id,'take_profit',CAST(j.key AS INTEGER),
                    json_extract(b.template_json,'$.targets[' || j.key || '].offset_ticks'),
                    json_extract(b.template_json,'$.targets[' || j.key || '].quantity_percent')
                 FROM managed_brackets b JOIN order_groups g ON g.group_id=b.bracket_id
                 JOIN json_each(b.target_client_order_ids_json) j
                 JOIN orders t ON t.client_order_id=j.value
                 WHERE b.status IN ('completed','cancelled') AND {complete}
                    AND NOT EXISTS (SELECT 1 FROM order_group_legs l
                        WHERE l.order_id=t.id AND l.group_id=b.bracket_id)"
            ),
            [],
        )
        .map_err(database_error)?;
        Ok(())
    }

    /// Project complete active historical brackets without changing the legacy
    /// writer's ownership of any order, quantity or bracket transition.
    fn project_active_brackets(tx: &StepTx<'_>) -> Result<(), String> {
        let complete = COMPLETE_LINKED_CHILDREN;
        tx.execute(
            &format!(
                "INSERT INTO order_groups(group_id,account_id,instrument_id,kind,status,
                    entry_order_id,management_json,template_id,template_revision,revision)
                 SELECT b.bracket_id,e.account_id,e.instrument_id,'bracket','active',e.id,
                    json_object('trailing',json_extract(b.template_json,'$.trailing_stop'),
                        'break_even',json_extract(b.template_json,'$.break_even'),
                        'entry_price',CASE WHEN b.entry_price_units IS NOT NULL
                            THEN json_object('units',b.entry_price_units,'scale',b.entry_price_scale)
                            ELSE NULL END),
                    json_extract(b.template_json,'$.template_id'),
                    json_extract(b.template_json,'$.revision'),1
                 FROM managed_brackets b JOIN orders e
                    ON e.client_order_id=b.entry_client_order_id
                 WHERE b.status='active' AND {complete}
                    AND NOT EXISTS (SELECT 1 FROM order_groups g WHERE g.group_id=b.bracket_id)"
            ),
            [],
        )
        .map_err(database_error)?;
        let incompatible: i64 = tx
            .query_row(
                "SELECT count(*) FROM managed_brackets b JOIN orders e
                    ON e.client_order_id=b.entry_client_order_id
                 JOIN order_groups g ON g.group_id=b.bracket_id
                 WHERE b.status='active' AND
                    (g.kind!='bracket' OR g.status!='active' OR g.entry_order_id!=e.id
                     OR g.account_id!=e.account_id OR g.instrument_id!=e.instrument_id)",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if incompatible != 0 {
            return Err("stored active bracket collides with a different group".into());
        }
        tx.execute(
            &format!(
                "INSERT INTO order_group_legs(group_id,order_id,role,target_index,
                    offset_ticks,allocation_percent)
                 SELECT b.bracket_id,s.id,'stop_loss',NULL,
                    json_extract(b.template_json,'$.stop_offset_ticks'),NULL
                 FROM managed_brackets b JOIN orders s
                    ON s.client_order_id=b.stop_client_order_id
                 JOIN order_groups g ON g.group_id=b.bracket_id
                 WHERE b.status='active' AND {complete}
                    AND NOT EXISTS (SELECT 1 FROM order_group_legs l
                        WHERE l.order_id=s.id AND l.group_id=b.bracket_id)"
            ),
            [],
        )
        .map_err(database_error)?;
        tx.execute(
            &format!(
                "INSERT INTO order_group_legs(group_id,order_id,role,target_index,
                    offset_ticks,allocation_percent)
                 SELECT b.bracket_id,t.id,'take_profit',CAST(j.key AS INTEGER),
                    json_extract(b.template_json,'$.targets[' || j.key || '].offset_ticks'),
                    json_extract(b.template_json,'$.targets[' || j.key || '].quantity_percent')
                 FROM managed_brackets b JOIN order_groups g ON g.group_id=b.bracket_id
                 JOIN json_each(b.target_client_order_ids_json) j
                 JOIN orders t ON t.client_order_id=j.value
                 WHERE b.status='active' AND {complete}
                    AND NOT EXISTS (SELECT 1 FROM order_group_legs l
                        WHERE l.order_id=t.id AND l.group_id=b.bracket_id)"
            ),
            [],
        )
        .map_err(database_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TradingRetention;
    use rusqlite::Connection;
    use std::fs;

    #[test]
    fn v19_protection_migrates_once_without_changing_legacy_order() {
        let connection = Connection::open_in_memory().unwrap();
        super::super::create_schema_at(&connection, 19);
        connection
            .execute_batch(
                "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 stop_units,stop_scale,status,submitted_unix_nanos,provenance_json)
             VALUES ('stop-1','protect-1','sim','BTC-USDC','sell','stop','gtc',
                 2,0,0,0,9900,2,'working',1,'{}');
             INSERT INTO protective_orders(client_order_id,role)
             VALUES ('protect-1','stop_loss');",
            )
            .unwrap();
        let mut store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        store.migrate(false).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM order_groups", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store.backfill_groups().unwrap();
        let actual: (String, String, String, String, String) = store
            .connection
            .query_row(
                "SELECT g.group_id,g.kind,g.status,l.role,o.status FROM order_groups g
             JOIN order_group_legs l USING(group_id) JOIN orders o ON l.order_id=o.id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            actual,
            (
                "protection/stop-1".into(),
                "protection".into(),
                "active".into(),
                "stop_loss".into(),
                "working".into()
            )
        );
        let before = store.write_count();
        store.backfill_groups().unwrap();
        assert_eq!(store.write_count(), before);
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM protective_orders", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn conflicting_group_membership_aborts_the_entire_backfill() {
        let connection = Connection::open_in_memory().unwrap();
        super::super::create_schema_at(&connection, 23);
        connection
            .execute_batch(
                "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 stop_units,stop_scale,status,submitted_unix_nanos,provenance_json)
             VALUES ('stop-a','protect-a','sim','BTC-USDC','sell','stop','gtc',
                     2,0,0,0,9900,2,'working',1,'{}'),
                    ('stop-b','protect-b','sim','BTC-USDC','sell','stop','gtc',
                     2,0,0,0,9900,2,'working',2,'{}');
             INSERT INTO protective_orders(client_order_id,role)
             VALUES ('protect-a','stop_loss'),('protect-b','stop_loss');
             INSERT INTO order_groups(group_id,account_id,instrument_id,kind,status,
                 management_json,revision)
             VALUES ('other','sim','BTC-USDC','protection','active','{}',1);
             INSERT INTO order_group_legs(group_id,order_id,role)
             VALUES ('other','stop-b','stop_loss');",
            )
            .unwrap();
        let store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        assert!(store.backfill_groups().is_err());
        let group_count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM order_groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            group_count, 1,
            "failed backfill rolls back newly projected rows"
        );
    }

    #[test]
    fn failed_last_bracket_leg_rolls_back_all_historical_groups() {
        let connection = Connection::open_in_memory().unwrap();
        super::super::create_schema_at(&connection, 23);
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 status,submitted_unix_nanos,provenance_json)
             VALUES ('protect','protect-client','sim','BTC-USDC','sell','stop','gtc',
                     1,0,0,0,'working',1,'{}'),
                    ('entry','entry-client','sim','BTC-USDC','buy','limit','gtc',
                     1,0,1,0,'filled',1,'{}'),
                    ('stop','stop-client','sim','BTC-USDC','sell','stop','gtc',
                     1,0,0,0,'working',2,'{}'),
                    ('target','target-client','sim','BTC-USDC','sell','limit','gtc',
                     1,0,0,0,'working',2,'{}');
             INSERT INTO protective_orders(client_order_id,role)
             VALUES ('protect-client','stop_loss');
             CREATE TRIGGER fail_last_group_leg BEFORE INSERT ON order_group_legs
             WHEN NEW.order_id='target' BEGIN SELECT RAISE(ABORT,'injected last leg failure'); END;"
        ).unwrap();
        let template = serde_json::json!({
            "template_id":"historical","revision":1,"name":"Historical",
            "stop_offset_ticks":8,
            "targets":[{"offset_ticks":8,"quantity_percent":100}],
            "trailing_stop":null,"break_even":null,"enabled":true
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
                stop_client_order_id,target_client_order_ids_json,status)
             VALUES ('entry-client',?1,'entry-client','stop-client',
                '[\"target-client\"]','active')",
                [&template],
            )
            .unwrap();
        let store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        assert!(store.backfill_groups().is_err());
        for table in ["order_groups", "order_group_legs"] {
            assert_eq!(
                store
                    .connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{table} must roll back"
            );
        }
        store
            .connection
            .execute_batch("DROP TRIGGER fail_last_group_leg")
            .unwrap();
        store.backfill_groups().unwrap();
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM order_groups", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM order_group_legs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
        let before = store.write_count();
        store.backfill_groups().unwrap();
        assert_eq!(store.write_count(), before);
    }

    #[test]
    fn startup_backfills_schema_19_and_already_current_protection() {
        let path = super::super::tests::temporary_database("protection-groups-v19");
        let connection = Connection::open(&path).unwrap();
        super::super::create_schema_at(&connection, 19);
        connection
            .execute_batch(
                "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 stop_units,stop_scale,status,submitted_unix_nanos,provenance_json)
             VALUES ('stop-1','protect-1','sim','BTC-USDC','sell','stop','gtc',
                 2,0,0,0,9900,2,'working',1,'{}');
             INSERT INTO protective_orders(client_order_id,role)
             VALUES ('protect-1','stop_loss');",
            )
            .unwrap();
        drop(connection);
        let store = TradingStore::open(&path, TradingRetention::default()).unwrap();
        let count = |store: &TradingStore| {
            store
                .connection
                .query_row("SELECT count(*) FROM order_group_legs", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
        };
        assert_eq!(count(&store), 1);
        let backups: Vec<_> = fs::read_dir(path.parent().unwrap().join("backups"))
            .unwrap()
            .map(|r| r.unwrap().path())
            .collect();
        assert!(backups.iter().any(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("trading-v19-")
        }));
        drop(store);
        let store = TradingStore::open(&path, TradingRetention::default()).unwrap();
        assert_eq!(count(&store), 1);
        assert_eq!(
            store
                .connection
                .query_row("SELECT status FROM orders WHERE id='stop-1'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "working"
        );
        drop(store);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn terminal_bracket_links_retained_children_without_rewriting_orders() {
        let connection = Connection::open_in_memory().unwrap();
        super::super::create_schema_at(&connection, 23);
        connection
            .execute_batch(
                "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 status,submitted_unix_nanos,provenance_json)
             VALUES ('entry','client-entry','sim','BTC-USDC','buy','limit','gtc',
                     2,0,2,0,'filled',1,'{}'),
                    ('stop','client-stop','sim','BTC-USDC','sell','stop','gtc',
                     2,0,2,0,'filled',2,'{}'),
                    ('target','client-target','sim','BTC-USDC','sell','limit','gtc',
                     2,0,0,0,'cancelled',2,'{}');",
            )
            .unwrap();
        let template = serde_json::json!({
            "template_id":"historical","revision":3,"name":"Historical",
            "stop_offset_ticks":8,
            "targets":[{"offset_ticks":16,"quantity_percent":100}],
            "trailing_stop":null,"break_even":null,"enabled":true
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
                stop_client_order_id,target_client_order_ids_json,status,entry_price_units,
                entry_price_scale) VALUES ('client-entry',?1,'client-entry','client-stop',
                '[\"client-target\"]','completed',10000,2)",
                [&template],
            )
            .unwrap();
        let store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        store.backfill_groups().unwrap();
        let facts: (String, String, String, i64) = store
            .connection
            .query_row(
                "SELECT status,entry_order_id,template_id,template_revision FROM order_groups",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            facts,
            ("completed".into(), "entry".into(), "historical".into(), 3)
        );
        let legs: Vec<_> = store
            .connection
            .prepare(
                "SELECT order_id,role,offset_ticks,allocation_percent FROM order_group_legs
             ORDER BY CASE role WHEN 'stop_loss' THEN 0 ELSE 1 END",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            legs,
            vec![
                ("stop".into(), "stop_loss".into(), 8, None),
                ("target".into(), "take_profit".into(), 16, Some(100)),
            ]
        );
        assert_incomplete_terminal_bracket(&store, &template);
        let before = store.write_count();
        store.backfill_groups().unwrap();
        assert_eq!(store.write_count(), before);
        assert_eq!(
            store
                .connection
                .query_row("SELECT status FROM orders WHERE id='target'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "cancelled"
        );
    }

    #[test]
    fn v19_active_bracket_links_existing_orders_and_survives_reopen() {
        let path = super::super::tests::temporary_database("active-bracket-v19");
        let connection = Connection::open(&path).unwrap();
        super::super::create_schema_at(&connection, 19);
        connection.execute_batch(
            "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 limit_units,limit_scale,stop_units,stop_scale,status,submitted_unix_nanos,provenance_json)
             VALUES ('entry','entry-client','sim','BTC-USDC','buy','limit','gtc',
                     4,0,4,0,10000,2,NULL,NULL,'filled',1,'{}'),
                    ('stop','stop-client','sim','BTC-USDC','sell','stop','gtc',
                     4,0,0,0,NULL,NULL,9920,2,'working',2,'{}'),
                    ('target-a','target-a-client','sim','BTC-USDC','sell','limit','gtc',
                     2,0,0,0,10080,2,NULL,NULL,'working',2,'{}'),
                    ('target-b','target-b-client','sim','BTC-USDC','sell','limit','gtc',
                     2,0,0,0,10160,2,NULL,NULL,'working',2,'{}');"
        ).unwrap();
        let template = serde_json::json!({
            "template_id":"historical-scale-out","revision":4,"name":"Historical",
            "stop_offset_ticks":8,
            "targets":[{"offset_ticks":8,"quantity_percent":50},
                       {"offset_ticks":16,"quantity_percent":50}],
            "trailing_stop":{"activation_ticks":12,"distance_ticks":6},
            "break_even":null,"enabled":true
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
                stop_client_order_id,target_client_order_ids_json,status,entry_price_units,
                entry_price_scale) VALUES ('entry-client',?1,'entry-client','stop-client',
                '[\"target-a-client\",\"target-b-client\"]','active',10000,2)",
                [&template],
            )
            .unwrap();
        drop(connection);

        for attempt in 0..2 {
            let store = TradingStore::open(&path, TradingRetention::default()).unwrap();
            assert_active_group(&store);
            assert_eq!(
                store
                    .connection
                    .query_row("SELECT count(*) FROM managed_brackets", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            if attempt == 0 {
                let backups: Vec<_> = fs::read_dir(path.parent().unwrap().join("backups"))
                    .unwrap()
                    .map(|r| r.unwrap().path())
                    .collect();
                assert!(backups.iter().any(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("trading-v19-")
                }));
            }
        }
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE managed_brackets SET status='completed' WHERE bracket_id='entry-client'",
                [],
            )
            .unwrap();
        drop(connection);
        let store = TradingStore::open(&path, TradingRetention::default()).unwrap();
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT status FROM order_groups WHERE group_id='entry-client'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "completed"
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT count(*) FROM order_group_legs WHERE group_id='entry-client'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            3
        );
        drop(store);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn assert_active_group(store: &TradingStore) {
        let group: (String, String, String, i64, String) = store
            .connection
            .query_row(
                "SELECT status,entry_order_id,template_id,template_revision,management_json
             FROM order_groups WHERE group_id='entry-client'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            (&group.0[..], &group.1[..], &group.2[..], group.3),
            ("active", "entry", "historical-scale-out", 4)
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&group.4).unwrap()["trailing"]["activation_ticks"],
            12
        );
        let legs: Vec<_> = store
            .connection
            .prepare(
                "SELECT l.order_id,l.role,l.target_index,l.offset_ticks,l.allocation_percent,
                o.status,o.quantity_units,o.limit_units,o.stop_units
             FROM order_group_legs l JOIN orders o ON o.id=l.order_id
             WHERE l.group_id='entry-client'
             ORDER BY CASE l.role WHEN 'stop_loss' THEN -1 ELSE l.target_index END",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            legs,
            vec![
                (
                    "stop".into(),
                    "stop_loss".into(),
                    None,
                    8,
                    None,
                    "working".into(),
                    4,
                    None,
                    Some(9920)
                ),
                (
                    "target-a".into(),
                    "take_profit".into(),
                    Some(0),
                    8,
                    Some(50),
                    "working".into(),
                    2,
                    Some(10080),
                    None
                ),
                (
                    "target-b".into(),
                    "take_profit".into(),
                    Some(1),
                    16,
                    Some(50),
                    "working".into(),
                    2,
                    Some(10160),
                    None
                )
            ]
        );
    }

    fn assert_incomplete_terminal_bracket(store: &TradingStore, template: &str) {
        store
            .connection
            .execute_batch(
                "INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 status,submitted_unix_nanos,provenance_json)
             VALUES ('entry-2','client-entry-2','sim','BTC-USDC','buy','limit','gtc',
                     2,0,0,0,'cancelled',3,'{}'),
                    ('stop-2','client-stop-2','sim','BTC-USDC','sell','stop','gtc',
                     2,0,0,0,'cancelled',3,'{}');",
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO managed_brackets(bracket_id,template_json,entry_client_order_id,
                stop_client_order_id,target_client_order_ids_json,status)
             VALUES ('client-entry-2',?1,'client-entry-2','client-stop-2',
                '[\"missing-target\"]','cancelled')",
                [&template],
            )
            .unwrap();
        store.backfill_groups().unwrap();
        let incomplete: (String, i64) = store
            .connection
            .query_row(
                "SELECT g.status,count(l.order_id) FROM order_groups g
             LEFT JOIN order_group_legs l ON l.group_id=g.group_id
             WHERE g.group_id='client-entry-2' GROUP BY g.group_id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            incomplete,
            ("cancelled".into(), 0),
            "never publish partial protection"
        );
    }
}
