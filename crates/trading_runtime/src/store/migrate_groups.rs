//! Startup conversion of legacy protection facts. Until the M5.5 owner switch,
//! legacy rows remain authoritative; these rows are a durable migration projection.

use super::{StepTx, TradingStore, database_error};

impl TradingStore {
    pub(super) fn backfill_protection_groups(&self) -> Result<(), String> {
        let tx = StepTx::begin_shared(&self.connection).map_err(database_error)?;
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
        tx.commit_without_revision().map_err(database_error)
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
        super::super::tests::create_schema_at(&connection, 19);
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
        store.backfill_protection_groups().unwrap();
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
        store.backfill_protection_groups().unwrap();
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
        super::super::tests::create_schema_at(&connection, 23);
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
        assert!(store.backfill_protection_groups().is_err());
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
    fn startup_backfills_schema_19_and_already_current_protection() {
        let path = super::super::tests::temporary_database("protection-groups-v19");
        let connection = Connection::open(&path).unwrap();
        super::super::tests::create_schema_at(&connection, 19);
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
}
