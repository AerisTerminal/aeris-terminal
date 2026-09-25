use super::{
    RiskLock, RiskProfile, TradingInstrument, TradingRetention, TrailingDrawdownMode, UserRecord,
};
use aeris_instruments::{
    ContractDate, ContractMetadata, InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance,
    SessionHours,
};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, Fill, FillId, FixedPoint, Order, OrderEvent, OrderEventId,
    OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position, TimeInForce,
    TradingAccount, TradingAccountId, TradingProvenance,
};
use rusqlite::{Connection, Transaction, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    fs,
    path::Path,
};

const SCHEMA_VERSION: u32 = 3;
const INITIAL_SCHEMA: &str = "CREATE TABLE metadata (
     key TEXT PRIMARY KEY,
     value INTEGER NOT NULL
 ) STRICT;
 INSERT INTO metadata(key, value) VALUES ('revision', 1), ('next_sequence', 1);
 CREATE TABLE accounts (
     id TEXT PRIMARY KEY, display_name TEXT NOT NULL, environment TEXT NOT NULL,
     currency TEXT NOT NULL, currency_scale INTEGER NOT NULL
 ) STRICT;
 CREATE TABLE instruments (
     id TEXT PRIMARY KEY, price_scale INTEGER NOT NULL, quantity_scale INTEGER NOT NULL,
     contract_json TEXT NOT NULL
 ) STRICT;
 CREATE TABLE orders (
     id TEXT PRIMARY KEY, client_order_id TEXT NOT NULL UNIQUE,
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id), side TEXT NOT NULL,
     order_type TEXT NOT NULL, time_in_force TEXT NOT NULL, quantity_units INTEGER NOT NULL,
     quantity_scale INTEGER NOT NULL, limit_units INTEGER, limit_scale INTEGER,
     stop_units INTEGER, stop_scale INTEGER, status TEXT NOT NULL,
     submitted_unix_nanos INTEGER NOT NULL, provenance_json TEXT NOT NULL
 ) STRICT;
 CREATE TABLE order_events (
     id TEXT PRIMARY KEY, order_id TEXT NOT NULL REFERENCES orders(id),
     sequence INTEGER NOT NULL UNIQUE, kind TEXT NOT NULL, event_unix_nanos INTEGER NOT NULL,
     detail TEXT, provenance_json TEXT NOT NULL
 ) STRICT;
 CREATE INDEX order_events_sequence ON order_events(sequence);
 CREATE TABLE fills (
     id TEXT PRIMARY KEY, order_id TEXT NOT NULL REFERENCES orders(id),
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id), side TEXT NOT NULL,
     price_units INTEGER NOT NULL, price_scale INTEGER NOT NULL,
     quantity_units INTEGER NOT NULL, quantity_scale INTEGER NOT NULL,
     execution_unix_nanos INTEGER NOT NULL, provenance_json TEXT NOT NULL
 ) STRICT;
 CREATE INDEX fills_execution ON fills(execution_unix_nanos);
 CREATE TABLE positions (
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id), net_units INTEGER NOT NULL,
     net_scale INTEGER NOT NULL, average_units INTEGER, average_scale INTEGER,
     realized_units INTEGER NOT NULL, realized_scale INTEGER NOT NULL,
     unrealized_units INTEGER NOT NULL, unrealized_scale INTEGER NOT NULL,
     last_fill_unix_nanos INTEGER NOT NULL, PRIMARY KEY(account_id, instrument_id)
 ) STRICT;
 CREATE TABLE user_records (
     kind TEXT NOT NULL, id TEXT NOT NULL, revision INTEGER NOT NULL,
     updated_unix_nanos INTEGER NOT NULL, json TEXT NOT NULL, PRIMARY KEY(kind, id)
 ) STRICT;
 CREATE INDEX user_records_retention ON user_records(kind, updated_unix_nanos);";
const MIGRATION_V2: &str = "CREATE TABLE risk_profiles (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), profile_id TEXT NOT NULL,
     version INTEGER NOT NULL, daily_loss_units INTEGER NOT NULL, daily_loss_scale INTEGER NOT NULL,
     trailing_units INTEGER, trailing_scale INTEGER, trailing_mode TEXT NOT NULL,
     max_contracts_units INTEGER NOT NULL, max_contracts_scale INTEGER NOT NULL,
     consistency_percent INTEGER, restricted_until_unix_nanos INTEGER, enabled INTEGER NOT NULL
 ) STRICT;
 CREATE TABLE risk_locks (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), reason TEXT NOT NULL,
     locked_at_unix_nanos INTEGER NOT NULL, profile_id TEXT, profile_version INTEGER
 ) STRICT;";
const MIGRATION_V3: &str =
    "ALTER TABLE risk_profiles ADD COLUMN session_start_unix_nanos INTEGER NOT NULL DEFAULT 1;";

pub(super) struct StoredState {
    pub revision: u64,
    pub next_sequence: u64,
    pub accounts: BTreeMap<TradingAccountId, TradingAccount>,
    pub instruments: BTreeMap<InstrumentId, TradingInstrument>,
    pub orders: BTreeMap<OrderId, Order>,
    pub order_events: VecDeque<OrderEvent>,
    pub fills: VecDeque<Fill>,
    pub positions: BTreeMap<(TradingAccountId, InstrumentId), Position>,
    pub risk_profiles: BTreeMap<TradingAccountId, RiskProfile>,
    pub risk_locks: BTreeMap<TradingAccountId, RiskLock>,
}

pub(super) struct TradingStore {
    connection: Connection,
    retention: TradingRetention,
}

impl TradingStore {
    pub(super) fn open(path: &Path, retention: TradingRetention) -> Result<Self, String> {
        if path != Path::new(":memory:") {
            let parent = path
                .parent()
                .ok_or_else(|| "trading database has no parent directory".to_string())?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("trading data directory could not be created: {error}"))?;
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("trading database could not be opened: {error}"))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .and_then(|()| connection.pragma_update(None, "synchronous", "FULL"))
            .and_then(|()| connection.pragma_update(None, "foreign_keys", "ON"))
            .map_err(|error| format!("trading database safety settings failed: {error}"))?;
        let mut store = Self {
            connection,
            retention,
        };
        store.migrate()?;
        store.ensure_simulated_account()?;
        store.enforce_retention()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), String> {
        let mut version = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .map_err(|error| format!("trading schema version could not be read: {error}"))?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "trading database schema {version} is newer than supported schema {SCHEMA_VERSION}"
            ));
        }
        if version == 0 {
            let transaction = self
                .connection
                .transaction()
                .map_err(|error| format!("trading migration could not start: {error}"))?;
            transaction
                .execute_batch(INITIAL_SCHEMA)
                .map_err(|error| format!("trading schema migration failed: {error}"))?;
            transaction
                .pragma_update(None, "user_version", 1_u32)
                .map_err(|error| {
                    format!("trading schema version could not be committed: {error}")
                })?;
            transaction
                .commit()
                .map_err(|error| format!("trading migration could not commit: {error}"))?;
            version = 1;
        }
        if version == 1 {
            let transaction = self
                .connection
                .transaction()
                .map_err(|error| format!("trading schema migration could not start: {error}"))?;
            transaction
                .execute_batch(MIGRATION_V2)
                .map_err(|error| format!("trading schema migration failed: {error}"))?;
            transaction
                .pragma_update(None, "user_version", 2_u32)
                .map_err(|error| {
                    format!("trading schema version could not be committed: {error}")
                })?;
            transaction
                .commit()
                .map_err(|error| format!("trading migration could not commit: {error}"))?;
            version = 2;
        }
        if version == 2 {
            let transaction = self
                .connection
                .transaction()
                .map_err(|error| format!("trading schema migration could not start: {error}"))?;
            transaction
                .execute_batch(MIGRATION_V3)
                .map_err(|error| format!("trading schema migration failed: {error}"))?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|error| {
                    format!("trading schema version could not be committed: {error}")
                })?;
            transaction
                .commit()
                .map_err(|error| format!("trading migration could not commit: {error}"))?;
        }
        Ok(())
    }

    fn ensure_simulated_account(&mut self) -> Result<(), String> {
        let account = TradingAccount {
            id: TradingAccountId::try_new("aeris-sim-1").map_err(|error| error.to_string())?,
            display_name: "SIM • Aeris Practice".to_string(),
            environment: AccountEnvironment::Simulated,
            currency: "USD".to_string(),
            currency_scale: 2,
        };
        self.put_account(&account)
    }

    pub(super) fn load_state(&mut self) -> Result<StoredState, String> {
        let revision = self.metadata("revision")?;
        let next_sequence = self.metadata("next_sequence")?;
        let mut accounts = BTreeMap::new();
        {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT id, display_name, environment, currency, currency_scale FROM accounts ORDER BY id",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map([], |row| {
                    let environment: String = row.get(2)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        environment,
                        row.get::<_, String>(3)?,
                        row.get::<_, u8>(4)?,
                    ))
                })
                .map_err(database_error)?;
            for row in rows {
                let (id, display_name, environment, currency, currency_scale) =
                    row.map_err(database_error)?;
                let id = TradingAccountId::try_new(id).map_err(|error| error.to_string())?;
                let account = TradingAccount {
                    id: id.clone(),
                    display_name,
                    environment: parse_environment(&environment)?,
                    currency,
                    currency_scale,
                };
                account.validate().map_err(|error| error.to_string())?;
                accounts.insert(id, account);
            }
        }
        let instruments = self.load_instruments()?;
        let orders = self.load_orders()?;
        let order_events = self.load_events()?;
        let fills = self.load_fills()?;
        let positions = self.load_positions()?;
        let risk_profiles = self.load_risk_profiles()?;
        let risk_locks = self.load_risk_locks()?;
        Ok(StoredState {
            revision,
            next_sequence,
            accounts,
            instruments,
            orders,
            order_events,
            fills,
            positions,
            risk_profiles,
            risk_locks,
        })
    }

    fn metadata(&self, key: &str) -> Result<u64, String> {
        self.connection
            .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(database_error)
            .and_then(sqlite_u64)
    }

    pub(super) fn set_revision(&self, revision: u64) -> Result<(), String> {
        let revision = sqlite_i64(revision)?;
        self.connection
            .execute(
                "UPDATE metadata SET value = ?1 WHERE key = 'revision'",
                [revision],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn put_account(&self, account: &TradingAccount) -> Result<(), String> {
        account.validate().map_err(|error| error.to_string())?;
        self.connection
            .execute(
                "INSERT INTO accounts(id, display_name, environment, currency, currency_scale)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,
                    environment=excluded.environment, currency=excluded.currency,
                    currency_scale=excluded.currency_scale",
                params![
                    account.id.as_str(),
                    account.display_name,
                    account.environment.as_str(),
                    account.currency,
                    account.currency_scale,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn put_instrument(&self, instrument: &TradingInstrument) -> Result<(), String> {
        let contract = encode_contract(&instrument.contract)?;
        self.connection
            .execute(
                "INSERT INTO instruments(id, price_scale, quantity_scale, contract_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET price_scale=excluded.price_scale,
                    quantity_scale=excluded.quantity_scale, contract_json=excluded.contract_json",
                params![
                    instrument.instrument_id.as_str(),
                    instrument.price_scale,
                    instrument.quantity_scale,
                    contract,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn put_risk_profile(&self, profile: &RiskProfile) -> Result<(), String> {
        profile.validate()?;
        self.connection
            .execute(
                "INSERT INTO risk_profiles(account_id, profile_id, version, daily_loss_units,
                    daily_loss_scale, trailing_units, trailing_scale, trailing_mode,
                    max_contracts_units, max_contracts_scale, consistency_percent,
                    restricted_until_unix_nanos, enabled, session_start_unix_nanos)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(account_id) DO UPDATE SET profile_id=excluded.profile_id,
                    version=excluded.version, daily_loss_units=excluded.daily_loss_units,
                    daily_loss_scale=excluded.daily_loss_scale, trailing_units=excluded.trailing_units,
                    trailing_scale=excluded.trailing_scale, trailing_mode=excluded.trailing_mode,
                    max_contracts_units=excluded.max_contracts_units,
                    max_contracts_scale=excluded.max_contracts_scale,
                    consistency_percent=excluded.consistency_percent,
                    restricted_until_unix_nanos=excluded.restricted_until_unix_nanos,
                    enabled=excluded.enabled,
                    session_start_unix_nanos=excluded.session_start_unix_nanos",
                params![
                    profile.account_id.as_str(),
                    profile.profile_id,
                    profile.version,
                    profile.daily_loss_limit.units(),
                    profile.daily_loss_limit.scale(),
                    profile.trailing_drawdown.map(FixedPoint::units),
                    profile.trailing_drawdown.map(FixedPoint::scale),
                    profile.trailing_mode.as_str(),
                    profile.max_contracts.units(),
                    profile.max_contracts.scale(),
                    profile.consistency_max_single_trade_percent,
                    profile.restricted_until_unix_nanos,
                    i64::from(profile.enabled),
                    profile.session_start_unix_nanos,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn put_risk_lock(&self, lock: &RiskLock) -> Result<(), String> {
        lock.validate()?;
        self.connection
            .execute(
                "INSERT INTO risk_locks(account_id, reason, locked_at_unix_nanos, profile_id,
                    profile_version) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(account_id) DO UPDATE SET reason=excluded.reason,
                    locked_at_unix_nanos=excluded.locked_at_unix_nanos,
                    profile_id=excluded.profile_id, profile_version=excluded.profile_version",
                params![
                    lock.account_id.as_str(),
                    lock.reason,
                    lock.locked_at_unix_nanos,
                    lock.profile_id,
                    lock.profile_version,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn delete_risk_lock(&self, account_id: &TradingAccountId) -> Result<(), String> {
        self.connection
            .execute(
                "DELETE FROM risk_locks WHERE account_id = ?1",
                [account_id.as_str()],
            )
            .map_err(database_error)?;
        Ok(())
    }

    fn load_risk_profiles(&self) -> Result<BTreeMap<TradingAccountId, RiskProfile>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, profile_id, version, daily_loss_units, daily_loss_scale,
                    trailing_units, trailing_scale, trailing_mode, max_contracts_units,
                    max_contracts_scale, consistency_percent, restricted_until_unix_nanos, enabled,
                    session_start_unix_nanos
                 FROM risk_profiles ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, u8>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<u8>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, u8>(9)?,
                    row.get::<_, Option<u8>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                account,
                profile_id,
                version,
                daily_loss_units,
                daily_loss_scale,
                trailing_units,
                trailing_scale,
                trailing_mode,
                max_contracts_units,
                max_contracts_scale,
                consistency_percent,
                restricted_until_unix_nanos,
                enabled,
                session_start_unix_nanos,
            ) = row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            let daily_loss_limit = FixedPoint::try_new(daily_loss_units, daily_loss_scale)
                .map_err(|error| error.to_string())?;
            let trailing_drawdown = trailing_units
                .zip(trailing_scale)
                .map(|(units, scale)| FixedPoint::try_new(units, scale))
                .transpose()
                .map_err(|error| error.to_string())?;
            let max_contracts = FixedPoint::try_new(max_contracts_units, max_contracts_scale)
                .map_err(|error| error.to_string())?;
            let profile = RiskProfile {
                account_id: account_id.clone(),
                profile_id,
                version,
                daily_loss_limit,
                trailing_drawdown,
                trailing_mode: TrailingDrawdownMode::parse(&trailing_mode)?,
                max_contracts,
                consistency_max_single_trade_percent: consistency_percent,
                restricted_until_unix_nanos,
                enabled: enabled != 0,
                session_start_unix_nanos,
            };
            profile.validate()?;
            result.insert(account_id, profile);
        }
        Ok(result)
    }

    fn load_risk_locks(&self) -> Result<BTreeMap<TradingAccountId, RiskLock>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, reason, locked_at_unix_nanos, profile_id, profile_version
                 FROM risk_locks ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account, reason, locked_at_unix_nanos, profile_id, profile_version) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            let lock = RiskLock {
                account_id: account_id.clone(),
                reason,
                locked_at_unix_nanos,
                profile_id,
                profile_version,
            };
            lock.validate()?;
            result.insert(account_id, lock);
        }
        Ok(result)
    }

    pub(super) fn insert_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        insert_order_row(&transaction, order)?;
        insert_event_row(&transaction, event)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn insert_fill(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        fill: &Fill,
        position: &Position,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE orders SET status = 'filled' WHERE id = ?1 AND status = 'working'",
                [order.id.as_str()],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, event)?;
        transaction
            .execute(
                "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                    price_scale, quantity_units, quantity_scale, execution_unix_nanos, provenance_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
        upsert_position(&transaction, position)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn cancel_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE orders SET status = 'cancelled' WHERE id = ?1 AND status = 'working'",
                [order.id.as_str()],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, event)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn modify_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE orders SET time_in_force = ?1, limit_units = ?2, limit_scale = ?3,
                    stop_units = ?4, stop_scale = ?5, provenance_json = ?6
                 WHERE id = ?7 AND status = 'working'",
                params![
                    order.time_in_force.as_str(),
                    order.limit_price.map(FixedPoint::units),
                    order.limit_price.map(FixedPoint::scale),
                    order.stop_price.map(FixedPoint::units),
                    order.stop_price.map(FixedPoint::scale),
                    encode_provenance(&order.provenance),
                    order.id.as_str(),
                ],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, event)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn update_position(&mut self, position: &Position) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        upsert_position(&transaction, position)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn put_user_record(&self, record: &UserRecord) -> Result<(), String> {
        let revision = sqlite_i64(record.revision)?;
        self.connection
            .execute(
                "INSERT INTO user_records(kind, id, revision, updated_unix_nanos, json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(kind, id) DO UPDATE SET revision=excluded.revision,
                    updated_unix_nanos=excluded.updated_unix_nanos, json=excluded.json
                 WHERE excluded.revision > user_records.revision",
                params![
                    record.kind.as_str(),
                    record.id,
                    revision,
                    record.updated_unix_nanos,
                    record.json,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn enforce_retention(&mut self) -> Result<(), String> {
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
        let working_orders = transaction
            .query_row(
                "SELECT COUNT(*) FROM orders WHERE status = 'working'",
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
                            SELECT id FROM orders WHERE status != 'working'
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
                    SELECT id FROM orders WHERE status != 'working'
                    ORDER BY submitted_unix_nanos DESC LIMIT -1 OFFSET ?1
                 )",
                [retained_closed_orders],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn export(&self, directory: &Path) -> Result<(), String> {
        fs::create_dir_all(directory)
            .map_err(|error| format!("trading export directory could not be created: {error}"))?;
        let executions = self.export_rows(
            "SELECT id, order_id, account_id, instrument_id, side, price_units, price_scale,
                    quantity_units, quantity_scale, execution_unix_nanos, provenance_json
             FROM fills ORDER BY execution_unix_nanos",
            |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "order_id": row.get::<_, String>(1)?,
                    "account_id": row.get::<_, String>(2)?,
                    "instrument_id": row.get::<_, String>(3)?,
                    "side": row.get::<_, String>(4)?,
                    "price": { "units": row.get::<_, i64>(5)?, "scale": row.get::<_, u8>(6)? },
                    "quantity": { "units": row.get::<_, i64>(7)?, "scale": row.get::<_, u8>(8)? },
                    "execution_unix_nanos": row.get::<_, i64>(9)?,
                    "provenance": serde_json::from_str::<Value>(&row.get::<_, String>(10)?)
                        .unwrap_or(Value::Null),
                }))
            },
        )?;
        let records = self.export_rows(
            "SELECT kind, id, revision, updated_unix_nanos, json FROM user_records
             ORDER BY kind, updated_unix_nanos",
            |row| {
                Ok(json!({
                    "kind": row.get::<_, String>(0)?,
                    "id": row.get::<_, String>(1)?,
                    "revision": row.get::<_, i64>(2)?,
                    "updated_unix_nanos": row.get::<_, i64>(3)?,
                    "value": serde_json::from_str::<Value>(&row.get::<_, String>(4)?)
                        .unwrap_or(Value::Null),
                }))
            },
        )?;
        write_json(directory.join("executions.json").as_path(), &executions)?;
        write_json(directory.join("user_records.json").as_path(), &records)?;
        let mut csv = String::from(
            "id,order_id,account_id,instrument_id,side,price_units,price_scale,quantity_units,quantity_scale,execution_unix_nanos\n",
        );
        for row in &executions {
            let price = &row["price"];
            let quantity = &row["quantity"];
            writeln!(
                csv,
                "{},{},{},{},{},{},{},{},{},{}",
                csv_field(row["id"].as_str().unwrap_or_default()),
                csv_field(row["order_id"].as_str().unwrap_or_default()),
                csv_field(row["account_id"].as_str().unwrap_or_default()),
                csv_field(row["instrument_id"].as_str().unwrap_or_default()),
                csv_field(row["side"].as_str().unwrap_or_default()),
                price["units"],
                price["scale"],
                quantity["units"],
                quantity["scale"],
                row["execution_unix_nanos"],
            )
            .map_err(|_| "trading CSV export could not be encoded".to_string())?;
        }
        write_atomic(directory.join("executions.csv").as_path(), csv.as_bytes())
    }

    fn export_rows(
        &self,
        sql: &str,
        map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
    ) -> Result<Vec<Value>, String> {
        let mut statement = self.connection.prepare(sql).map_err(database_error)?;
        statement
            .query_map([], map)
            .map_err(database_error)?
            .map(|row| row.map_err(database_error))
            .collect()
    }

    fn load_instruments(&self) -> Result<BTreeMap<InstrumentId, TradingInstrument>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare("SELECT id, price_scale, quantity_scale, contract_json FROM instruments")
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u8>(1)?,
                    row.get::<_, u8>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (id, price_scale, quantity_scale, contract) = row.map_err(database_error)?;
            let instrument_id = InstrumentId::try_new(id).map_err(|error| error.to_string())?;
            output.insert(
                instrument_id.clone(),
                TradingInstrument {
                    instrument_id,
                    price_scale,
                    quantity_scale,
                    contract: decode_contract(&contract)?,
                },
            );
        }
        Ok(output)
    }

    fn load_orders(&self) -> Result<BTreeMap<OrderId, Order>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, client_order_id, account_id, instrument_id, side, order_type,
                    time_in_force, quantity_units, quantity_scale, limit_units, limit_scale,
                    stop_units, stop_scale, status, submitted_unix_nanos, provenance_json
             FROM orders ORDER BY submitted_unix_nanos",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<u8>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<u8>>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, String>(15)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                id,
                client,
                account,
                instrument,
                side,
                order_type,
                tif,
                quantity_units,
                quantity_scale,
                limit_units,
                limit_scale,
                stop_units,
                stop_scale,
                status,
                submitted,
                provenance,
            ) = row.map_err(database_error)?;
            let id = OrderId::try_new(id).map_err(|error| error.to_string())?;
            let order = Order {
                id: id.clone(),
                client_order_id: ClientOrderId::try_new(client)
                    .map_err(|error| error.to_string())?,
                account_id: TradingAccountId::try_new(account)
                    .map_err(|error| error.to_string())?,
                instrument_id: InstrumentId::try_new(instrument)
                    .map_err(|error| error.to_string())?,
                side: parse_side(&side)?,
                order_type: parse_order_type(&order_type)?,
                time_in_force: parse_time_in_force(&tif)?,
                quantity: fixed(quantity_units, quantity_scale)?,
                limit_price: optional_fixed(limit_units, limit_scale)?,
                stop_price: optional_fixed(stop_units, stop_scale)?,
                status: parse_status(&status)?,
                submitted_unix_nanos: submitted,
                provenance: decode_provenance(&provenance)?,
            };
            order.validate().map_err(|error| error.to_string())?;
            output.insert(id, order);
        }
        Ok(output)
    }

    fn load_events(&self) -> Result<VecDeque<OrderEvent>, String> {
        let mut output = VecDeque::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, order_id, sequence, kind, event_unix_nanos, detail, provenance_json
             FROM order_events ORDER BY sequence",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (id, order, sequence, kind, time, detail, provenance) =
                row.map_err(database_error)?;
            let event = OrderEvent {
                id: OrderEventId::try_new(id).map_err(|error| error.to_string())?,
                order_id: OrderId::try_new(order).map_err(|error| error.to_string())?,
                sequence: sqlite_u64(sequence)?,
                kind: parse_event_kind(&kind)?,
                event_unix_nanos: time,
                detail,
                provenance: decode_provenance(&provenance)?,
            };
            event.validate().map_err(|error| error.to_string())?;
            output.push_back(event);
        }
        Ok(output)
    }

    fn load_fills(&self) -> Result<VecDeque<Fill>, String> {
        let mut output = VecDeque::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, order_id, account_id, instrument_id, side, price_units, price_scale,
                    quantity_units, quantity_scale, execution_unix_nanos, provenance_json
             FROM fills ORDER BY execution_unix_nanos",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, u8>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                id,
                order,
                account,
                instrument,
                side,
                price_units,
                price_scale,
                quantity_units,
                quantity_scale,
                time,
                provenance,
            ) = row.map_err(database_error)?;
            let fill = Fill {
                id: FillId::try_new(id).map_err(|error| error.to_string())?,
                order_id: OrderId::try_new(order).map_err(|error| error.to_string())?,
                account_id: TradingAccountId::try_new(account)
                    .map_err(|error| error.to_string())?,
                instrument_id: InstrumentId::try_new(instrument)
                    .map_err(|error| error.to_string())?,
                side: parse_side(&side)?,
                price: fixed(price_units, price_scale)?,
                quantity: fixed(quantity_units, quantity_scale)?,
                execution_unix_nanos: time,
                provenance: decode_provenance(&provenance)?,
            };
            fill.validate().map_err(|error| error.to_string())?;
            output.push_back(fill);
        }
        Ok(output)
    }

    fn load_positions(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, InstrumentId), Position>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self.connection.prepare(
            "SELECT account_id, instrument_id, net_units, net_scale, average_units, average_scale,
                    realized_units, realized_scale, unrealized_units, unrealized_scale,
                    last_fill_unix_nanos FROM positions",
        ).map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u8>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, u8>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, u8>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                account,
                instrument,
                net_units,
                net_scale,
                average_units,
                average_scale,
                realized_units,
                realized_scale,
                unrealized_units,
                unrealized_scale,
                time,
            ) = row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            let instrument_id =
                InstrumentId::try_new(instrument).map_err(|error| error.to_string())?;
            output.insert(
                (account_id.clone(), instrument_id.clone()),
                Position {
                    account_id,
                    instrument_id,
                    net_quantity: fixed(net_units, net_scale)?,
                    average_entry_price: optional_fixed(average_units, average_scale)?,
                    realized_pnl: fixed(realized_units, realized_scale)?,
                    unrealized_pnl: fixed(unrealized_units, unrealized_scale)?,
                    last_fill_unix_nanos: time,
                },
            );
        }
        Ok(output)
    }
}

fn insert_order_row(transaction: &Transaction<'_>, order: &Order) -> Result<(), String> {
    let (limit_units, limit_scale) = split_optional(order.limit_price);
    let (stop_units, stop_scale) = split_optional(order.stop_price);
    transaction
        .execute(
            "INSERT INTO orders(id, client_order_id, account_id, instrument_id, side, order_type,
            time_in_force, quantity_units, quantity_scale, limit_units, limit_scale, stop_units,
            stop_scale, status, submitted_unix_nanos, provenance_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                order.id.as_str(),
                order.client_order_id.as_str(),
                order.account_id.as_str(),
                order.instrument_id.as_str(),
                order.side.as_str(),
                order.order_type.as_str(),
                order.time_in_force.as_str(),
                order.quantity.units(),
                order.quantity.scale(),
                limit_units,
                limit_scale,
                stop_units,
                stop_scale,
                order.status.as_str(),
                order.submitted_unix_nanos,
                encode_provenance(&order.provenance)
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn insert_event_row(transaction: &Transaction<'_>, event: &OrderEvent) -> Result<(), String> {
    let sequence = sqlite_i64(event.sequence)?;
    transaction.execute(
        "INSERT INTO order_events(id, order_id, sequence, kind, event_unix_nanos, detail, provenance_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![event.id.as_str(), event.order_id.as_str(), sequence, event.kind.as_str(),
            event.event_unix_nanos, event.detail, encode_provenance(&event.provenance)],
    ).map_err(database_error)?;
    Ok(())
}

fn upsert_position(transaction: &Transaction<'_>, position: &Position) -> Result<(), String> {
    let (average_units, average_scale) = split_optional(position.average_entry_price);
    transaction
        .execute(
            "INSERT INTO positions(account_id, instrument_id, net_units, net_scale, average_units,
            average_scale, realized_units, realized_scale, unrealized_units, unrealized_scale,
            last_fill_unix_nanos) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(account_id, instrument_id) DO UPDATE SET net_units=excluded.net_units,
            net_scale=excluded.net_scale, average_units=excluded.average_units,
            average_scale=excluded.average_scale, realized_units=excluded.realized_units,
            realized_scale=excluded.realized_scale, unrealized_units=excluded.unrealized_units,
            unrealized_scale=excluded.unrealized_scale,
            last_fill_unix_nanos=excluded.last_fill_unix_nanos",
            params![
                position.account_id.as_str(),
                position.instrument_id.as_str(),
                position.net_quantity.units(),
                position.net_quantity.scale(),
                average_units,
                average_scale,
                position.realized_pnl.units(),
                position.realized_pnl.scale(),
                position.unrealized_pnl.units(),
                position.unrealized_pnl.scale(),
                position.last_fill_unix_nanos
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn update_next_sequence(transaction: &Transaction<'_>, next: u64) -> Result<(), String> {
    let next = sqlite_i64(next)?;
    transaction
        .execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'next_sequence'",
            [next],
        )
        .map_err(database_error)?;
    Ok(())
}

fn encode_provenance(value: &TradingProvenance) -> String {
    json!({"venue_id": value.venue_id, "provider_id": value.provider_id,
        "session_generation": value.session_generation, "source_sequence": value.source_sequence,
        "observed_unix_nanos": value.observed_unix_nanos})
    .to_string()
}

fn decode_provenance(raw: &str) -> Result<TradingProvenance, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|_| "stored provenance is invalid".to_string())?;
    let provenance = TradingProvenance {
        venue_id: json_string(&value, "venue_id")?,
        provider_id: json_string(&value, "provider_id")?,
        session_generation: json_u64(&value, "session_generation")?,
        source_sequence: json_u64(&value, "source_sequence")?,
        observed_unix_nanos: json_i64(&value, "observed_unix_nanos")?,
    };
    provenance.validate().map_err(|error| error.to_string())?;
    Ok(provenance)
}

fn encode_contract(contract: &ContractMetadata) -> Result<String, String> {
    contract.validate().map_err(|error| error.to_string())?;
    let decimal = |value: Option<InstrumentDecimal>| {
        value.map(|value| {
            json!({
                "units": value.units(), "scale": value.scale()
            })
        })
    };
    let date = |value: Option<ContractDate>| {
        value.map(|value| {
            json!({
                "year": value.year, "month": value.month, "day": value.day
            })
        })
    };
    Ok(json!({"tick_size": decimal(contract.tick_size), "point_value": decimal(contract.point_value),
        "currency": contract.currency, "expiry": date(contract.expiry),
        "first_notice": date(contract.first_notice), "last_trade": date(contract.last_trade),
        "session_hours": contract.session_hours.iter().map(|value| json!({"weekday": value.weekday,
            "open_seconds": value.open_seconds, "close_seconds": value.close_seconds,
            "timezone": value.timezone})).collect::<Vec<_>>(),
        "provenance": {"provider_id": contract.provenance.provider_id,
            "provider_symbol": contract.provenance.provider_symbol,
            "session_generation": contract.provenance.session_generation}}).to_string())
}

fn decode_contract(raw: &str) -> Result<ContractMetadata, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|_| "stored contract metadata is invalid".to_string())?;
    let provenance = value
        .get("provenance")
        .ok_or_else(|| "stored contract provenance is missing".to_string())?;
    let sessions = value
        .get("session_hours")
        .and_then(Value::as_array)
        .ok_or_else(|| "stored session hours are invalid".to_string())?
        .iter()
        .map(|session| {
            Ok(SessionHours {
                weekday: json_u64(session, "weekday")?
                    .try_into()
                    .map_err(|_| "weekday is invalid".to_string())?,
                open_seconds: json_u64(session, "open_seconds")?
                    .try_into()
                    .map_err(|_| "open time is invalid".to_string())?,
                close_seconds: json_u64(session, "close_seconds")?
                    .try_into()
                    .map_err(|_| "close time is invalid".to_string())?,
                timezone: json_string(session, "timezone")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let contract = ContractMetadata {
        tick_size: decode_decimal(value.get("tick_size"))?,
        point_value: decode_decimal(value.get("point_value"))?,
        currency: json_string(&value, "currency")?,
        expiry: decode_date(value.get("expiry"))?,
        first_notice: decode_date(value.get("first_notice"))?,
        last_trade: decode_date(value.get("last_trade"))?,
        session_hours: sessions,
        provenance: InstrumentMetadataProvenance {
            provider_id: json_string(provenance, "provider_id")?,
            provider_symbol: json_string(provenance, "provider_symbol")?,
            session_generation: json_u64(provenance, "session_generation")?,
        },
    };
    contract.validate().map_err(|error| error.to_string())?;
    Ok(contract)
}

fn decode_decimal(value: Option<&Value>) -> Result<Option<InstrumentDecimal>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    Ok(Some(
        InstrumentDecimal::try_new(
            json_i64(value, "units")?,
            json_u64(value, "scale")?
                .try_into()
                .map_err(|_| "decimal scale is invalid".to_string())?,
        )
        .map_err(|error| error.to_string())?,
    ))
}

fn decode_date(value: Option<&Value>) -> Result<Option<ContractDate>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let date = ContractDate {
        year: json_u64(value, "year")?
            .try_into()
            .map_err(|_| "year is invalid".to_string())?,
        month: json_u64(value, "month")?
            .try_into()
            .map_err(|_| "month is invalid".to_string())?,
        day: json_u64(value, "day")?
            .try_into()
            .map_err(|_| "day is invalid".to_string())?,
    };
    date.validate().map_err(|error| error.to_string())?;
    Ok(Some(date))
}

fn fixed(units: i64, scale: u8) -> Result<FixedPoint, String> {
    FixedPoint::try_new(units, scale).map_err(|error| error.to_string())
}

fn optional_fixed(units: Option<i64>, scale: Option<u8>) -> Result<Option<FixedPoint>, String> {
    match (units, scale) {
        (None, None) => Ok(None),
        (Some(units), Some(scale)) => fixed(units, scale).map(Some),
        _ => Err("stored optional fixed-point value is incomplete".to_string()),
    }
}

fn split_optional(value: Option<FixedPoint>) -> (Option<i64>, Option<u8>) {
    (value.map(FixedPoint::units), value.map(FixedPoint::scale))
}

fn parse_environment(value: &str) -> Result<AccountEnvironment, String> {
    match value {
        "simulated" => Ok(AccountEnvironment::Simulated),
        "live" => Ok(AccountEnvironment::Live),
        _ => Err("stored account environment is invalid".to_string()),
    }
}
fn parse_side(value: &str) -> Result<OrderSide, String> {
    match value {
        "buy" => Ok(OrderSide::Buy),
        "sell" => Ok(OrderSide::Sell),
        _ => Err("stored order side is invalid".to_string()),
    }
}
fn parse_order_type(value: &str) -> Result<OrderType, String> {
    match value {
        "market" => Ok(OrderType::Market),
        "limit" => Ok(OrderType::Limit),
        "stop" => Ok(OrderType::Stop),
        "stop_limit" => Ok(OrderType::StopLimit),
        _ => Err("stored order type is invalid".to_string()),
    }
}
fn parse_time_in_force(value: &str) -> Result<TimeInForce, String> {
    match value {
        "day" => Ok(TimeInForce::Day),
        "gtc" => Ok(TimeInForce::GoodTillCancelled),
        "ioc" => Ok(TimeInForce::ImmediateOrCancel),
        "fok" => Ok(TimeInForce::FillOrKill),
        _ => Err("stored time in force is invalid".to_string()),
    }
}
fn parse_status(value: &str) -> Result<OrderStatus, String> {
    match value {
        "pending" => Ok(OrderStatus::Pending),
        "working" => Ok(OrderStatus::Working),
        "filled" => Ok(OrderStatus::Filled),
        "cancelled" => Ok(OrderStatus::Cancelled),
        "rejected" => Ok(OrderStatus::Rejected),
        _ => Err("stored order status is invalid".to_string()),
    }
}
fn parse_event_kind(value: &str) -> Result<OrderEventKind, String> {
    match value {
        "accepted" => Ok(OrderEventKind::Accepted),
        "modified" => Ok(OrderEventKind::Modified),
        "filled" => Ok(OrderEventKind::Filled),
        "cancelled" => Ok(OrderEventKind::Cancelled),
        "rejected" => Ok(OrderEventKind::Rejected),
        _ => Err("stored order event kind is invalid".to_string()),
    }
}

fn json_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("stored {field} is invalid"))
}
fn json_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("stored {field} is invalid"))
}
fn json_i64(value: &Value, field: &str) -> Result<i64, String> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("stored {field} is invalid"))
}
fn usize_to_i64(value: usize) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "retention limit exceeds SQLite range".to_string())
}
fn sqlite_i64(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "trading sequence exceeds SQLite range".to_string())
}
fn sqlite_u64(value: i64) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| "stored trading sequence is negative".to_string())
}
fn database_error(error: rusqlite::Error) -> String {
    let error: Box<dyn std::error::Error> = Box::new(error);
    format!("trading database operation failed: {error}")
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn write_json(path: &Path, values: &[Value]) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(values)
        .map_err(|_| "trading export could not be encoded".to_string())?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)
        .map_err(|error| format!("trading export could not be written: {error}"))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("old trading export could not be replaced: {error}"))?;
    }
    fs::rename(&temporary, path)
        .map_err(|error| format!("trading export could not be committed: {error}"))
}
