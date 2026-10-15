mod export;
mod fills;
mod groups;
mod ledger;
mod orders;
mod retention;
mod risk;
mod schema;
mod user_records;
#[cfg(test)]
use export::write_atomic;
#[cfg(test)]
use orders::decode_contract;
pub use schema::SCHEMA_VERSION;
pub(crate) use schema::preflight_existing_database;
#[cfg(test)]
use schema::*;
use {
    ledger::{fixed, optional_fixed, split_optional, upsert_position},
    orders::{
        decode_provenance, encode_contract, encode_provenance, insert_event_row, insert_order_row,
        parse_side, update_next_sequence,
    },
    risk::{upsert_discipline_state, upsert_risk_rule_state, upsert_risk_trade_cycle},
    schema::sweep_stale_preflight_directories,
};

use super::{
    BracketStrategyTemplate, BracketTarget, BreakEvenRule, DisciplineState,
    EconomicEventRiskAction, EconomicEventRiskImportance, EconomicEventRiskRule, ManagedBracket,
    ManagedBracketStatus, ProtectiveOrder, ProtectiveOrderRole, RiskLock, RiskProfile,
    RiskRuleState, RiskTradeCycleState, SessionBias, SessionChecklistItem, SessionPlan,
    SessionPlanLevel, TradeCopierConfig, TradeCopierTarget, TradingInstrument, TradingRetention,
    TrailingDrawdownMode, TrailingStopRule, UserRecord,
};
use aeris_instruments::{
    ContractDate, ContractMetadata, InstrumentClass, InstrumentDecimal, InstrumentId,
    InstrumentMetadataProvenance, PriceGrid, SessionHours,
};
use aeris_trading::{
    AccountEnvironment, BrokerPosition, ClientOrderId, Fill, FillId, FixedPoint, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position,
    TimeInForce, TradingAccount, TradingAccountId, TradingProvenance,
};
use rusqlite::{Connection, OpenFlags, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt::{self, Write as _},
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

/// A failure opening or preparing the authoritative trading database.
#[derive(Debug)]
pub enum StoreError {
    /// SQLite reported that the existing database does not pass `quick_check`.
    Corrupt {
        path: PathBuf,
        newest_backup: Option<PathBuf>,
        detail: String,
    },
    /// The database was written by a version of Aeris with a newer schema.
    NewerSchema {
        found: u32,
        supported: u32,
    },
    Other(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt {
                path,
                newest_backup,
                detail,
            } => {
                write!(formatter, "trading database {} is corrupt", path.display())?;
                if let Some(backup) = newest_backup {
                    write!(formatter, "; newest backup: {}", backup.display())?;
                }
                write!(formatter, ": {detail}")
            }
            Self::NewerSchema { found, supported } => write!(
                formatter,
                "trading database schema {found} is newer than supported schema {supported}"
            ),
            Self::Other(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<String> for StoreError {
    fn from(detail: String) -> Self {
        Self::Other(detail)
    }
}

fn newest_backup(database_path: &Path) -> Option<PathBuf> {
    let backups = database_path.parent()?.join("backups");
    let mut candidates = fs::read_dir(backups)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("trading-v") && name.ends_with(".sqlite3"))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| backup_unix(path).unwrap_or(0));
    candidates.pop()
}

fn backup_unix(path: &Path) -> Option<u128> {
    path.file_stem()?.to_str()?.rsplit_once('-')?.1.parse().ok()
}

fn corrupt_error(path: &Path, detail: impl Into<String>) -> StoreError {
    StoreError::Corrupt {
        path: path.to_path_buf(),
        newest_backup: newest_backup(path),
        detail: detail.into(),
    }
}

pub(super) struct StoredState {
    pub revision: u64,
    pub next_sequence: u64,
    pub accounts: BTreeMap<TradingAccountId, TradingAccount>,
    pub instruments: BTreeMap<InstrumentId, TradingInstrument>,
    pub orders: crate::book::OrderBook,
    pub order_events: VecDeque<OrderEvent>,
    pub fills: VecDeque<Fill>,
    pub completed_trade_pnl: BTreeMap<FillId, FixedPoint>,
    pub fill_realized_pnl: BTreeMap<FillId, FixedPoint>,
    pub trade_pnl_cycles: BTreeMap<(TradingAccountId, InstrumentId), FixedPoint>,
    pub positions: BTreeMap<(TradingAccountId, InstrumentId), Position>,
    pub broker_positions: BTreeMap<(TradingAccountId, String), BrokerPosition>,
    /// The broker position each retained broker fill belongs to.
    pub broker_fill_positions: BTreeMap<FillId, String>,
    /// The latest relay session generation ever attached.
    pub venue_generation: u64,
    /// The last balance each broker account reported.
    pub broker_balances: BTreeMap<TradingAccountId, FixedPoint>,
    pub risk_profiles: BTreeMap<TradingAccountId, RiskProfile>,
    pub risk_locks: BTreeMap<TradingAccountId, RiskLock>,
    pub risk_rule_states: BTreeMap<TradingAccountId, RiskRuleState>,
    pub risk_trade_cycles: BTreeMap<(TradingAccountId, InstrumentId), RiskTradeCycleState>,
    pub session_plans: BTreeMap<TradingAccountId, SessionPlan>,
    pub discipline_states: BTreeMap<TradingAccountId, DisciplineState>,
    pub trade_copiers: BTreeMap<TradingAccountId, TradeCopierConfig>,
    pub strategy_templates: BTreeMap<String, BracketStrategyTemplate>,
    pub managed_brackets: BTreeMap<String, ManagedBracket>,
    pub protective_orders: BTreeMap<ClientOrderId, ProtectiveOrder>,
}

pub(super) struct TradingStore {
    connection: Connection,
    retention: TradingRetention,
}

/// A single owner step's SQLite transaction. Dropping without `commit` rolls back.
/// M4.10 will collect the step's facts before opening this transaction.
pub(crate) struct StepTx<'a>(Transaction<'a>);

impl<'a> StepTx<'a> {
    pub(crate) fn begin(store: &'a mut TradingStore) -> rusqlite::Result<Self> {
        store.connection.transaction().map(Self)
    }

    pub(crate) fn commit(self) -> rusqlite::Result<()> {
        self.0.commit()
    }
}

impl<'a> std::ops::Deref for StepTx<'a> {
    type Target = Transaction<'a>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One broker deal and the owner order state it leaves behind.
pub(super) struct BrokerFillRecord<'a> {
    pub fill: &'a Fill,
    /// The order after this fill.
    pub order: &'a Order,
    pub event: &'a OrderEvent,
    /// A broker-originated order first seen through this deal, with its broker order id.
    pub mirror: Option<(&'a Order, &'a str)>,
    pub broker_deal_id: &'a str,
    pub broker_position_id: &'a str,
    /// What a closing deal realized, in the account currency.
    pub realized_pnl: Option<FixedPoint>,
}

#[derive(Clone, Copy)]
pub(super) struct FillPolicyPersistence<'a> {
    pub rule_state: Option<&'a RiskRuleState>,
    pub trade_cycle: &'a RiskTradeCycleState,
    pub discipline_state: &'a DisciplineState,
    pub completed_trade_pnl: Option<FixedPoint>,
    pub fill_realized_pnl: Option<FixedPoint>,
    pub trade_pnl_cycle: Option<FixedPoint>,
}

impl TradingStore {
    #[cfg(test)]
    pub(super) fn crash_during_commit(&self, nth: usize) {
        let mut remaining = nth;
        self.connection
            .commit_hook(Some(move || {
                remaining = remaining.saturating_sub(1);
                remaining == 0
            }))
            .expect("install replay-only SQLite commit hook");
    }

    pub(super) fn open(path: &Path, retention: TradingRetention) -> Result<Self, StoreError> {
        let existed = path != Path::new(":memory:") && path.exists();
        if path != Path::new(":memory:") {
            sweep_stale_preflight_directories(path);
        }
        if existed {
            preflight_existing_database(path)?;
        }
        if path != Path::new(":memory:") {
            let parent = path.parent().ok_or_else(|| {
                StoreError::Other("trading database has no parent directory".to_string())
            })?;
            fs::create_dir_all(parent).map_err(|error| {
                StoreError::Other(format!(
                    "trading data directory could not be created: {error}"
                ))
            })?;
        }
        let connection = Connection::open(path).map_err(|error| {
            StoreError::Other(format!("trading database could not be opened: {error}"))
        })?;
        connection
            .busy_timeout(std::time::Duration::from_millis(5_000))
            .map_err(|error| {
                StoreError::Other(format!("trading database busy timeout failed: {error}"))
            })?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .and_then(|()| connection.pragma_update(None, "synchronous", "FULL"))
            .and_then(|()| connection.pragma_update(None, "foreign_keys", "ON"))
            .map_err(|error| {
                StoreError::Other(format!("trading database safety settings failed: {error}"))
            })?;
        let mut store = Self {
            connection,
            retention,
        };
        store.migrate(existed)?;
        store
            .ensure_default_strategy_templates()
            .map_err(StoreError::Other)?;
        store.enforce_retention().map_err(StoreError::Other)?;
        Ok(store)
    }
}

impl TradingStore {
    pub(super) fn checkpoint_for_shutdown(&self) -> Result<(), String> {
        let (busy, _, _) = self
            .connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            })
            .map_err(|error| format!("trading WAL truncate checkpoint failed: {error}"))?;
        if busy == 0 {
            Ok(())
        } else {
            Err("trading WAL truncate checkpoint could not acquire the database lock".to_string())
        }
    }
}

impl TradingStore {
    pub(super) fn load_state(&mut self) -> Result<StoredState, String> {
        let revision = self.metadata("revision")?;
        let next_sequence = self.metadata("next_sequence")?;
        let accounts = self.load_accounts()?;
        let instruments = self.load_instruments()?;
        let orders = self.load_orders()?;
        let order_events = self.load_events()?;
        let fills = self.load_fills()?;
        let completed_trade_pnl = self.load_completed_trade_pnl()?;
        let fill_realized_pnl = self.load_fill_realized_pnl()?;
        let trade_pnl_cycles = self.load_trade_pnl_cycles()?;
        let positions = self.load_positions()?;
        let broker_positions = self.load_broker_positions()?;
        let broker_order_ids = self.load_broker_order_ids()?;
        let broker_fill_positions = self.load_broker_fill_positions()?;
        let venue_generation = self.metadata("venue_generation")?;
        let broker_balances = self.load_broker_balances()?;
        let risk_profiles = self.load_risk_profiles()?;
        let risk_locks = self.load_risk_locks()?;
        let risk_rule_states = self.load_risk_rule_states()?;
        let risk_trade_cycles = self.load_risk_trade_cycles()?;
        let session_plans = self.load_session_plans()?;
        let discipline_states = self.load_discipline_states()?;
        let trade_copiers = self.load_trade_copiers()?;
        let strategy_templates = self.load_strategy_templates()?;
        let managed_brackets = self.load_managed_brackets()?;
        let protective_orders = self.load_protective_orders()?;
        Ok(StoredState {
            revision,
            next_sequence,
            accounts,
            instruments,
            orders: crate::book::OrderBook::from_stored(orders, broker_order_ids),
            order_events,
            fills,
            completed_trade_pnl,
            fill_realized_pnl,
            trade_pnl_cycles,
            positions,
            broker_positions,
            broker_fill_positions,
            venue_generation,
            broker_balances,
            risk_profiles,
            risk_locks,
            risk_rule_states,
            risk_trade_cycles,
            session_plans,
            discipline_states,
            trade_copiers,
            strategy_templates,
            managed_brackets,
            protective_orders,
        })
    }
}

impl TradingStore {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash as _, Hasher as _};
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_database(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aeris-trading-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("fixture root");
        root.join("trading.sqlite3")
    }

    #[test]
    fn open_sweeps_only_owned_stale_preflight_copies() {
        let path = temporary_database("stale-preflight");
        let parent = path.parent().expect("fixture parent");
        let stale = parent.join(".trading.sqlite3.preflight-123-456");
        fs::create_dir(&stale).expect("stale scratch");
        fs::write(stale.join("trading.sqlite3"), b"old copy").expect("stale database copy");
        fs::write(stale.join("trading.sqlite3-wal"), b"old WAL").expect("stale WAL copy");
        let unrelated = parent.join(".trading.sqlite3.preflight-not-owned");
        fs::create_dir(&unrelated).expect("unrelated directory");
        fs::write(unrelated.join("notes"), b"keep").expect("unrelated data");

        let store = TradingStore::open(&path, TradingRetention::default()).expect("new store");
        assert!(!stale.exists());
        assert_eq!(fs::read(unrelated.join("notes")).expect("notes"), b"keep");
        drop(store);
        fs::remove_dir_all(parent).expect("fixture cleanup");
    }

    #[test]
    fn unsafe_preflight_scratch_is_reported_and_preserved() {
        let path = temporary_database("unsafe-preflight");
        let scratch = path
            .parent()
            .expect("fixture parent")
            .join(".trading.sqlite3.preflight-123-456");
        fs::create_dir(&scratch).expect("stale scratch");
        let unknown = scratch.join("user-note");
        fs::write(&unknown, b"do not delete").expect("unexpected data");
        let error = schema::remove_preflight_scratch(&scratch, path.file_name().expect("name"))
            .expect_err("unknown contents cannot be removed");
        assert!(error.to_string().contains("unexpected file"));

        let store = TradingStore::open(&path, TradingRetention::default())
            .expect("cleanup failure is logged");
        assert_eq!(
            fs::read(unknown).expect("unknown preserved"),
            b"do not delete"
        );
        drop(store);
        fs::remove_dir_all(path.parent().expect("fixture parent")).expect("fixture cleanup");
    }

    fn create_schema_at(connection: &Connection, version: u32) {
        for migration in MIGRATIONS.iter().take(version as usize) {
            assert_eq!(migration.from + 1, migration.to);
            connection
                .execute_batch(migration.sql)
                .expect("historical migration");
        }
        connection
            .pragma_update(None, "user_version", version)
            .expect("historical version");
    }

    fn file_hash(path: &Path) -> u64 {
        let mut hasher = DefaultHasher::new();
        fs::read(path).expect("fixture reads").hash(&mut hasher);
        hasher.finish()
    }

    /// Deterministically creates a sanitized v18 database without relying on a checked-in binary
    /// whose SQLite page layout could vary between library versions.
    fn create_sanitized_v18_fixture(path: &Path) {
        let connection = Connection::open(path).expect("v18 fixture opens");
        create_schema_at(&connection, 18);
        connection
            .execute(
                "INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                 VALUES ('note','sanitized-v18-record',7,123456789,'{\"text\":\"fixture\"}')",
                [],
            )
            .expect("v18 fixture row");
    }

    #[test]
    #[ignore = "writes the documented sanitized fixture path for desktop validation"]
    fn generate_sanitized_v18_fixture_for_validation() {
        let path = std::env::var_os("AERIS_V18_FIXTURE_OUT")
            .map(PathBuf::from)
            .expect("set AERIS_V18_FIXTURE_OUT to the output trading.sqlite3 path");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        create_sanitized_v18_fixture(&path);
        println!("generated sanitized v18 fixture at {}", path.display());
    }

    #[test]
    fn migrations_are_contiguous_and_every_historical_version_reaches_head() {
        assert_eq!(MIGRATIONS.len(), SCHEMA_VERSION as usize);
        for (expected, migration) in (0..SCHEMA_VERSION).zip(MIGRATIONS) {
            assert_eq!(migration.from, expected);
            assert_eq!(migration.to, expected + 1);
        }
        for version in 0..SCHEMA_VERSION {
            let connection = Connection::open_in_memory().expect("in-memory database");
            create_schema_at(&connection, version);
            let mut store = TradingStore {
                connection,
                retention: TradingRetention::default(),
            };
            store.migrate(false).expect("historical schema migrates");
            let actual: u32 = store
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .expect("version");
            assert_eq!(actual, SCHEMA_VERSION, "starting at v{version}");
        }
    }

    #[test]
    fn every_row_bearing_historical_version_preserves_rows_when_migrated() {
        for version in 1..SCHEMA_VERSION {
            let connection = Connection::open_in_memory().expect("in-memory database");
            create_schema_at(&connection, version);
            connection
                .execute(
                    "INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                     VALUES ('note','migration-sentinel',41,987654321,'{\"safe\":true}')",
                    [],
                )
                .expect("historical row");
            let mut store = TradingStore {
                connection,
                retention: TradingRetention::default(),
            };
            store.migrate(false).expect("historical schema migrates");
            let preserved: (u32, i64, String) = store
                .connection
                .query_row(
                    "SELECT revision, updated_unix_nanos, json
                     FROM user_records WHERE kind = 'note' AND id = 'migration-sentinel'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("historical row survives");
            assert_eq!(
                preserved,
                (41, 987_654_321, "{\"safe\":true}".to_string()),
                "starting at v{version}"
            );
        }
    }

    #[test]
    fn open_applies_database_safety_pragmas() {
        let path = temporary_database("pragmas");
        let store =
            TradingStore::open(&path, TradingRetention::default()).expect("store opens safely");
        let journal: String = store
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal mode");
        let synchronous: u32 = store
            .connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .expect("synchronous");
        let foreign_keys: u32 = store
            .connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign keys");
        let busy_timeout: u32 = store
            .connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("busy timeout");
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 2);
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5_000);
        drop(store);
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn quick_check_rejects_empty_headerless_and_truncated_files_without_changing_hash() {
        for (name, bytes) in [
            ("empty", Vec::new()),
            ("headerless", vec![0_u8; 4_096]),
            ("truncated", b"SQLite format 3\0partial".to_vec()),
        ] {
            let path = temporary_database(name);
            fs::write(&path, &bytes).expect("invalid fixture");
            let before_hash = file_hash(&path);
            let Err(error) = TradingStore::open(&path, TradingRetention::default()) else {
                panic!("invalid database unexpectedly opened");
            };
            assert!(matches!(error, StoreError::Corrupt { .. }));
            assert_eq!(file_hash(&path), before_hash);
            assert_eq!(fs::read(&path).expect("fixture reads"), bytes);
            fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
        }
    }

    #[test]
    fn existing_sqlite_file_without_an_aeris_schema_is_corrupt_and_unchanged() {
        let path = temporary_database("empty-sqlite");
        let connection = Connection::open(&path).expect("empty SQLite fixture");
        connection
            .execute_batch("VACUUM;")
            .expect("materialize header");
        drop(connection);
        let before_hash = file_hash(&path);

        let Err(error) = TradingStore::open(&path, TradingRetention::default()) else {
            panic!("existing empty SQLite file must not be initialized");
        };

        assert!(matches!(error, StoreError::Corrupt { .. }));
        assert_eq!(file_hash(&path), before_hash);
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn quick_check_rejects_a_corrupt_database_page_without_changing_hash() {
        let path = temporary_database("corrupt-page");
        let connection = Connection::open(&path).expect("fixture opens");
        create_schema_at(&connection, SCHEMA_VERSION);
        connection
            .execute_batch(
                "INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                 VALUES ('note','large-record',1,1,printf('%.*c', 12000, 'x'));",
            )
            .expect("large fixture row");
        drop(connection);
        let mut bytes = fs::read(&path).expect("fixture reads");
        let page_size = match u16::from_be_bytes([bytes[16], bytes[17]]) {
            1 => 65_536,
            size => usize::from(size),
        };
        assert!(bytes.len() >= page_size * 2, "fixture has multiple pages");
        bytes[page_size..page_size * 2].fill(0xff);
        fs::write(&path, &bytes).expect("corrupt fixture writes");
        let before_hash = file_hash(&path);
        let Err(error) = TradingStore::open(&path, TradingRetention::default()) else {
            panic!("page-corrupt database unexpectedly opened");
        };
        assert!(matches!(error, StoreError::Corrupt { .. }));
        assert_eq!(file_hash(&path), before_hash);
        assert_eq!(fs::read(&path).expect("fixture reads"), bytes);
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn preflight_never_changes_an_existing_wal_database_or_its_sidecars() {
        let path = temporary_database("wal-preflight");
        let connection = Connection::open(&path).expect("fixture opens");
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("WAL mode");
        create_schema_at(&connection, SCHEMA_VERSION);
        connection
            .execute(
                "INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                 VALUES ('note','wal-record',1,1,'{}')",
                [],
            )
            .expect("WAL fixture row");
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(wal.is_file());
        assert!(shm.is_file());
        let before = [
            (path.clone(), file_hash(&path)),
            (wal.clone(), file_hash(&wal)),
            (shm.clone(), file_hash(&shm)),
        ];

        preflight_existing_database(&path).expect("copied WAL database passes preflight");

        for (file, hash) in before {
            assert_eq!(file_hash(&file), hash, "{} changed", file.display());
        }
        drop(connection);
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn newer_schema_preserves_file_hash_after_typed_rejection() {
        let path = temporary_database("newer");
        let connection = Connection::open(&path).expect("fixture opens");
        create_schema_at(&connection, SCHEMA_VERSION);
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("newer version");
        drop(connection);
        let before = fs::read(&path).expect("fixture reads");
        let before_hash = file_hash(&path);
        let Err(error) = TradingStore::open(&path, TradingRetention::default()) else {
            panic!("newer database unexpectedly opened");
        };
        assert!(matches!(
            error,
            StoreError::NewerSchema {
                found,
                supported
            } if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION
        ));
        assert_eq!(file_hash(&path), before_hash);
        assert_eq!(fs::read(&path).expect("fixture reads"), before);
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn migration_backs_up_old_schema_and_retains_three() {
        let path = temporary_database("backup");
        create_sanitized_v18_fixture(&path);
        let backups = path.parent().expect("parent").join("backups");
        fs::create_dir_all(&backups).expect("backups");
        for unix in 1..=3 {
            fs::write(
                backups.join(format!("trading-v17-{unix:013}.sqlite3")),
                b"old",
            )
            .expect("old backup");
        }
        let store =
            TradingStore::open(&path, TradingRetention::default()).expect("migration succeeds");
        drop(store);
        let retained = fs::read_dir(&backups)
            .expect("backups read")
            .map(|entry| entry.expect("entry").path())
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 3);
        let v18 = retained
            .iter()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("trading-v18-"))
            })
            .expect("v18 backup");
        let backup_connection =
            Connection::open_with_flags(v18, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("backup");
        let version: u32 = backup_connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("backup version");
        assert_eq!(version, 18);
        let preserved: String = backup_connection
            .query_row(
                "SELECT json FROM user_records
                 WHERE kind = 'note' AND id = 'sanitized-v18-record'",
                [],
                |row| row.get(0),
            )
            .expect("sanitized row is present in backup");
        assert_eq!(preserved, "{\"text\":\"fixture\"}");
        drop(backup_connection);
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn fresh_and_current_databases_do_not_create_migration_backups() {
        let fresh_path = temporary_database("fresh-no-backup");
        let fresh =
            TradingStore::open(&fresh_path, TradingRetention::default()).expect("fresh store");
        drop(fresh);
        assert!(
            !fresh_path
                .parent()
                .expect("parent")
                .join("backups")
                .exists()
        );

        let current_path = temporary_database("current-no-backup");
        let connection = Connection::open(&current_path).expect("current fixture");
        create_schema_at(&connection, SCHEMA_VERSION);
        drop(connection);
        let current =
            TradingStore::open(&current_path, TradingRetention::default()).expect("current store");
        drop(current);
        assert!(
            !current_path
                .parent()
                .expect("parent")
                .join("backups")
                .exists()
        );

        fs::remove_dir_all(fresh_path.parent().expect("parent")).expect("fixture cleanup");
        fs::remove_dir_all(current_path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    #[ignore = "manual release-mode 50k-order quick_check measurement"]
    fn measured_quick_check_on_deterministic_50k_order_database() {
        const ORDERS: u32 = 50_000;
        let path = temporary_database("quick-check-50k");
        let connection = Connection::open(&path).expect("measurement fixture");
        create_schema_at(&connection, SCHEMA_VERSION);
        connection
            .execute_batch(
                "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,
                     venue_id)
                 VALUES ('account','Sanitized','simulated','USD',2,'aeris-sim');
                 INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
                 VALUES ('instrument',2,0,'{}');
                 WITH RECURSIVE sequence(value) AS (
                     VALUES(1) UNION ALL SELECT value + 1 FROM sequence WHERE value < 50000
                 )
                 INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                     time_in_force,quantity_units,quantity_scale,limit_units,limit_scale,
                     stop_units,stop_scale,status,submitted_unix_nanos,provenance_json,
                     filled_units,filled_scale)
                 SELECT printf('order-%05d',value),printf('client-%05d',value),'account',
                     'instrument','buy','market','day',1,0,NULL,NULL,NULL,NULL,'working',
                     value,'{}',0,0 FROM sequence;",
            )
            .expect("deterministic orders");
        let count: u32 = connection
            .query_row("SELECT COUNT(*) FROM orders", [], |row| row.get(0))
            .expect("order count");
        assert_eq!(count, ORDERS);
        drop(connection);

        let started = std::time::Instant::now();
        preflight_existing_database(&path).expect("quick_check passes");
        let elapsed = started.elapsed();
        println!(
            "SQLite quick_check: {ORDERS} deterministic orders, {} bytes, {elapsed:?}",
            fs::metadata(&path).expect("database metadata").len()
        );
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn shutdown_checkpoint_truncates_wal() {
        let path = temporary_database("checkpoint");
        let store =
            TradingStore::open(&path, TradingRetention::default()).expect("store opens safely");
        store
            .connection
            .execute(
                "INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                 VALUES ('note','checkpoint',1,1,'{}')",
                [],
            )
            .expect("WAL write");
        store.checkpoint_for_shutdown().expect("checkpoint");
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        assert!(fs::metadata(wal).is_ok_and(|metadata| metadata.len() == 0));
        drop(store);
        fs::remove_dir_all(path.parent().expect("parent")).expect("fixture cleanup");
    }

    #[test]
    fn dropped_export_stage_preserves_previous_content() {
        let root =
            std::env::temp_dir().join(format!("aeris-trading-export-drop-{}", std::process::id()));
        fs::create_dir_all(&root).expect("fixture root");
        let path = root.join("executions.csv");
        write_atomic(&path, b"previous\n").expect("previous export");
        let mut staged =
            aeris_platform_sys::fs::StagedFile::create(&path).expect("export stage creates");
        staged.write_all(b"replacement\n").expect("stage writes");
        drop(staged);
        assert_eq!(
            fs::read(&path).expect("previous export reads"),
            b"previous\n"
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn legacy_tastytrade_future_contract_uses_exact_instrument_display_symbol() {
        let legacy = r#"{
            "tick_size":null,"point_value":null,"currency":"USD", "expiry":null,
            "first_notice":null,"last_trade":null,"session_hours":[],
            "provenance":{"provider_id":"tastytrade",
                "provider_symbol":"/ESZ26:XCME","session_generation":1}
        }"#;
        let id = InstrumentId::try_new("tastytrade:Future:/ESZ6").expect("instrument id");
        let contract = decode_contract(legacy, &id).expect("legacy contract decodes");
        assert_eq!(contract.provenance.display_symbol, "ESZ6");
        assert_eq!(contract.provenance.provider_symbol, "/ESZ26:XCME");
        assert_eq!(contract.order_quantity_increment, None);
        assert_eq!(contract.price_grid, PriceGrid::Unknown);
        assert_eq!(contract.instrument_class, InstrumentClass::Other);

        let equity = InstrumentId::try_new("tastytrade:Equity:AAPL").expect("equity id");
        assert_eq!(
            decode_contract(legacy, &equity)
                .expect("other identity retains legacy fallback")
                .provenance
                .display_symbol,
            "/ESZ26:XCME"
        );
    }

    #[test]
    fn contract_grid_and_class_survive_durable_round_trip() {
        let instrument = crate::tests::instrument();
        let encoded = encode_contract(&instrument.contract).expect("encode terms");
        let restored = decode_contract(&encoded, &instrument.instrument_id).expect("decode terms");
        assert_eq!(restored.price_grid, instrument.contract.price_grid);
        assert_eq!(restored.instrument_class, InstrumentClass::Future);
    }

    #[test]
    fn current_schema_migrates_historical_order_fill_progress() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(INITIAL_SCHEMA)
            .and_then(|()| connection.execute_batch(MIGRATION_V2))
            .and_then(|()| connection.execute_batch(MIGRATION_V3))
            .and_then(|()| connection.execute_batch(MIGRATION_V4))
            .expect("schema four fixture");
        connection
            .pragma_update(None, "user_version", 4_u32)
            .expect("schema version");
        connection
            .execute_batch(
                "INSERT INTO accounts(id, display_name, environment, currency, currency_scale)
                    VALUES ('account', 'SIM account', 'simulated', 'USD', 2);
                 INSERT INTO instruments(id, price_scale, quantity_scale, contract_json)
                    VALUES ('instrument', 2, 0, '{}');",
            )
            .expect("order parents");
        for (id, status) in [("filled-order", "filled"), ("working-order", "working")] {
            connection
                .execute(
                    "INSERT INTO orders(id, client_order_id, account_id, instrument_id, side,
                        order_type, time_in_force, quantity_units, quantity_scale, limit_units,
                        limit_scale, stop_units, stop_scale, status, submitted_unix_nanos,
                        provenance_json)
                     VALUES (?1, ?2, 'account', 'instrument', 'buy', 'limit', 'day', 4, 0,
                        5000, 2, NULL, NULL, ?3, 1, '{}')",
                    params![id, format!("client-{id}"), status],
                )
                .expect("historical order");
        }
        connection
            .execute_batch(
                "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                    price_scale, quantity_units, quantity_scale, execution_unix_nanos, provenance_json)
                 VALUES ('legacy-fill', 'filled-order', 'account', 'instrument', 'buy',
                    5000, 2, 4, 0, 1, '{}');",
            )
            .expect("historical fill");

        let mut store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        store.migrate(false).expect("schema five migration");

        let version = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .expect("migrated version");
        assert_eq!(version, SCHEMA_VERSION);
        let filled = store
            .connection
            .query_row(
                "SELECT filled_units, filled_scale FROM orders WHERE id = 'filled-order'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, u8>(1)?)),
            )
            .expect("filled progress");
        assert_eq!(filled, (4, 0));
        let working = store
            .connection
            .query_row(
                "SELECT filled_units, filled_scale FROM orders WHERE id = 'working-order'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, u8>(1)?)),
            )
            .expect("working progress");
        assert_eq!(working, (0, 0));
        assert!(
            store
                .load_completed_trade_pnl()
                .expect("historical fill has no recorded outcome")
                .is_empty(),
            "migration must not invent final P&L for old executions"
        );
        assert!(
            store
                .load_fill_realized_pnl()
                .expect("historical fill has no recorded realized P&L")
                .is_empty(),
            "migration must not invent realized P&L for old executions"
        );
    }

    #[test]
    fn migration_v18_preserves_v17_rows_and_adds_empty_broker_tables() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(INITIAL_SCHEMA)
            .expect("base schema");
        for migration in [
            MIGRATION_V2,
            MIGRATION_V3,
            MIGRATION_V4,
            MIGRATION_V5,
            MIGRATION_V6,
            MIGRATION_V7,
            MIGRATION_V8,
            MIGRATION_V9,
            MIGRATION_V10,
            MIGRATION_V11,
            MIGRATION_V12,
            MIGRATION_V13,
            MIGRATION_V14,
            MIGRATION_V15,
            MIGRATION_V16,
            MIGRATION_V17,
        ] {
            connection.execute_batch(migration).expect("v17 migration");
        }
        connection
            .pragma_update(None, "user_version", 17_u32)
            .expect("version");
        connection
            .execute_batch(
                "INSERT INTO accounts(id,display_name,environment,currency,currency_scale)
                    VALUES ('sim','SIM account','simulated','USD',2);
                 INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
                    VALUES ('symbol',2,0,'{}');
                 INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,
                    order_type,time_in_force,quantity_units,quantity_scale,filled_units,
                    filled_scale,limit_units,limit_scale,status,submitted_unix_nanos,provenance_json)
                    VALUES ('order','client','sim','symbol','buy','limit','day',1,0,0,0,
                    100,2,'working',1,'{}');
                 INSERT INTO order_events(id,order_id,sequence,kind,event_unix_nanos,provenance_json)
                    VALUES ('event','order',1,'accepted',1,'{}');
                 INSERT INTO fills(id,order_id,account_id,instrument_id,side,price_units,
                    price_scale,quantity_units,quantity_scale,execution_unix_nanos,provenance_json)
                    VALUES ('fill','order','sim','symbol','buy',100,2,1,0,1,'{}');
                 INSERT INTO positions(account_id,instrument_id,net_units,net_scale,
                    realized_units,realized_scale,unrealized_units,unrealized_scale,last_fill_unix_nanos)
                    VALUES ('sim','symbol',1,0,0,2,0,2,1);
                 INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                    VALUES ('note','record',1,1,'{}');",
            )
            .expect("v17 rows");
        let mut store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        store.migrate(false).expect("v18 and v19 migrations");
        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(
            store.metadata("venue_generation").expect("v19 generation"),
            0
        );
        let (venue, broker_ref): (String, Option<String>) = store
            .connection
            .query_row(
                "SELECT venue_id, broker_ref FROM accounts WHERE id = 'sim'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("legacy account");
        assert_eq!(venue, "aeris-sim");
        assert_eq!(broker_ref, None);
        for (table, count) in [
            ("accounts", 1),
            ("orders", 1),
            ("order_events", 1),
            ("fills", 1),
            ("positions", 1),
            ("user_records", 1),
            ("broker_orders", 0),
            ("broker_deals", 0),
            ("broker_positions", 0),
            ("broker_account_state", 0),
        ] {
            let actual: i64 = store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("table retained");
            assert_eq!(actual, count, "{table}");
        }
    }

    #[test]
    fn a_newer_schema_is_rejected_instead_of_downgraded() {
        let mut store = TradingStore {
            connection: Connection::open_in_memory().expect("in-memory database"),
            retention: TradingRetention::default(),
        };
        store.migrate(false).expect("fresh schema");
        store
            .connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("newer version");
        assert!(
            store
                .migrate(false)
                .expect_err("newer schema rejected")
                .to_string()
                .contains("newer")
        );
    }

    #[test]
    fn broker_history_cascades_with_retired_orders_and_account() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        let mut store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        store.migrate(false).expect("current schema");
        store
            .connection
            .execute_batch(
                "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,
                venue_id,broker_ref) VALUES ('broker','Demo','demo','USD',2,'ctrader','ref');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
                VALUES ('symbol',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                limit_units,limit_scale,status,submitted_unix_nanos,provenance_json)
                VALUES ('order','client','broker','symbol','buy','limit','day',
                1,0,1,0,100,2,'filled',1,'{}');
             INSERT INTO fills(id,order_id,account_id,instrument_id,side,price_units,
                price_scale,quantity_units,quantity_scale,execution_unix_nanos,provenance_json)
                VALUES ('fill','order','broker','symbol','buy',100,2,1,0,1,'{}');
             INSERT INTO broker_orders(order_id,client_order_id,session_generation,
                updated_unix_nanos) VALUES ('order','client',1,1);
             INSERT INTO broker_deals(broker_deal_id,fill_id,account_id,
                executed_unix_millis) VALUES ('deal','fill','broker',1);
             INSERT INTO broker_account_state(account_id,balance_units,balance_scale,
                session_generation) VALUES ('broker',100,2,1);
             INSERT INTO broker_positions(account_id,broker_position_id,instrument_id,
                side,quantity_units,quantity_scale,entry_units,entry_scale,
                swap_units,swap_scale,commission_units,commission_scale,
                gross_unrealized_units,gross_unrealized_scale,
                net_unrealized_units,net_unrealized_scale,opened_unix_nanos)
                VALUES ('broker','position','symbol','buy',1,0,100,2,0,2,0,2,0,2,0,2,1);",
            )
            .expect("broker history");
        assert_eq!(store.load_broker_positions().expect("positions").len(), 1);
        store
            .connection
            .execute("DELETE FROM fills WHERE id='fill'", [])
            .expect("retire fill");
        store
            .connection
            .execute("DELETE FROM orders WHERE id='order'", [])
            .expect("retire order");
        for table in ["broker_orders", "broker_deals"] {
            let count: i64 = store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("dependent history");
            assert_eq!(count, 0, "{table} must cascade with retained history");
        }
        store
            .connection
            .execute("DELETE FROM accounts WHERE id='broker'", [])
            .expect("retire account");
        for table in ["broker_positions", "broker_account_state"] {
            let count: i64 = store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("account-dependent rows");
            assert_eq!(count, 0, "{table} must cascade with account");
        }
    }
}
