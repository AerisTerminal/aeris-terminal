use super::{
    BracketStrategyTemplate, BracketTarget, BreakEvenRule, DisciplineState,
    EconomicEventRiskAction, EconomicEventRiskImportance, EconomicEventRiskRule, ManagedBracket,
    ManagedBracketStatus, ProtectiveOrder, ProtectiveOrderRole, RiskLock, RiskProfile,
    RiskRuleState, RiskTradeCycleState, SessionBias, SessionChecklistItem, SessionPlan,
    SessionPlanLevel, TradeCopierConfig, TradeCopierTarget, TradingInstrument, TradingRetention,
    TrailingDrawdownMode, TrailingStopRule, UserRecord,
};
use aeris_instruments::{
    ContractDate, ContractMetadata, InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance,
    SessionHours,
};
use aeris_trading::{
    AccountEnvironment, BrokerPosition, ClientOrderId, Fill, FillId, FixedPoint, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position,
    TimeInForce, TradingAccount, TradingAccountId, TradingProvenance,
};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    fs,
    path::Path,
};

pub(super) const SCHEMA_VERSION: u32 = 18;
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
const MIGRATION_V4: &str =
    "ALTER TABLE risk_profiles ADD COLUMN session_start_realized_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE risk_profiles ADD COLUMN session_start_realized_scale INTEGER NOT NULL DEFAULT 2;";
const MIGRATION_V5: &str = "ALTER TABLE orders ADD COLUMN filled_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE orders ADD COLUMN filled_scale INTEGER NOT NULL DEFAULT 0;
 UPDATE orders SET filled_scale = quantity_scale;
 UPDATE orders SET filled_units = quantity_units WHERE status = 'filled';";
const MIGRATION_V6: &str = "CREATE TABLE trade_copiers (
     source_account_id TEXT PRIMARY KEY REFERENCES accounts(id),
     revision INTEGER NOT NULL, enabled INTEGER NOT NULL
 ) STRICT;
 CREATE TABLE trade_copier_targets (
     source_account_id TEXT NOT NULL REFERENCES trade_copiers(source_account_id) ON DELETE CASCADE,
     target_account_id TEXT NOT NULL REFERENCES accounts(id), multiplier_units INTEGER NOT NULL,
     multiplier_scale INTEGER NOT NULL, enabled INTEGER NOT NULL,
     PRIMARY KEY(source_account_id, target_account_id)
 ) STRICT;";
const MIGRATION_V7: &str = "CREATE TABLE strategy_templates (
     template_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, name TEXT NOT NULL,
     stop_offset_ticks INTEGER NOT NULL, targets_json TEXT NOT NULL,
     trailing_activation_ticks INTEGER, trailing_distance_ticks INTEGER,
     break_even_activation_ticks INTEGER, break_even_offset_ticks INTEGER,
     enabled INTEGER NOT NULL
 ) STRICT;";
const MIGRATION_V8: &str = "CREATE TABLE managed_brackets (
     bracket_id TEXT PRIMARY KEY, template_json TEXT NOT NULL,
     entry_client_order_id TEXT NOT NULL UNIQUE, stop_client_order_id TEXT,
     target_client_order_ids_json TEXT NOT NULL, status TEXT NOT NULL,
     entry_price_units INTEGER, entry_price_scale INTEGER
 ) STRICT;";
const MIGRATION_V9: &str = "CREATE TABLE protective_orders (
     client_order_id TEXT PRIMARY KEY REFERENCES orders(client_order_id) ON DELETE CASCADE,
     role TEXT NOT NULL
 ) STRICT;";
const MIGRATION_V10: &str = "CREATE TABLE risk_rule_states (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), profile_id TEXT NOT NULL,
     profile_version INTEGER NOT NULL, peak_session_units INTEGER NOT NULL,
     peak_session_scale INTEGER NOT NULL
 ) STRICT;
 INSERT INTO risk_rule_states(account_id, profile_id, profile_version,
     peak_session_units, peak_session_scale)
 SELECT account_id, profile_id, version, 0, daily_loss_scale FROM risk_profiles;";
const MIGRATION_V11: &str = "ALTER TABLE risk_rule_states
     ADD COLUMN total_winning_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE risk_rule_states ADD COLUMN total_winning_scale INTEGER NOT NULL DEFAULT 2;
 ALTER TABLE risk_rule_states
     ADD COLUMN largest_winner_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE risk_rule_states ADD COLUMN largest_winner_scale INTEGER NOT NULL DEFAULT 2;
 CREATE TABLE risk_trade_cycles (
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id),
     realized_units INTEGER NOT NULL, realized_scale INTEGER NOT NULL,
     PRIMARY KEY(account_id, instrument_id)
 ) STRICT;";
const MIGRATION_V12: &str = "CREATE TABLE session_plans (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), plan_id TEXT NOT NULL,
     revision INTEGER NOT NULL, session_start_unix_nanos INTEGER NOT NULL,
     session_end_unix_nanos INTEGER NOT NULL, plan_json TEXT NOT NULL
 ) STRICT;";
const MIGRATION_V13: &str = "ALTER TABLE risk_trade_cycles
     ADD COLUMN peak_quantity_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE risk_trade_cycles ADD COLUMN peak_quantity_scale INTEGER NOT NULL DEFAULT 0;
 CREATE TABLE discipline_states (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), rapid_loss_count INTEGER NOT NULL,
     loss_window_started_unix_nanos INTEGER, last_loss_unix_nanos INTEGER,
     post_loss_quantity_units INTEGER, post_loss_quantity_scale INTEGER,
     last_stop_fill_unix_nanos INTEGER, cooldown_until_unix_nanos INTEGER
 ) STRICT;";
const MIGRATION_V14: &str = "ALTER TABLE risk_profiles ADD COLUMN event_action TEXT;
 ALTER TABLE risk_profiles ADD COLUMN event_minimum_importance INTEGER;
 ALTER TABLE risk_profiles ADD COLUMN event_lead_seconds INTEGER;
 CREATE TABLE economic_event_risk_actions (
     account_id TEXT NOT NULL REFERENCES accounts(id), event_id TEXT NOT NULL,
     profile_version INTEGER NOT NULL, action TEXT NOT NULL,
     applied_unix_nanos INTEGER NOT NULL,
     PRIMARY KEY(account_id, event_id, profile_version)
 ) STRICT;";
const MIGRATION_V15: &str = "ALTER TABLE accounts ADD COLUMN starting_equity_units INTEGER;
 ALTER TABLE accounts ADD COLUMN starting_equity_scale INTEGER;";
const MIGRATION_V16: &str = "ALTER TABLE fills ADD COLUMN completed_trade_pnl_units INTEGER;
 ALTER TABLE fills ADD COLUMN completed_trade_pnl_scale INTEGER;
 CREATE TABLE trade_pnl_cycles (
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id),
     realized_units INTEGER NOT NULL, realized_scale INTEGER NOT NULL,
     PRIMARY KEY(account_id, instrument_id)
 ) STRICT;";
const MIGRATION_V17: &str = "ALTER TABLE fills ADD COLUMN realized_pnl_units INTEGER;
 ALTER TABLE fills ADD COLUMN realized_pnl_scale INTEGER;";
const MIGRATION_V18: &str =
    "ALTER TABLE accounts ADD COLUMN venue_id TEXT NOT NULL DEFAULT 'aeris-sim';
 ALTER TABLE accounts ADD COLUMN broker_ref TEXT;
 ALTER TABLE accounts ADD COLUMN broker_account_type TEXT;
 ALTER TABLE accounts ADD COLUMN connection_state TEXT;
 CREATE TABLE broker_orders (
     order_id TEXT PRIMARY KEY REFERENCES orders(id) ON DELETE CASCADE,
     broker_order_id TEXT UNIQUE, client_order_id TEXT NOT NULL,
     session_generation INTEGER NOT NULL, updated_unix_nanos INTEGER NOT NULL
 ) STRICT;
 CREATE TABLE broker_deals (
     broker_deal_id TEXT PRIMARY KEY, fill_id TEXT NOT NULL UNIQUE
         REFERENCES fills(id) ON DELETE CASCADE,
     account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
     broker_position_id TEXT, executed_unix_millis INTEGER NOT NULL
 ) STRICT;
 CREATE TABLE broker_positions (
     account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
     broker_position_id TEXT NOT NULL, instrument_id TEXT NOT NULL REFERENCES instruments(id),
     side TEXT NOT NULL, quantity_units INTEGER NOT NULL, quantity_scale INTEGER NOT NULL,
     entry_units INTEGER NOT NULL, entry_scale INTEGER NOT NULL,
     stop_units INTEGER, stop_scale INTEGER, take_units INTEGER, take_scale INTEGER,
     swap_units INTEGER NOT NULL, swap_scale INTEGER NOT NULL,
     commission_units INTEGER NOT NULL, commission_scale INTEGER NOT NULL,
     gross_unrealized_units INTEGER NOT NULL, gross_unrealized_scale INTEGER NOT NULL,
     net_unrealized_units INTEGER NOT NULL, net_unrealized_scale INTEGER NOT NULL,
     opened_unix_nanos INTEGER NOT NULL,
     PRIMARY KEY(account_id, broker_position_id)
 ) STRICT;
 CREATE TABLE broker_account_state (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
     balance_units INTEGER NOT NULL, balance_scale INTEGER NOT NULL,
     last_deal_unix_millis INTEGER, session_generation INTEGER NOT NULL
 ) STRICT;";

pub(super) struct StoredState {
    pub revision: u64,
    pub next_sequence: u64,
    pub accounts: BTreeMap<TradingAccountId, TradingAccount>,
    pub instruments: BTreeMap<InstrumentId, TradingInstrument>,
    pub orders: BTreeMap<OrderId, Order>,
    pub order_events: VecDeque<OrderEvent>,
    pub fills: VecDeque<Fill>,
    pub completed_trade_pnl: BTreeMap<FillId, FixedPoint>,
    pub fill_realized_pnl: BTreeMap<FillId, FixedPoint>,
    pub trade_pnl_cycles: BTreeMap<(TradingAccountId, InstrumentId), FixedPoint>,
    pub positions: BTreeMap<(TradingAccountId, InstrumentId), Position>,
    pub broker_positions: BTreeMap<(TradingAccountId, String), BrokerPosition>,
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

struct StoredRiskProfile {
    account: String,
    profile_id: String,
    version: u32,
    daily_loss_units: i64,
    daily_loss_scale: u8,
    trailing_units: Option<i64>,
    trailing_scale: Option<u8>,
    trailing_mode: String,
    max_contracts_units: i64,
    max_contracts_scale: u8,
    consistency_percent: Option<u8>,
    restricted_until_unix_nanos: Option<i64>,
    enabled: i64,
    session_start_unix_nanos: i64,
    session_start_realized_units: i64,
    session_start_realized_scale: u8,
    event_action: Option<String>,
    event_minimum_importance: Option<i64>,
    event_lead_seconds: Option<u32>,
}

fn decode_risk_profile(
    stored: StoredRiskProfile,
) -> Result<(TradingAccountId, RiskProfile), String> {
    let account_id =
        TradingAccountId::try_new(stored.account).map_err(|error| error.to_string())?;
    let daily_loss_limit = FixedPoint::try_new(stored.daily_loss_units, stored.daily_loss_scale)
        .map_err(|error| error.to_string())?;
    let trailing_drawdown = stored
        .trailing_units
        .zip(stored.trailing_scale)
        .map(|(units, scale)| FixedPoint::try_new(units, scale))
        .transpose()
        .map_err(|error| error.to_string())?;
    let max_contracts = FixedPoint::try_new(stored.max_contracts_units, stored.max_contracts_scale)
        .map_err(|error| error.to_string())?;
    let session_start_realized_pnl = FixedPoint::try_new(
        stored.session_start_realized_units,
        stored.session_start_realized_scale,
    )
    .map_err(|error| error.to_string())?;
    let economic_event_rule = stored
        .event_action
        .zip(stored.event_minimum_importance)
        .zip(stored.event_lead_seconds)
        .map(
            |((action, importance), lead_seconds)| -> Result<_, String> {
                Ok(EconomicEventRiskRule {
                    action: EconomicEventRiskAction::parse(&action)?,
                    minimum_importance: EconomicEventRiskImportance::from_i64(importance)?,
                    lead_seconds,
                })
            },
        )
        .transpose()?;
    let profile = RiskProfile {
        account_id: account_id.clone(),
        profile_id: stored.profile_id,
        version: stored.version,
        daily_loss_limit,
        trailing_drawdown,
        trailing_mode: TrailingDrawdownMode::parse(&stored.trailing_mode)?,
        max_contracts,
        consistency_max_single_trade_percent: stored.consistency_percent,
        restricted_until_unix_nanos: stored.restricted_until_unix_nanos,
        economic_event_rule,
        enabled: stored.enabled != 0,
        session_start_unix_nanos: stored.session_start_unix_nanos,
        session_start_realized_pnl,
    };
    profile.validate()?;
    Ok((account_id, profile))
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
        store.ensure_default_strategy_templates()?;
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
            self.apply_migration(2, MIGRATION_V2)?;
            version = 2;
        }
        if version == 2 {
            self.apply_migration(3, MIGRATION_V3)?;
            version = 3;
        }
        if version == 3 {
            self.apply_migration(4, MIGRATION_V4)?;
            version = 4;
        }
        if version == 4 {
            self.apply_migration(5, MIGRATION_V5)?;
            version = 5;
        }
        if version == 5 {
            self.apply_migration(6, MIGRATION_V6)?;
            version = 6;
        }
        if version == 6 {
            self.apply_migration(7, MIGRATION_V7)?;
            version = 7;
        }
        if version == 7 {
            self.apply_migration(8, MIGRATION_V8)?;
            version = 8;
        }
        if version == 8 {
            self.apply_migration(9, MIGRATION_V9)?;
            version = 9;
        }
        if version == 9 {
            self.apply_migration(10, MIGRATION_V10)?;
            version = 10;
        }
        if version == 10 {
            self.apply_migration(11, MIGRATION_V11)?;
            version = 11;
        }
        if version == 11 {
            self.apply_migration(12, MIGRATION_V12)?;
            version = 12;
        }
        if version == 12 {
            self.apply_migration(13, MIGRATION_V13)?;
            version = 13;
        }
        if version == 13 {
            self.apply_migration(14, MIGRATION_V14)?;
            version = 14;
        }
        if version == 14 {
            self.apply_migration(15, MIGRATION_V15)?;
            version = 15;
        }
        if version == 15 {
            self.apply_migration(16, MIGRATION_V16)?;
            version = 16;
        }
        if version == 16 {
            self.apply_migration(17, MIGRATION_V17)?;
            version = 17;
        }
        if version == 17 {
            self.apply_migration(SCHEMA_VERSION, MIGRATION_V18)?;
        }
        Ok(())
    }

    fn apply_migration(&mut self, version: u32, sql: &str) -> Result<(), String> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("trading schema migration could not start: {error}"))?;
        transaction
            .execute_batch(sql)
            .map_err(|error| format!("trading schema migration failed: {error}"))?;
        transaction
            .pragma_update(None, "user_version", version)
            .map_err(|error| format!("trading schema version could not be committed: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("trading migration could not commit: {error}"))
    }

    fn ensure_default_strategy_templates(&self) -> Result<(), String> {
        for template in [
            BracketStrategyTemplate {
                template_id: "managed-bracket".to_string(),
                revision: 1,
                name: "Managed stop + target".to_string(),
                stop_offset_ticks: 8,
                targets: vec![BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 100,
                }],
                trailing_stop: Some(TrailingStopRule {
                    activation_ticks: 12,
                    distance_ticks: 6,
                }),
                break_even: Some(BreakEvenRule {
                    activation_ticks: 8,
                    offset_ticks: 1,
                }),
                enabled: true,
            },
            BracketStrategyTemplate {
                template_id: "managed-scale-out".to_string(),
                revision: 1,
                name: "Managed 50/50 scale-out".to_string(),
                stop_offset_ticks: 8,
                targets: vec![
                    BracketTarget {
                        offset_ticks: 8,
                        quantity_percent: 50,
                    },
                    BracketTarget {
                        offset_ticks: 16,
                        quantity_percent: 50,
                    },
                ],
                trailing_stop: Some(TrailingStopRule {
                    activation_ticks: 12,
                    distance_ticks: 6,
                }),
                break_even: Some(BreakEvenRule {
                    activation_ticks: 8,
                    offset_ticks: 1,
                }),
                enabled: true,
            },
        ] {
            self.put_strategy_template(&template)?;
        }
        Ok(())
    }

    pub(super) fn load_state(&mut self) -> Result<StoredState, String> {
        let revision = self.metadata("revision")?;
        let next_sequence = self.metadata("next_sequence")?;
        let mut accounts = BTreeMap::new();
        {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT id, display_name, environment, currency, currency_scale,
                        starting_equity_units, starting_equity_scale FROM accounts ORDER BY id",
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
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<u8>>(6)?,
                    ))
                })
                .map_err(database_error)?;
            for row in rows {
                let (
                    id,
                    display_name,
                    environment,
                    currency,
                    currency_scale,
                    starting_equity_units,
                    starting_equity_scale,
                ) = row.map_err(database_error)?;
                let id = TradingAccountId::try_new(id).map_err(|error| error.to_string())?;
                let account = TradingAccount {
                    id: id.clone(),
                    display_name,
                    environment: parse_environment(&environment)?,
                    currency,
                    currency_scale,
                    starting_equity: starting_equity_units
                        .zip(starting_equity_scale)
                        .map(|(units, scale)| FixedPoint::try_new(units, scale))
                        .transpose()
                        .map_err(|error| error.to_string())?,
                };
                account.validate().map_err(|error| error.to_string())?;
                accounts.insert(id, account);
            }
        }
        let instruments = self.load_instruments()?;
        let orders = self.load_orders()?;
        let order_events = self.load_events()?;
        let fills = self.load_fills()?;
        let completed_trade_pnl = self.load_completed_trade_pnl()?;
        let fill_realized_pnl = self.load_fill_realized_pnl()?;
        let trade_pnl_cycles = self.load_trade_pnl_cycles()?;
        let positions = self.load_positions()?;
        let broker_positions = self.load_broker_positions()?;
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
            orders,
            order_events,
            fills,
            completed_trade_pnl,
            fill_realized_pnl,
            trade_pnl_cycles,
            positions,
            broker_positions,
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
                "INSERT INTO accounts(id, display_name, environment, currency, currency_scale,
                    starting_equity_units, starting_equity_scale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,
                    environment=excluded.environment, currency=excluded.currency,
                    currency_scale=excluded.currency_scale,
                    starting_equity_units=excluded.starting_equity_units,
                    starting_equity_scale=excluded.starting_equity_scale",
                params![
                    account.id.as_str(),
                    account.display_name,
                    account.environment.as_str(),
                    account.currency,
                    account.currency_scale,
                    account.starting_equity.map(FixedPoint::units),
                    account.starting_equity.map(FixedPoint::scale),
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn delete_practice_account(
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

    pub(super) fn put_risk_profile_and_state(
        &mut self,
        profile: &RiskProfile,
        state: &RiskRuleState,
    ) -> Result<(), String> {
        profile.validate()?;
        if state.account_id != profile.account_id
            || state.profile_id != profile.profile_id
            || state.profile_version != profile.version
        {
            return Err("risk profile state identity does not match profile".to_string());
        }
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO risk_profiles(account_id, profile_id, version, daily_loss_units,
                    daily_loss_scale, trailing_units, trailing_scale, trailing_mode,
                    max_contracts_units, max_contracts_scale, consistency_percent,
                    restricted_until_unix_nanos, enabled, session_start_unix_nanos,
                    session_start_realized_units, session_start_realized_scale, event_action,
                    event_minimum_importance, event_lead_seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT(account_id) DO UPDATE SET profile_id=excluded.profile_id,
                    version=excluded.version, daily_loss_units=excluded.daily_loss_units,
                    daily_loss_scale=excluded.daily_loss_scale, trailing_units=excluded.trailing_units,
                    trailing_scale=excluded.trailing_scale, trailing_mode=excluded.trailing_mode,
                    max_contracts_units=excluded.max_contracts_units,
                    max_contracts_scale=excluded.max_contracts_scale,
                    consistency_percent=excluded.consistency_percent,
                    restricted_until_unix_nanos=excluded.restricted_until_unix_nanos,
                    enabled=excluded.enabled,
                    session_start_unix_nanos=excluded.session_start_unix_nanos,
                    session_start_realized_units=excluded.session_start_realized_units,
                    session_start_realized_scale=excluded.session_start_realized_scale,
                    event_action=excluded.event_action,
                    event_minimum_importance=excluded.event_minimum_importance,
                    event_lead_seconds=excluded.event_lead_seconds",
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
                    profile.session_start_realized_pnl.units(),
                    profile.session_start_realized_pnl.scale(),
                    profile.economic_event_rule.map(|rule| rule.action.as_str()),
                    profile
                        .economic_event_rule
                        .map(|rule| rule.minimum_importance.as_i64()),
                    profile.economic_event_rule.map(|rule| rule.lead_seconds),
                ],
            )
            .map_err(database_error)?;
        upsert_risk_rule_state(&transaction, state)?;
        transaction
            .execute(
                "DELETE FROM risk_trade_cycles WHERE account_id = ?1",
                [profile.account_id.as_str()],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn put_session_plan(&self, plan: &SessionPlan) -> Result<(), String> {
        plan.validate()?;
        self.connection
            .execute(
                "INSERT INTO session_plans(account_id, plan_id, revision,
                    session_start_unix_nanos, session_end_unix_nanos, plan_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(account_id) DO UPDATE SET plan_id=excluded.plan_id,
                    revision=excluded.revision,
                    session_start_unix_nanos=excluded.session_start_unix_nanos,
                    session_end_unix_nanos=excluded.session_end_unix_nanos,
                    plan_json=excluded.plan_json",
                params![
                    plan.account_id.as_str(),
                    plan.plan_id,
                    plan.revision,
                    plan.session_start_unix_nanos,
                    plan.session_end_unix_nanos,
                    encode_session_plan(plan)?,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn put_trade_copier(&mut self, config: &TradeCopierConfig) -> Result<(), String> {
        config.validate()?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO trade_copiers(source_account_id, revision, enabled)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_account_id) DO UPDATE SET revision=excluded.revision,
                    enabled=excluded.enabled WHERE excluded.revision > trade_copiers.revision",
                params![
                    config.source_account_id.as_str(),
                    config.revision,
                    i64::from(config.enabled)
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM trade_copier_targets WHERE source_account_id = ?1",
                [config.source_account_id.as_str()],
            )
            .map_err(database_error)?;
        for target in &config.targets {
            transaction
                .execute(
                    "INSERT INTO trade_copier_targets(source_account_id, target_account_id,
                        multiplier_units, multiplier_scale, enabled) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        config.source_account_id.as_str(),
                        target.account_id.as_str(),
                        target.quantity_multiplier.units(),
                        target.quantity_multiplier.scale(),
                        i64::from(target.enabled),
                    ],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)
    }

    pub(super) fn put_strategy_template(
        &self,
        template: &BracketStrategyTemplate,
    ) -> Result<(), String> {
        template.validate()?;
        let targets = serde_json::to_string(&template.targets)
            .map_err(|error| format!("strategy targets could not be encoded: {error}"))?;
        self.connection
            .execute(
                "INSERT INTO strategy_templates(template_id, revision, name, stop_offset_ticks,
                    targets_json, trailing_activation_ticks, trailing_distance_ticks,
                    break_even_activation_ticks, break_even_offset_ticks, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(template_id) DO UPDATE SET revision=excluded.revision,
                    name=excluded.name, stop_offset_ticks=excluded.stop_offset_ticks,
                    targets_json=excluded.targets_json,
                    trailing_activation_ticks=excluded.trailing_activation_ticks,
                    trailing_distance_ticks=excluded.trailing_distance_ticks,
                    break_even_activation_ticks=excluded.break_even_activation_ticks,
                    break_even_offset_ticks=excluded.break_even_offset_ticks,
                    enabled=excluded.enabled
                 WHERE excluded.revision > strategy_templates.revision",
                params![
                    template.template_id,
                    template.revision,
                    template.name,
                    template.stop_offset_ticks,
                    targets,
                    template.trailing_stop.map(|rule| rule.activation_ticks),
                    template.trailing_stop.map(|rule| rule.distance_ticks),
                    template.break_even.map(|rule| rule.activation_ticks),
                    template.break_even.map(|rule| rule.offset_ticks),
                    i64::from(template.enabled),
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(super) fn put_managed_bracket(&self, bracket: &ManagedBracket) -> Result<(), String> {
        upsert_managed_bracket(&self.connection, bracket)
    }

    pub(super) fn delete_managed_bracket(&self, bracket_id: &str) -> Result<(), String> {
        self.connection
            .execute(
                "DELETE FROM managed_brackets WHERE bracket_id = ?1",
                [bracket_id],
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

    pub(super) fn economic_event_action_exists(
        &self,
        account_id: &TradingAccountId,
        event_id: &str,
        profile_version: u32,
    ) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM economic_event_risk_actions
                 WHERE account_id = ?1 AND event_id = ?2 AND profile_version = ?3)",
                params![account_id.as_str(), event_id, profile_version],
                |row| row.get::<_, bool>(0),
            )
            .map_err(database_error)
    }

    pub(super) fn put_economic_event_action(
        &mut self,
        account_id: &TradingAccountId,
        event_id: &str,
        profile_version: u32,
        action: EconomicEventRiskAction,
        applied_unix_nanos: i64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO economic_event_risk_actions(account_id, event_id, profile_version,
                    action, applied_unix_nanos) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    account_id.as_str(),
                    event_id,
                    profile_version,
                    action.as_str(),
                    applied_unix_nanos,
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM economic_event_risk_actions WHERE rowid IN (
                    SELECT rowid FROM economic_event_risk_actions
                    ORDER BY applied_unix_nanos DESC LIMIT -1 OFFSET ?1
                 )",
                [usize_to_i64(self.retention.maximum_user_records_per_kind)?],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
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

    pub(super) fn put_risk_rule_state(&self, state: &RiskRuleState) -> Result<(), String> {
        upsert_risk_rule_state(&self.connection, state)
    }

    pub(super) fn put_discipline_state(&self, state: &DisciplineState) -> Result<(), String> {
        upsert_discipline_state(&self.connection, state)
    }

    fn load_risk_profiles(&self) -> Result<BTreeMap<TradingAccountId, RiskProfile>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, profile_id, version, daily_loss_units, daily_loss_scale,
                    trailing_units, trailing_scale, trailing_mode, max_contracts_units,
                    max_contracts_scale, consistency_percent, restricted_until_unix_nanos, enabled,
                    session_start_unix_nanos, session_start_realized_units,
                    session_start_realized_scale, event_action, event_minimum_importance,
                    event_lead_seconds
                 FROM risk_profiles ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(StoredRiskProfile {
                    account: row.get(0)?,
                    profile_id: row.get(1)?,
                    version: row.get(2)?,
                    daily_loss_units: row.get(3)?,
                    daily_loss_scale: row.get(4)?,
                    trailing_units: row.get(5)?,
                    trailing_scale: row.get(6)?,
                    trailing_mode: row.get(7)?,
                    max_contracts_units: row.get(8)?,
                    max_contracts_scale: row.get(9)?,
                    consistency_percent: row.get(10)?,
                    restricted_until_unix_nanos: row.get(11)?,
                    enabled: row.get(12)?,
                    session_start_unix_nanos: row.get(13)?,
                    session_start_realized_units: row.get(14)?,
                    session_start_realized_scale: row.get(15)?,
                    event_action: row.get(16)?,
                    event_minimum_importance: row.get(17)?,
                    event_lead_seconds: row.get(18)?,
                })
            })
            .map_err(database_error)?;
        for row in rows {
            let (account_id, profile) = decode_risk_profile(row.map_err(database_error)?)?;
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

    fn load_risk_rule_states(&self) -> Result<BTreeMap<TradingAccountId, RiskRuleState>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, profile_id, profile_version, peak_session_units,
                    peak_session_scale, total_winning_units, total_winning_scale,
                    largest_winner_units, largest_winner_scale
                 FROM risk_rule_states ORDER BY account_id",
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
                    row.get::<_, i64>(5)?,
                    row.get::<_, u8>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                account_id,
                profile_id,
                profile_version,
                peak_units,
                peak_scale,
                total_units,
                total_scale,
                largest_units,
                largest_scale,
            ) = row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account_id).map_err(|error| error.to_string())?;
            result.insert(
                account_id.clone(),
                RiskRuleState {
                    account_id,
                    profile_id,
                    profile_version,
                    peak_session_pnl: fixed(peak_units, peak_scale)?,
                    total_winning_pnl: fixed(total_units, total_scale)?,
                    largest_winning_trade_pnl: fixed(largest_units, largest_scale)?,
                },
            );
        }
        Ok(result)
    }

    fn load_risk_trade_cycles(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, InstrumentId), RiskTradeCycleState>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, instrument_id, realized_units, realized_scale,
                    peak_quantity_units, peak_quantity_scale
                 FROM risk_trade_cycles ORDER BY account_id, instrument_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u8>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, u8>(5)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account_id, instrument_id, units, scale, peak_units, peak_scale) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account_id).map_err(|error| error.to_string())?;
            let instrument_id =
                InstrumentId::try_new(instrument_id).map_err(|error| error.to_string())?;
            let state = RiskTradeCycleState {
                account_id: account_id.clone(),
                instrument_id: instrument_id.clone(),
                realized_pnl: fixed(units, scale)?,
                peak_quantity: fixed(peak_units, peak_scale)?,
            };
            result.insert((account_id, instrument_id), state);
        }
        Ok(result)
    }

    fn load_session_plans(&self) -> Result<BTreeMap<TradingAccountId, SessionPlan>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, plan_id, revision, session_start_unix_nanos,
                    session_end_unix_nanos, plan_json FROM session_plans ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account_id, plan_id, revision, start, end, encoded) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account_id).map_err(|error| error.to_string())?;
            let plan =
                decode_session_plan(account_id.clone(), plan_id, revision, start, end, &encoded)?;
            plan.validate()?;
            result.insert(account_id, plan);
        }
        Ok(result)
    }

    fn load_discipline_states(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, DisciplineState>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, rapid_loss_count, loss_window_started_unix_nanos,
                    last_loss_unix_nanos, post_loss_quantity_units, post_loss_quantity_scale,
                    last_stop_fill_unix_nanos, cooldown_until_unix_nanos
                 FROM discipline_states ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u8>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<u8>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account, count, window, last_loss, cap_units, cap_scale, last_stop, cooldown) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            result.insert(
                account_id.clone(),
                DisciplineState {
                    account_id,
                    rapid_loss_count: count,
                    loss_window_started_unix_nanos: window,
                    last_loss_unix_nanos: last_loss,
                    post_loss_quantity_cap: optional_fixed(cap_units, cap_scale)?,
                    last_stop_fill_unix_nanos: last_stop,
                    cooldown_until_unix_nanos: cooldown,
                },
            );
        }
        Ok(result)
    }

    fn load_trade_copiers(&self) -> Result<BTreeMap<TradingAccountId, TradeCopierConfig>, String> {
        let mut result = BTreeMap::new();
        let mut configs = self
            .connection
            .prepare(
                "SELECT source_account_id, revision, enabled FROM trade_copiers
                 ORDER BY source_account_id",
            )
            .map_err(database_error)?;
        let rows = configs
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (source, revision, enabled) = row.map_err(database_error)?;
            let source_account_id =
                TradingAccountId::try_new(source).map_err(|error| error.to_string())?;
            let mut targets = self
                .connection
                .prepare(
                    "SELECT target_account_id, multiplier_units, multiplier_scale, enabled
                     FROM trade_copier_targets WHERE source_account_id = ?1
                     ORDER BY target_account_id",
                )
                .map_err(database_error)?;
            let targets = targets
                .query_map([source_account_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, u8>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(database_error)?
                .map(|row| {
                    let (account, units, scale, enabled) = row.map_err(database_error)?;
                    Ok(TradeCopierTarget {
                        account_id: TradingAccountId::try_new(account)
                            .map_err(|error| error.to_string())?,
                        quantity_multiplier: fixed(units, scale)?,
                        enabled: enabled != 0,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let config = TradeCopierConfig {
                source_account_id: source_account_id.clone(),
                revision,
                enabled: enabled != 0,
                targets,
            };
            config.validate()?;
            result.insert(source_account_id, config);
        }
        Ok(result)
    }

    fn load_strategy_templates(&self) -> Result<BTreeMap<String, BracketStrategyTemplate>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT template_id, revision, name, stop_offset_ticks, targets_json,
                    trailing_activation_ticks, trailing_distance_ticks,
                    break_even_activation_ticks, break_even_offset_ticks, enabled
                 FROM strategy_templates ORDER BY template_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<u32>>(5)?,
                    row.get::<_, Option<u32>>(6)?,
                    row.get::<_, Option<u32>>(7)?,
                    row.get::<_, Option<i32>>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                template_id,
                revision,
                name,
                stop_offset_ticks,
                targets,
                trailing_activation_ticks,
                trailing_distance_ticks,
                break_even_activation_ticks,
                break_even_offset_ticks,
                enabled,
            ) = row.map_err(database_error)?;
            let targets = serde_json::from_str::<Vec<BracketTarget>>(&targets)
                .map_err(|error| format!("strategy targets could not be decoded: {error}"))?;
            let trailing_stop = match (trailing_activation_ticks, trailing_distance_ticks) {
                (Some(activation_ticks), Some(distance_ticks)) => Some(TrailingStopRule {
                    activation_ticks,
                    distance_ticks,
                }),
                (None, None) => None,
                _ => return Err("stored strategy trailing rule is incomplete".to_string()),
            };
            let break_even = match (break_even_activation_ticks, break_even_offset_ticks) {
                (Some(activation_ticks), Some(offset_ticks)) => Some(BreakEvenRule {
                    activation_ticks,
                    offset_ticks,
                }),
                (None, None) => None,
                _ => return Err("stored strategy break-even rule is incomplete".to_string()),
            };
            let template = BracketStrategyTemplate {
                template_id: template_id.clone(),
                revision,
                name,
                stop_offset_ticks,
                targets,
                trailing_stop,
                break_even,
                enabled: enabled != 0,
            };
            template.validate()?;
            result.insert(template_id, template);
        }
        Ok(result)
    }

    fn load_managed_brackets(&self) -> Result<BTreeMap<String, ManagedBracket>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT bracket_id, template_json, entry_client_order_id,
                    stop_client_order_id, target_client_order_ids_json, status,
                    entry_price_units, entry_price_scale
                 FROM managed_brackets ORDER BY bracket_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<u8>>(7)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                bracket_id,
                template,
                entry_client_order_id,
                stop_client_order_id,
                target_client_order_ids,
                status,
                entry_price_units,
                entry_price_scale,
            ) = row.map_err(database_error)?;
            let target_client_order_ids =
                serde_json::from_str::<Vec<String>>(&target_client_order_ids)
                    .map_err(|error| {
                        format!("managed bracket targets could not be decoded: {error}")
                    })?
                    .into_iter()
                    .map(ClientOrderId::try_new)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?;
            let template =
                serde_json::from_str::<BracketStrategyTemplate>(&template).map_err(|error| {
                    format!("managed bracket template could not be decoded: {error}")
                })?;
            let entry_price = entry_price_units
                .zip(entry_price_scale)
                .map(|(units, scale)| fixed(units, scale))
                .transpose()?;
            if entry_price_units.is_some() != entry_price_scale.is_some() {
                return Err("stored managed bracket entry price is incomplete".to_string());
            }
            let bracket = ManagedBracket {
                bracket_id: bracket_id.clone(),
                template,
                entry_client_order_id: ClientOrderId::try_new(entry_client_order_id)
                    .map_err(|error| error.to_string())?,
                stop_client_order_id: stop_client_order_id
                    .map(ClientOrderId::try_new)
                    .transpose()
                    .map_err(|error| error.to_string())?,
                target_client_order_ids,
                status: ManagedBracketStatus::parse(&status)?,
                entry_price,
            };
            bracket.validate()?;
            result.insert(bracket_id, bracket);
        }
        Ok(result)
    }

    fn load_protective_orders(&self) -> Result<BTreeMap<ClientOrderId, ProtectiveOrder>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare("SELECT client_order_id, role FROM protective_orders ORDER BY client_order_id")
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        for row in rows {
            let (client_order_id, role) = row.map_err(database_error)?;
            let client_order_id =
                ClientOrderId::try_new(client_order_id).map_err(|error| error.to_string())?;
            result.insert(
                client_order_id.clone(),
                ProtectiveOrder {
                    client_order_id,
                    role: ProtectiveOrderRole::parse(&role)?,
                },
            );
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

    pub(super) fn insert_order_and_managed_bracket(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        bracket: &ManagedBracket,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        insert_order_row(&transaction, order)?;
        insert_event_row(&transaction, event)?;
        upsert_managed_bracket(&transaction, bracket)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn insert_protective_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        protective: &ProtectiveOrder,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        insert_order_row(&transaction, order)?;
        insert_event_row(&transaction, event)?;
        transaction
            .execute(
                "INSERT INTO protective_orders(client_order_id, role) VALUES (?1, ?2)",
                params![
                    protective.client_order_id.as_str(),
                    protective.role.as_str()
                ],
            )
            .map_err(database_error)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn insert_managed_child_orders(
        &mut self,
        orders: &[(Order, OrderEvent)],
        bracket: &ManagedBracket,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        for (order, event) in orders {
            insert_order_row(&transaction, order)?;
            insert_event_row(&transaction, event)?;
        }
        upsert_managed_bracket(&transaction, bracket)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }

    pub(super) fn insert_fill(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        fill: &Fill,
        position: &Position,
        policy: FillPolicyPersistence<'_>,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE orders SET status = 'filled', filled_units = quantity_units,
                    filled_scale = quantity_scale
                 WHERE id = ?1 AND status IN ('working', 'partially_filled')",
                [order.id.as_str()],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, event)?;
        transaction
            .execute(
                "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                    price_scale, quantity_units, quantity_scale, execution_unix_nanos,
                    provenance_json, completed_trade_pnl_units, completed_trade_pnl_scale,
                    realized_pnl_units, realized_pnl_scale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                    policy.completed_trade_pnl.map(FixedPoint::units),
                    policy.completed_trade_pnl.map(FixedPoint::scale),
                    policy.fill_realized_pnl.map(FixedPoint::units),
                    policy.fill_realized_pnl.map(FixedPoint::scale),
                ],
            )
            .map_err(database_error)?;
        upsert_position(&transaction, position)?;
        if let Some(cycle) = policy.trade_pnl_cycle {
            transaction
                .execute(
                    "INSERT INTO trade_pnl_cycles(account_id, instrument_id, realized_units, realized_scale)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(account_id, instrument_id) DO UPDATE SET
                       realized_units=excluded.realized_units, realized_scale=excluded.realized_scale",
                    params![
                        fill.account_id.as_str(),
                        fill.instrument_id.as_str(),
                        cycle.units(),
                        cycle.scale(),
                    ],
                )
                .map_err(database_error)?;
        } else {
            transaction
                .execute(
                    "DELETE FROM trade_pnl_cycles WHERE account_id = ?1 AND instrument_id = ?2",
                    params![fill.account_id.as_str(), fill.instrument_id.as_str()],
                )
                .map_err(database_error)?;
        }
        if let Some(rule_state) = policy.rule_state {
            upsert_risk_rule_state(&transaction, rule_state)?;
        }
        upsert_risk_trade_cycle(&transaction, policy.trade_cycle)?;
        upsert_discipline_state(&transaction, policy.discipline_state)?;
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
                "UPDATE orders SET status = 'cancelled'
                 WHERE id = ?1 AND status IN
                    ('pending', 'working', 'pending_modify', 'partially_filled', 'pending_cancel')",
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
                 WHERE id = ?7 AND status IN ('working', 'partially_filled')",
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
            let contract = decode_contract(&contract, &instrument_id)?;
            output.insert(
                instrument_id.clone(),
                TradingInstrument {
                    instrument_id,
                    price_scale,
                    quantity_scale,
                    contract,
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
                    stop_units, stop_scale, status, submitted_unix_nanos, provenance_json,
                    filled_units, filled_scale
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
                    row.get::<_, i64>(16)?,
                    row.get::<_, u8>(17)?,
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
                filled_units,
                filled_scale,
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
                filled_quantity: fixed(filled_units, filled_scale)?,
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

    fn load_completed_trade_pnl(&self) -> Result<BTreeMap<FillId, FixedPoint>, String> {
        self.load_fill_amounts(
            "SELECT id, completed_trade_pnl_units, completed_trade_pnl_scale
             FROM fills WHERE completed_trade_pnl_units IS NOT NULL
             ORDER BY execution_unix_nanos",
        )
    }

    fn load_fill_realized_pnl(&self) -> Result<BTreeMap<FillId, FixedPoint>, String> {
        self.load_fill_amounts(
            "SELECT id, realized_pnl_units, realized_pnl_scale
             FROM fills WHERE realized_pnl_units IS NOT NULL
             ORDER BY execution_unix_nanos",
        )
    }

    /// Loads one optional fixed-point amount per fill from a `(id, units, scale)` query.
    fn load_fill_amounts(&self, query: &str) -> Result<BTreeMap<FillId, FixedPoint>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self.connection.prepare(query).map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, u8>(2)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (id, units, scale) = row.map_err(database_error)?;
            output.insert(
                FillId::try_new(id).map_err(|error| error.to_string())?,
                fixed(units, scale)?,
            );
        }
        Ok(output)
    }

    fn load_trade_pnl_cycles(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, InstrumentId), FixedPoint>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, instrument_id, realized_units, realized_scale
                 FROM trade_pnl_cycles ORDER BY account_id, instrument_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u8>(3)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account, instrument, units, scale) = row.map_err(database_error)?;
            output.insert(
                (
                    TradingAccountId::try_new(account).map_err(|error| error.to_string())?,
                    InstrumentId::try_new(instrument).map_err(|error| error.to_string())?,
                ),
                fixed(units, scale)?,
            );
        }
        Ok(output)
    }

    fn load_broker_positions(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, String), BrokerPosition>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, broker_position_id, instrument_id, side,
                quantity_units, quantity_scale, entry_units, entry_scale,
                stop_units, stop_scale, take_units, take_scale,
                swap_units, swap_scale, commission_units, commission_scale,
                gross_unrealized_units, gross_unrealized_scale,
                net_unrealized_units, net_unrealized_scale, opened_unix_nanos
             FROM broker_positions ORDER BY account_id, broker_position_id",
            )
            .map_err(database_error)?;
        let mut rows = statement.query([]).map_err(database_error)?;
        let mut positions = BTreeMap::new();
        while let Some(row) = rows.next().map_err(database_error)? {
            let string = |column| row.get::<_, String>(column).map_err(database_error);
            let integer = |column| row.get::<_, i64>(column).map_err(database_error);
            let scale = |column| row.get::<_, u8>(column).map_err(database_error);
            let optional_integer =
                |column| row.get::<_, Option<i64>>(column).map_err(database_error);
            let optional_scale = |column| row.get::<_, Option<u8>>(column).map_err(database_error);
            let account_id =
                TradingAccountId::try_new(string(0)?).map_err(|error| error.to_string())?;
            let broker_position_id = string(1)?;
            let position = BrokerPosition {
                account_id: account_id.clone(),
                broker_position_id: broker_position_id.clone(),
                instrument_id: InstrumentId::try_new(string(2)?)
                    .map_err(|error| error.to_string())?,
                side: parse_side(&string(3)?)?,
                quantity: fixed(integer(4)?, scale(5)?)?,
                entry_price: fixed(integer(6)?, scale(7)?)?,
                stop_loss: optional_fixed(optional_integer(8)?, optional_scale(9)?)?,
                take_profit: optional_fixed(optional_integer(10)?, optional_scale(11)?)?,
                swap: fixed(integer(12)?, scale(13)?)?,
                commission: fixed(integer(14)?, scale(15)?)?,
                gross_unrealized: fixed(integer(16)?, scale(17)?)?,
                net_unrealized: fixed(integer(18)?, scale(19)?)?,
                opened_unix_nanos: integer(20)?,
            };
            position.validate().map_err(|error| error.to_string())?;
            positions.insert((account_id, broker_position_id), position);
        }
        Ok(positions)
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

fn upsert_risk_rule_state(connection: &Connection, state: &RiskRuleState) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO risk_rule_states(account_id, profile_id, profile_version,
                peak_session_units, peak_session_scale, total_winning_units,
                total_winning_scale, largest_winner_units, largest_winner_scale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(account_id) DO UPDATE SET profile_id=excluded.profile_id,
                profile_version=excluded.profile_version,
                peak_session_units=excluded.peak_session_units,
                peak_session_scale=excluded.peak_session_scale,
                total_winning_units=excluded.total_winning_units,
                total_winning_scale=excluded.total_winning_scale,
                largest_winner_units=excluded.largest_winner_units,
                largest_winner_scale=excluded.largest_winner_scale",
            params![
                state.account_id.as_str(),
                state.profile_id,
                state.profile_version,
                state.peak_session_pnl.units(),
                state.peak_session_pnl.scale(),
                state.total_winning_pnl.units(),
                state.total_winning_pnl.scale(),
                state.largest_winning_trade_pnl.units(),
                state.largest_winning_trade_pnl.scale(),
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn upsert_risk_trade_cycle(
    connection: &Connection,
    state: &RiskTradeCycleState,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO risk_trade_cycles(account_id, instrument_id, realized_units,
                realized_scale, peak_quantity_units, peak_quantity_scale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, instrument_id) DO UPDATE SET
                realized_units=excluded.realized_units,
                realized_scale=excluded.realized_scale,
                peak_quantity_units=excluded.peak_quantity_units,
                peak_quantity_scale=excluded.peak_quantity_scale",
            params![
                state.account_id.as_str(),
                state.instrument_id.as_str(),
                state.realized_pnl.units(),
                state.realized_pnl.scale(),
                state.peak_quantity.units(),
                state.peak_quantity.scale(),
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn upsert_discipline_state(connection: &Connection, state: &DisciplineState) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO discipline_states(account_id, rapid_loss_count,
                loss_window_started_unix_nanos, last_loss_unix_nanos,
                post_loss_quantity_units, post_loss_quantity_scale,
                last_stop_fill_unix_nanos, cooldown_until_unix_nanos)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(account_id) DO UPDATE SET rapid_loss_count=excluded.rapid_loss_count,
                loss_window_started_unix_nanos=excluded.loss_window_started_unix_nanos,
                last_loss_unix_nanos=excluded.last_loss_unix_nanos,
                post_loss_quantity_units=excluded.post_loss_quantity_units,
                post_loss_quantity_scale=excluded.post_loss_quantity_scale,
                last_stop_fill_unix_nanos=excluded.last_stop_fill_unix_nanos,
                cooldown_until_unix_nanos=excluded.cooldown_until_unix_nanos",
            params![
                state.account_id.as_str(),
                state.rapid_loss_count,
                state.loss_window_started_unix_nanos,
                state.last_loss_unix_nanos,
                state.post_loss_quantity_cap.map(FixedPoint::units),
                state.post_loss_quantity_cap.map(FixedPoint::scale),
                state.last_stop_fill_unix_nanos,
                state.cooldown_until_unix_nanos,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn upsert_managed_bracket(connection: &Connection, bracket: &ManagedBracket) -> Result<(), String> {
    bracket.validate()?;
    let target_client_order_ids = bracket
        .target_client_order_ids
        .iter()
        .map(ClientOrderId::as_str)
        .collect::<Vec<_>>();
    let target_client_order_ids = serde_json::to_string(&target_client_order_ids)
        .map_err(|error| format!("managed bracket targets could not be encoded: {error}"))?;
    let template = serde_json::to_string(&bracket.template)
        .map_err(|error| format!("managed bracket template could not be encoded: {error}"))?;
    connection
        .execute(
            "INSERT INTO managed_brackets(bracket_id, template_json,
                entry_client_order_id, stop_client_order_id, target_client_order_ids_json,
                status, entry_price_units, entry_price_scale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(bracket_id) DO UPDATE SET
                stop_client_order_id=excluded.stop_client_order_id,
                target_client_order_ids_json=excluded.target_client_order_ids_json,
                status=excluded.status, entry_price_units=excluded.entry_price_units,
                entry_price_scale=excluded.entry_price_scale",
            params![
                bracket.bracket_id,
                template,
                bracket.entry_client_order_id.as_str(),
                bracket
                    .stop_client_order_id
                    .as_ref()
                    .map(ClientOrderId::as_str),
                target_client_order_ids,
                bracket.status.as_str(),
                bracket.entry_price.map(FixedPoint::units),
                bracket.entry_price.map(FixedPoint::scale),
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn insert_order_row(transaction: &Transaction<'_>, order: &Order) -> Result<(), String> {
    let (limit_units, limit_scale) = split_optional(order.limit_price);
    let (stop_units, stop_scale) = split_optional(order.stop_price);
    transaction
        .execute(
            "INSERT INTO orders(id, client_order_id, account_id, instrument_id, side, order_type,
            time_in_force, quantity_units, quantity_scale, limit_units, limit_scale, stop_units,
            stop_scale, status, submitted_unix_nanos, provenance_json, filled_units, filled_scale)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
            ?17, ?18)",
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
                encode_provenance(&order.provenance),
                order.filled_quantity.units(),
                order.filled_quantity.scale(),
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

#[derive(Deserialize, Serialize)]
struct StoredSessionPlanPayload {
    bias: String,
    maximum_loss_units: i64,
    maximum_loss_scale: u8,
    session_start_realized_units: i64,
    session_start_realized_scale: u8,
    allowed_setups: Vec<String>,
    active_setup: Option<String>,
    checklist: Vec<StoredChecklistItem>,
    levels: Vec<StoredPlanLevel>,
}

#[derive(Deserialize, Serialize)]
struct StoredChecklistItem {
    item_id: String,
    label: String,
    completed: bool,
}

#[derive(Deserialize, Serialize)]
struct StoredPlanLevel {
    instrument_id: String,
    label: String,
    price_units: i64,
    price_scale: u8,
}

fn encode_session_plan(plan: &SessionPlan) -> Result<String, String> {
    let payload = StoredSessionPlanPayload {
        bias: plan.bias.as_str().to_string(),
        maximum_loss_units: plan.maximum_loss.units(),
        maximum_loss_scale: plan.maximum_loss.scale(),
        session_start_realized_units: plan.session_start_realized_pnl.units(),
        session_start_realized_scale: plan.session_start_realized_pnl.scale(),
        allowed_setups: plan.allowed_setups.clone(),
        active_setup: plan.active_setup.clone(),
        checklist: plan
            .checklist
            .iter()
            .map(|item| StoredChecklistItem {
                item_id: item.item_id.clone(),
                label: item.label.clone(),
                completed: item.completed,
            })
            .collect(),
        levels: plan
            .levels
            .iter()
            .map(|level| StoredPlanLevel {
                instrument_id: level.instrument_id.as_str().to_string(),
                label: level.label.clone(),
                price_units: level.price.units(),
                price_scale: level.price.scale(),
            })
            .collect(),
    };
    serde_json::to_string(&payload)
        .map_err(|error| format!("session plan could not be encoded: {error}"))
}

fn decode_session_plan(
    account_id: TradingAccountId,
    plan_id: String,
    revision: u32,
    session_start_unix_nanos: i64,
    session_end_unix_nanos: i64,
    raw: &str,
) -> Result<SessionPlan, String> {
    let payload: StoredSessionPlanPayload = serde_json::from_str(raw)
        .map_err(|error| format!("stored session plan is invalid: {error}"))?;
    Ok(SessionPlan {
        account_id,
        plan_id,
        revision,
        session_start_unix_nanos,
        session_end_unix_nanos,
        bias: SessionBias::parse(&payload.bias)?,
        maximum_loss: fixed(payload.maximum_loss_units, payload.maximum_loss_scale)?,
        session_start_realized_pnl: fixed(
            payload.session_start_realized_units,
            payload.session_start_realized_scale,
        )?,
        allowed_setups: payload.allowed_setups,
        active_setup: payload.active_setup,
        checklist: payload
            .checklist
            .into_iter()
            .map(|item| SessionChecklistItem {
                item_id: item.item_id,
                label: item.label,
                completed: item.completed,
            })
            .collect(),
        levels: payload
            .levels
            .into_iter()
            .map(|level| {
                Ok(SessionPlanLevel {
                    instrument_id: InstrumentId::try_new(level.instrument_id)
                        .map_err(|error| error.to_string())?,
                    label: level.label,
                    price: fixed(level.price_units, level.price_scale)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
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
        "order_quantity_increment": decimal(contract.order_quantity_increment),
        "currency": contract.currency, "expiry": date(contract.expiry),
        "first_notice": date(contract.first_notice), "last_trade": date(contract.last_trade),
        "session_hours": contract.session_hours.iter().map(|value| json!({"weekday": value.weekday,
            "open_seconds": value.open_seconds, "close_seconds": value.close_seconds,
            "timezone": value.timezone})).collect::<Vec<_>>(),
        "provenance": {"provider_id": contract.provenance.provider_id,
            "provider_symbol": contract.provenance.provider_symbol,
            "display_symbol": contract.provenance.display_symbol,
            "session_generation": contract.provenance.session_generation}}).to_string())
}

fn decode_contract(raw: &str, instrument_id: &InstrumentId) -> Result<ContractMetadata, String> {
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
        order_quantity_increment: decode_decimal(value.get("order_quantity_increment"))?,
        currency: json_string(&value, "currency")?,
        expiry: decode_date(value.get("expiry"))?,
        first_notice: decode_date(value.get("first_notice"))?,
        last_trade: decode_date(value.get("last_trade"))?,
        session_hours: sessions,
        provenance: InstrumentMetadataProvenance {
            provider_id: json_string(provenance, "provider_id")?,
            provider_symbol: json_string(provenance, "provider_symbol")?,
            display_symbol: provenance
                .get("display_symbol")
                .and_then(Value::as_str)
                .or_else(|| instrument_id.as_str().strip_prefix("tastytrade:Future:/"))
                .or_else(|| provenance.get("provider_symbol").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string(),
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
        "demo" => Ok(AccountEnvironment::Demo),
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
        "pending_modify" => Ok(OrderStatus::PendingModify),
        "partially_filled" => Ok(OrderStatus::PartiallyFilled),
        "pending_cancel" => Ok(OrderStatus::PendingCancel),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        store.migrate().expect("schema five migration");

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
        store.migrate().expect("v18 migration");
        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 18);
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
        store
            .connection
            .pragma_update(None, "user_version", 19_u32)
            .expect("newer version");
        assert!(
            store
                .migrate()
                .expect_err("newer schema rejected")
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
        store.migrate().expect("current schema");
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
