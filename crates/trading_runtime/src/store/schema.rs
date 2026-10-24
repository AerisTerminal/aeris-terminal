use super::*;

pub const SCHEMA_VERSION: u32 = 29;
pub(super) const INITIAL_SCHEMA: &str = "CREATE TABLE metadata (
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
pub(super) const MIGRATION_V2: &str = "CREATE TABLE risk_profiles (
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
pub(super) const MIGRATION_V3: &str =
    "ALTER TABLE risk_profiles ADD COLUMN session_start_unix_nanos INTEGER NOT NULL DEFAULT 1;";
pub(super) const MIGRATION_V4: &str =
    "ALTER TABLE risk_profiles ADD COLUMN session_start_realized_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE risk_profiles ADD COLUMN session_start_realized_scale INTEGER NOT NULL DEFAULT 2;";
pub(super) const MIGRATION_V5: &str =
    "ALTER TABLE orders ADD COLUMN filled_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE orders ADD COLUMN filled_scale INTEGER NOT NULL DEFAULT 0;
 UPDATE orders SET filled_scale = quantity_scale;
 UPDATE orders SET filled_units = quantity_units WHERE status = 'filled';";
pub(super) const MIGRATION_V6: &str = "CREATE TABLE trade_copiers (
     source_account_id TEXT PRIMARY KEY REFERENCES accounts(id),
     revision INTEGER NOT NULL, enabled INTEGER NOT NULL
 ) STRICT;
 CREATE TABLE trade_copier_targets (
     source_account_id TEXT NOT NULL REFERENCES trade_copiers(source_account_id) ON DELETE CASCADE,
     target_account_id TEXT NOT NULL REFERENCES accounts(id), multiplier_units INTEGER NOT NULL,
     multiplier_scale INTEGER NOT NULL, enabled INTEGER NOT NULL,
     PRIMARY KEY(source_account_id, target_account_id)
 ) STRICT;";
pub(super) const MIGRATION_V7: &str = "CREATE TABLE strategy_templates (
     template_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, name TEXT NOT NULL,
     stop_offset_ticks INTEGER NOT NULL, targets_json TEXT NOT NULL,
     trailing_activation_ticks INTEGER, trailing_distance_ticks INTEGER,
     break_even_activation_ticks INTEGER, break_even_offset_ticks INTEGER,
     enabled INTEGER NOT NULL
 ) STRICT;";
pub(super) const MIGRATION_V8: &str = "CREATE TABLE managed_brackets (
     bracket_id TEXT PRIMARY KEY, template_json TEXT NOT NULL,
     entry_client_order_id TEXT NOT NULL UNIQUE, stop_client_order_id TEXT,
     target_client_order_ids_json TEXT NOT NULL, status TEXT NOT NULL,
     entry_price_units INTEGER, entry_price_scale INTEGER
 ) STRICT;";
pub(super) const MIGRATION_V9: &str = "CREATE TABLE protective_orders (
     client_order_id TEXT PRIMARY KEY REFERENCES orders(client_order_id) ON DELETE CASCADE,
     role TEXT NOT NULL
 ) STRICT;";
pub(super) const MIGRATION_V10: &str = "CREATE TABLE risk_rule_states (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), profile_id TEXT NOT NULL,
     profile_version INTEGER NOT NULL, peak_session_units INTEGER NOT NULL,
     peak_session_scale INTEGER NOT NULL
 ) STRICT;
 INSERT INTO risk_rule_states(account_id, profile_id, profile_version,
     peak_session_units, peak_session_scale)
 SELECT account_id, profile_id, version, 0, daily_loss_scale FROM risk_profiles;";
pub(super) const MIGRATION_V11: &str = "ALTER TABLE risk_rule_states
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
pub(super) const MIGRATION_V12: &str = "CREATE TABLE session_plans (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), plan_id TEXT NOT NULL,
     revision INTEGER NOT NULL, session_start_unix_nanos INTEGER NOT NULL,
     session_end_unix_nanos INTEGER NOT NULL, plan_json TEXT NOT NULL
 ) STRICT;";
pub(super) const MIGRATION_V13: &str = "ALTER TABLE risk_trade_cycles
     ADD COLUMN peak_quantity_units INTEGER NOT NULL DEFAULT 0;
 ALTER TABLE risk_trade_cycles ADD COLUMN peak_quantity_scale INTEGER NOT NULL DEFAULT 0;
 CREATE TABLE discipline_states (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id), rapid_loss_count INTEGER NOT NULL,
     loss_window_started_unix_nanos INTEGER, last_loss_unix_nanos INTEGER,
     post_loss_quantity_units INTEGER, post_loss_quantity_scale INTEGER,
     last_stop_fill_unix_nanos INTEGER, cooldown_until_unix_nanos INTEGER
 ) STRICT;";
pub(super) const MIGRATION_V14: &str = "ALTER TABLE risk_profiles ADD COLUMN event_action TEXT;
 ALTER TABLE risk_profiles ADD COLUMN event_minimum_importance INTEGER;
 ALTER TABLE risk_profiles ADD COLUMN event_lead_seconds INTEGER;
 CREATE TABLE economic_event_risk_actions (
     account_id TEXT NOT NULL REFERENCES accounts(id), event_id TEXT NOT NULL,
     profile_version INTEGER NOT NULL, action TEXT NOT NULL,
     applied_unix_nanos INTEGER NOT NULL,
     PRIMARY KEY(account_id, event_id, profile_version)
 ) STRICT;";
pub(super) const MIGRATION_V15: &str =
    "ALTER TABLE accounts ADD COLUMN starting_equity_units INTEGER;
 ALTER TABLE accounts ADD COLUMN starting_equity_scale INTEGER;";
pub(super) const MIGRATION_V16: &str =
    "ALTER TABLE fills ADD COLUMN completed_trade_pnl_units INTEGER;
 ALTER TABLE fills ADD COLUMN completed_trade_pnl_scale INTEGER;
 CREATE TABLE trade_pnl_cycles (
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id),
     realized_units INTEGER NOT NULL, realized_scale INTEGER NOT NULL,
     PRIMARY KEY(account_id, instrument_id)
 ) STRICT;";
pub(super) const MIGRATION_V17: &str = "ALTER TABLE fills ADD COLUMN realized_pnl_units INTEGER;
 ALTER TABLE fills ADD COLUMN realized_pnl_scale INTEGER;";
pub(super) const MIGRATION_V18: &str =
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
/// The venue generation survives restarts so a new relay session always advances it and a
/// retired session's events can never be mistaken for current ones.
pub(super) const MIGRATION_V19: &str =
    "INSERT INTO metadata(key, value) VALUES ('venue_generation', 0);";
/// Legacy instruments have no verified route and cannot be subscribed until Terms arrive.
pub(super) const MIGRATION_V20: &str = "ALTER TABLE instruments ADD COLUMN market_route_json TEXT;";
/// Preserve legacy P&L and cycle baselines without inventing missing per-fill values.
/// Superseded columns and tables remain on disk for a later released migration, but readers
/// after V21 use the new accounting rows exclusively.
pub(super) const MIGRATION_V21: &str = "
 CREATE TABLE fill_accounting (
     fill_id TEXT PRIMARY KEY REFERENCES fills(id) ON DELETE CASCADE,
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id),
     realized_gross_units INTEGER, realized_gross_scale INTEGER,
     commission_units INTEGER NOT NULL DEFAULT 0, commission_scale INTEGER NOT NULL DEFAULT 2,
     fees_units INTEGER NOT NULL DEFAULT 0, fees_scale INTEGER NOT NULL DEFAULT 2,
     cycle_id INTEGER NOT NULL DEFAULT 0, cycle_closed INTEGER NOT NULL DEFAULT 0,
     cycle_realized_units INTEGER, cycle_realized_scale INTEGER
 ) STRICT;
 CREATE TABLE trade_cycles (
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id),
     cycle_id INTEGER NOT NULL DEFAULT 0,
     peak_quantity_units INTEGER NOT NULL, peak_quantity_scale INTEGER NOT NULL,
     realized_units INTEGER NOT NULL, realized_scale INTEGER NOT NULL,
     risk_realized_units INTEGER NOT NULL, risk_realized_scale INTEGER NOT NULL,
     pnl_known INTEGER NOT NULL,
     PRIMARY KEY(account_id, instrument_id)
 ) STRICT;
 INSERT INTO fill_accounting(fill_id,account_id,instrument_id,realized_gross_units,
     realized_gross_scale,cycle_realized_units,cycle_realized_scale,cycle_closed)
 SELECT id,account_id,instrument_id,realized_pnl_units,realized_pnl_scale,
     completed_trade_pnl_units,completed_trade_pnl_scale,
     completed_trade_pnl_units IS NOT NULL FROM fills;
 INSERT INTO trade_cycles(account_id,instrument_id,peak_quantity_units,peak_quantity_scale,
     realized_units,realized_scale,risk_realized_units,risk_realized_scale,pnl_known)
 SELECT r.account_id,r.instrument_id,r.peak_quantity_units,r.peak_quantity_scale,
     COALESCE(t.realized_units,r.realized_units),
     COALESCE(t.realized_scale,r.realized_scale),
     r.realized_units,r.realized_scale,
     t.account_id IS NOT NULL
 FROM risk_trade_cycles r LEFT JOIN trade_pnl_cycles t
     ON t.account_id=r.account_id AND t.instrument_id=r.instrument_id;
 INSERT INTO trade_cycles(account_id,instrument_id,peak_quantity_units,peak_quantity_scale,
     realized_units,realized_scale,risk_realized_units,risk_realized_scale,pnl_known)
 SELECT t.account_id,t.instrument_id,0,0,t.realized_units,t.realized_scale,
     t.realized_units,t.realized_scale,1
 FROM trade_pnl_cycles t LEFT JOIN risk_trade_cycles r
     ON t.account_id=r.account_id AND t.instrument_id=r.instrument_id
 WHERE r.account_id IS NULL;
 CREATE INDEX orders_open_scope ON orders(account_id,instrument_id,status)
     WHERE status IN ('working','partially_filled','pending');
 CREATE INDEX fills_account_execution ON fills(account_id,instrument_id,execution_unix_nanos);
 CREATE INDEX order_events_order_sequence ON order_events(order_id,sequence);";
/// Lifecycle metadata for existing facts. Historical details remain display text;
/// only recognized legacy phrases acquire a stable code, never a guessed reason.
pub(super) const MIGRATION_V22: &str = "
 ALTER TABLE orders ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
 ALTER TABLE orders ADD COLUMN accepted_unix_nanos INTEGER;
 ALTER TABLE orders ADD COLUMN updated_unix_nanos INTEGER;
 ALTER TABLE orders ADD COLUMN triggered_unix_nanos INTEGER;
 ALTER TABLE orders ADD COLUMN terminal_unix_nanos INTEGER;
 ALTER TABLE orders ADD COLUMN expires_at_unix_nanos INTEGER;
 ALTER TABLE orders ADD COLUMN eligible_after_unix_nanos INTEGER;
 ALTER TABLE orders ADD COLUMN group_id TEXT;
 ALTER TABLE order_events ADD COLUMN reason_code TEXT;
 ALTER TABLE order_events ADD COLUMN reason_json TEXT;
 UPDATE orders SET accepted_unix_nanos=submitted_unix_nanos,
     updated_unix_nanos=submitted_unix_nanos,
     eligible_after_unix_nanos=submitted_unix_nanos,
     terminal_unix_nanos=CASE WHEN status IN ('filled','cancelled','rejected','expired')
         THEN COALESCE((SELECT MAX(event_unix_nanos) FROM order_events
             WHERE order_id=orders.id AND kind IN ('filled','cancelled','rejected','expired')),
             submitted_unix_nanos)
         ELSE NULL END;
 UPDATE order_events SET reason_code=CASE
     WHEN kind='filled' AND detail='touch fill' THEN 'touch_fill'
     WHEN kind='cancelled' AND detail='cancelled by local command' THEN 'user_cancel'
     WHEN kind='accepted' AND detail LIKE 'RISK WARNING%' THEN 'risk_warning'
     WHEN kind='accepted' AND detail LIKE 'LOCAL-MANAGED%' THEN 'local_managed'
     ELSE NULL END;";

/// Group rows are introduced before the owner switches away from the legacy writer.
/// Existing managed rows remain authoritative until the idempotent group backfill
/// creates their real member orders and switches the owner in a later M5.5 part.
pub(super) const MIGRATION_V23: &str = "
 CREATE TABLE order_groups (
     group_id TEXT PRIMARY KEY,
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL REFERENCES instruments(id),
     kind TEXT NOT NULL CHECK(kind IN ('bracket','oco','protection')),
     status TEXT NOT NULL CHECK(status IN ('pending','active','completed','cancelled')),
     entry_order_id TEXT REFERENCES orders(id),
     management_json TEXT NOT NULL,
     template_id TEXT,
     template_revision INTEGER,
     revision INTEGER NOT NULL CHECK(revision > 0),
     CHECK ((template_id IS NULL) = (template_revision IS NULL))
 ) STRICT;
 CREATE INDEX order_groups_scope ON order_groups(account_id,instrument_id,status);
 CREATE TABLE order_group_legs (
     group_id TEXT NOT NULL REFERENCES order_groups(group_id) ON DELETE CASCADE,
     order_id TEXT NOT NULL UNIQUE REFERENCES orders(id),
     role TEXT NOT NULL CHECK(role IN ('stop_loss','take_profit')),
     target_index INTEGER,
     offset_ticks INTEGER CHECK(offset_ticks > 0),
     allocation_percent INTEGER CHECK(allocation_percent BETWEEN 1 AND 100),
     PRIMARY KEY(group_id,order_id),
     CHECK ((role = 'stop_loss' AND target_index IS NULL)
         OR (role = 'take_profit' AND target_index IS NOT NULL))
 ) STRICT;";

/// The group owner replaced the legacy protection tables; they drop once the group backfill
/// ran and no awaiting legacy bracket still waits for the runtime conversion.
pub(super) const MIGRATION_V24: &str = "
 DROP TABLE managed_brackets;
 DROP TABLE protective_orders;";

/// The simulated cash ledger, per-account fee schedules and the exact integer cost basis
/// behind the displayed average entry. The instrument class and price-grid columns are
/// decoded from the contract document by the Rust backfill. The migration is a chain entry
/// reachable only after the gated V24 drop, so a store still holding awaiting legacy
/// brackets never sees these tables.
pub(super) const MIGRATION_V25: &str = "
 CREATE TABLE ledger_entries (
     id TEXT PRIMARY KEY,
     account_id TEXT NOT NULL REFERENCES accounts(id),
     kind TEXT NOT NULL,
     amount_units INTEGER NOT NULL, amount_scale INTEGER NOT NULL,
     fill_id TEXT REFERENCES fills(id),
     at_unix_nanos INTEGER NOT NULL
 ) STRICT;
 CREATE INDEX ledger_entries_account ON ledger_entries(account_id, at_unix_nanos);
 CREATE TABLE fee_schedules (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id),
     model_json TEXT NOT NULL
 ) STRICT;
 ALTER TABLE instruments ADD COLUMN instrument_class TEXT;
 ALTER TABLE instruments ADD COLUMN price_grid_json TEXT;
 ALTER TABLE positions ADD COLUMN cost_basis_hi INTEGER;
 ALTER TABLE positions ADD COLUMN cost_basis_lo INTEGER;
 ALTER TABLE positions ADD COLUMN cost_price_scale INTEGER;
 ALTER TABLE positions ADD COLUMN cost_quantity_scale INTEGER;";

/// Position and ledger checkpoints plus the durable equity history. Checkpoints are written
/// in the same transaction that prunes fills and ledger entries, so a crash leaves neither
/// the checkpoint nor the deletion behind. Chain entry after V25, so a store still gated on
/// legacy bracket conversion never sees these tables.
pub(super) const MIGRATION_V27: &str = "
 CREATE TABLE risk_sessions (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id),
     boundary TEXT NOT NULL,
     session_start_unix_nanos INTEGER NOT NULL,
     session_start_realized_units INTEGER NOT NULL,
     session_start_realized_scale INTEGER NOT NULL
 ) STRICT;
 INSERT INTO risk_sessions(account_id, boundary, session_start_unix_nanos,
     session_start_realized_units, session_start_realized_scale)
 SELECT account_id, 'utc_midnight', session_start_unix_nanos,
     session_start_realized_units, session_start_realized_scale FROM risk_profiles;
 ALTER TABLE risk_locks ADD COLUMN kind TEXT NOT NULL DEFAULT 'manual';
 UPDATE risk_locks SET kind = CASE
     WHEN reason = 'daily loss limit reached' THEN 'daily_loss_limit'
     WHEN reason = 'intraday trailing drawdown reached' THEN 'trailing_drawdown'
     WHEN reason = 'end_of_day trailing drawdown reached' THEN 'trailing_drawdown'
     WHEN reason = 'session plan maximum loss reached' THEN 'session_plan'
     WHEN reason = 'news/session restriction is active' THEN 'news_restriction'
     ELSE 'manual' END;";
/// Broker accounts are netted futures accounts: positions live in the shared `positions`
/// ledger, so the hedged `broker_positions` table and the per-deal position id go. Server
/// bracket levels persist with their entry order until the venue releases them.
pub(super) const MIGRATION_V28: &str = "
 DROP TABLE broker_positions;
 ALTER TABLE broker_deals DROP COLUMN broker_position_id;
 CREATE TABLE broker_bracket_levels (
     order_id TEXT NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
     side TEXT NOT NULL,
     ticks INTEGER NOT NULL,
     quantity_units INTEGER NOT NULL, quantity_scale INTEGER NOT NULL,
     PRIMARY KEY(order_id, side, ticks)
 ) STRICT;";
/// Broker recovery facts that must survive a restart: contracts whose deals or positions the
/// owner could not account (the earliest such deal floors the account's deal replay), and
/// reverses waiting for their exit, which the next start reports instead of completing.
pub(super) const MIGRATION_V29: &str = "
 CREATE TABLE broker_unresolved_contracts (
     account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
     instrument_id TEXT NOT NULL,
     first_deal_unix_nanos INTEGER,
     PRIMARY KEY(account_id, instrument_id)
 ) STRICT;
 CREATE TABLE broker_pending_reverses (
     account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
     instrument_id TEXT NOT NULL,
     requested_unix_nanos INTEGER NOT NULL,
     PRIMARY KEY(account_id, instrument_id)
 ) STRICT;";

pub(super) const MIGRATION_V26: &str = "
 CREATE TABLE position_checkpoints (
     account_id TEXT NOT NULL REFERENCES accounts(id),
     instrument_id TEXT NOT NULL,
     net_quantity_units INTEGER NOT NULL, net_quantity_scale INTEGER NOT NULL,
     average_entry_units INTEGER, average_entry_scale INTEGER,
     realized_units INTEGER NOT NULL, realized_scale INTEGER NOT NULL,
     cost_basis_hi INTEGER, cost_basis_lo INTEGER, cost_basis_scale INTEGER,
     last_fill_unix_nanos INTEGER NOT NULL,
     fill_count INTEGER NOT NULL,
     PRIMARY KEY (account_id, instrument_id)
 ) STRICT;
 CREATE TABLE ledger_checkpoints (
     account_id TEXT PRIMARY KEY REFERENCES accounts(id),
     balance_units INTEGER NOT NULL, balance_scale INTEGER NOT NULL,
     entry_count INTEGER NOT NULL,
     at_unix_nanos INTEGER NOT NULL
 ) STRICT;
 CREATE TABLE equity_samples (
     account_id TEXT NOT NULL REFERENCES accounts(id),
     at_unix_nanos INTEGER NOT NULL,
     equity_units INTEGER NOT NULL, equity_scale INTEGER NOT NULL,
     resolution TEXT NOT NULL,
     PRIMARY KEY (account_id, at_unix_nanos, resolution)
 ) STRICT;";

pub(super) struct Migration {
    pub(super) from: u32,
    pub(super) to: u32,
    pub(super) sql: &'static str,
}

pub(super) const MIGRATIONS: &[Migration] = &[
    Migration {
        from: 0,
        to: 1,
        sql: INITIAL_SCHEMA,
    },
    Migration {
        from: 1,
        to: 2,
        sql: MIGRATION_V2,
    },
    Migration {
        from: 2,
        to: 3,
        sql: MIGRATION_V3,
    },
    Migration {
        from: 3,
        to: 4,
        sql: MIGRATION_V4,
    },
    Migration {
        from: 4,
        to: 5,
        sql: MIGRATION_V5,
    },
    Migration {
        from: 5,
        to: 6,
        sql: MIGRATION_V6,
    },
    Migration {
        from: 6,
        to: 7,
        sql: MIGRATION_V7,
    },
    Migration {
        from: 7,
        to: 8,
        sql: MIGRATION_V8,
    },
    Migration {
        from: 8,
        to: 9,
        sql: MIGRATION_V9,
    },
    Migration {
        from: 9,
        to: 10,
        sql: MIGRATION_V10,
    },
    Migration {
        from: 10,
        to: 11,
        sql: MIGRATION_V11,
    },
    Migration {
        from: 11,
        to: 12,
        sql: MIGRATION_V12,
    },
    Migration {
        from: 12,
        to: 13,
        sql: MIGRATION_V13,
    },
    Migration {
        from: 13,
        to: 14,
        sql: MIGRATION_V14,
    },
    Migration {
        from: 14,
        to: 15,
        sql: MIGRATION_V15,
    },
    Migration {
        from: 15,
        to: 16,
        sql: MIGRATION_V16,
    },
    Migration {
        from: 16,
        to: 17,
        sql: MIGRATION_V17,
    },
    Migration {
        from: 17,
        to: 18,
        sql: MIGRATION_V18,
    },
    Migration {
        from: 18,
        to: 19,
        sql: MIGRATION_V19,
    },
    Migration {
        from: 19,
        to: 20,
        sql: MIGRATION_V20,
    },
    Migration {
        from: 20,
        to: 21,
        sql: MIGRATION_V21,
    },
    Migration {
        from: 21,
        to: 22,
        sql: MIGRATION_V22,
    },
    Migration {
        from: 22,
        to: 23,
        sql: MIGRATION_V23,
    },
    // 23 -> 24 is the gated group-table drop applied by finalize_group_migration; the chain
    // resumes at 24 once it landed.
    Migration {
        from: 24,
        to: 25,
        sql: MIGRATION_V25,
    },
    Migration {
        from: 25,
        to: 26,
        sql: MIGRATION_V26,
    },
    Migration {
        from: 26,
        to: 27,
        sql: MIGRATION_V27,
    },
    Migration {
        from: 27,
        to: 28,
        sql: MIGRATION_V28,
    },
    Migration {
        from: 28,
        to: 29,
        sql: MIGRATION_V29,
    },
];

pub(crate) fn preflight_existing_database(path: &Path) -> Result<(), StoreError> {
    const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
    let mut header = [0_u8; SQLITE_HEADER.len()];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| {
            corrupt_error(path, format!("SQLite header could not be read: {error}"))
        })?;
    if &header != SQLITE_HEADER {
        return Err(corrupt_error(path, "SQLite header is missing or invalid"));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| corrupt_error(path, "database path has no file name"))?;
    let scratch = parent.join(format!(
        ".{}.preflight-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        crate::clock::system_unix_nanos()
    ));
    fs::create_dir(&scratch).map_err(|error| {
        corrupt_error(
            path,
            format!("quick_check scratch directory could not be created: {error}"),
        )
    })?;
    let copy = scratch.join(file_name);
    let checked = (|| {
        fs::copy(path, &copy).map_err(|error| {
            corrupt_error(path, format!("quick_check database copy failed: {error}"))
        })?;
        for suffix in ["-wal", "-shm"] {
            let source = PathBuf::from(format!("{}{suffix}", path.display()));
            if source.is_file() {
                fs::copy(
                    &source,
                    scratch.join(format!("{}{suffix}", file_name.to_string_lossy())),
                )
                .map_err(|error| {
                    corrupt_error(path, format!("quick_check sidecar copy failed: {error}"))
                })?;
            }
        }
        preflight_database_copy(path, &copy)
    })();
    if let Err(error) = remove_preflight_scratch(&scratch, file_name) {
        aeris_observability::diagnostic!(
            "trading quick_check scratch cleanup failed for {}: {error}",
            scratch.display()
        );
    }
    checked
}

/// Only directories with the exact preflight name and expected SQLite copy files
/// are eligible. A foreign file or link is left untouched rather than recursively
/// deleting data that does not belong to this temporary copy.
pub(super) fn sweep_stale_preflight_directories(database: &Path) {
    const MAX_SWEEP: usize = 32;
    let Some(parent) = database.parent() else {
        return;
    };
    let Some(file_name) = database.file_name() else {
        return;
    };
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            aeris_observability::diagnostic!(
                "trading preflight scratch directory {} could not be read: {error}",
                parent.display()
            );
            return;
        }
    };
    let prefix = format!(".{}.preflight-", file_name.to_string_lossy());
    let mut candidates = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                aeris_observability::diagnostic!("trading preflight scratch scan failed: {error}");
                continue;
            }
        };
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        let Some((pid, nanos)) = suffix.split_once('-') else {
            continue;
        };
        if ![pid, nanos].iter().all(|part| {
            !part.is_empty() && part.len() <= 20 && part.bytes().all(|b| b.is_ascii_digit())
        }) {
            continue;
        }
        if candidates == MAX_SWEEP {
            aeris_observability::diagnostic!(
                "trading preflight scratch sweep deferred beyond {MAX_SWEEP} directories"
            );
            break;
        }
        candidates += 1;
        if let Err(error) = remove_preflight_scratch(&entry.path(), file_name) {
            aeris_observability::diagnostic!(
                "trading preflight scratch cleanup failed for {}: {error}",
                entry.path().display()
            );
        }
    }
}

pub(super) fn remove_preflight_scratch(
    scratch: &Path,
    database_name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(scratch)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "preflight scratch is not a plain directory",
        ));
    }
    let expected = [
        database_name.to_os_string(),
        format!("{}-wal", database_name.to_string_lossy()).into(),
        format!("{}-shm", database_name.to_string_lossy()).into(),
    ];
    let mut files = Vec::new();
    for entry in fs::read_dir(scratch)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !expected.contains(&entry.file_name()) {
            return Err(std::io::Error::other(
                "preflight scratch contains an unexpected file or link",
            ));
        }
        files.push(entry.path());
    }
    for file in files {
        fs::remove_file(file)?;
    }
    fs::remove_dir(scratch)
}

pub(super) fn preflight_database_copy(
    original_path: &Path,
    copy_path: &Path,
) -> Result<(), StoreError> {
    let connection = Connection::open_with_flags(copy_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| {
            corrupt_error(
                original_path,
                format!("quick_check could not open the file: {error}"),
            )
        })?;
    connection
        .busy_timeout(std::time::Duration::from_millis(5_000))
        .map_err(|error| {
            corrupt_error(
                original_path,
                format!("quick_check timeout setup failed: {error}"),
            )
        })?;
    let detail = connection
        .pragma_query_value(None, "quick_check", |row| row.get::<_, String>(0))
        .map_err(|error| corrupt_error(original_path, format!("quick_check failed: {error}")))?;
    if detail != "ok" {
        return Err(corrupt_error(original_path, detail));
    }
    let version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(|error| {
            corrupt_error(
                original_path,
                format!("schema version could not be read: {error}"),
            )
        })?;
    if version == 0 {
        let user_tables: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| {
                corrupt_error(
                    original_path,
                    format!("SQLite schema could not be inspected: {error}"),
                )
            })?;
        if user_tables == 0 {
            return Err(corrupt_error(
                original_path,
                "existing SQLite database has no Aeris schema",
            ));
        }
    }
    if version > SCHEMA_VERSION {
        return Err(StoreError::NewerSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

pub(super) fn retain_newest_backups(directory: &Path, retention: usize) -> Result<(), String> {
    let mut backups = fs::read_dir(directory)
        .map_err(|error| format!("trading backup directory could not be read: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("trading-v") && name.ends_with(".sqlite3"))
        })
        .collect::<Vec<_>>();
    backups.sort_by_key(|path| backup_unix(path).unwrap_or(0));
    let remove_count = backups.len().saturating_sub(retention);
    for backup in backups.into_iter().take(remove_count) {
        fs::remove_file(&backup).map_err(|error| {
            format!(
                "retired trading backup {} could not be removed: {error}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

impl TradingStore {
    pub(super) fn migrate(&mut self, back_up_existing_database: bool) -> Result<(), StoreError> {
        let mut version = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .map_err(|error| {
                StoreError::Other(format!("trading schema version could not be read: {error}"))
            })?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if back_up_existing_database && version < SCHEMA_VERSION {
            self.back_up_before_migration(version)?;
        }
        // The V24 drop is not in this chain: it lands only after the group backfill and the
        // runtime conversion of awaiting brackets, via the finalize step below.
        let chain_target = MIGRATIONS.last().map_or(0, |migration| migration.to);
        while version < chain_target {
            let Some(migration) = MIGRATIONS
                .iter()
                .find(|migration| migration.from == version)
            else {
                // 23 -> 24 is the gated group-table drop below; open() resumes this chain
                // once the finalize step committed it.
                if version == 23 {
                    break;
                }
                return Err(StoreError::Other(format!(
                    "trading schema migration from version {version} is missing"
                )));
            };
            self.apply_migration(migration.to, migration.sql)
                .map_err(StoreError::Other)?;
            version = migration.to;
        }
        Ok(())
    }

    /// Runs the migration steps that must follow the group backfill and the runtime
    /// conversion of awaiting legacy brackets: the gated V24 drop, the rest of the chain
    /// and the V25 ledger backfill. Idempotent; a store still gated on awaiting conversions
    /// stays at version 23.
    pub(crate) fn complete_post_group_migrations(&mut self) -> Result<(), String> {
        self.finalize_group_migration()?;
        self.migrate(false).map_err(|error| error.to_string())?;
        self.backfill_ledger()
    }

    /// Drops the pre-group protection tables once no awaiting legacy bracket still waits for
    /// the runtime conversion, committing schema version 24. Idempotent: a store whose
    /// tables already dropped returns without writing.
    pub(crate) fn finalize_group_migration(&mut self) -> Result<(), String> {
        if !self.legacy_protection_tables_present()? {
            return Ok(());
        }
        let awaiting = self
            .connection
            .query_row(
                "SELECT count(*) FROM managed_brackets WHERE status = 'awaiting_entry'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(super::database_error)?;
        if awaiting != 0 {
            return Ok(());
        }
        self.apply_migration(24, MIGRATION_V24)
    }

    pub(super) fn back_up_before_migration(&self, old_version: u32) -> Result<(), StoreError> {
        let database_path = self.connection.path().map(Path::new);
        let Some(database_path) = database_path
            .filter(|path| !path.as_os_str().is_empty() && *path != Path::new(":memory:"))
        else {
            return Ok(());
        };
        let trading_directory = database_path.parent().ok_or_else(|| {
            StoreError::Other("trading database has no parent directory".to_string())
        })?;
        let backups = trading_directory.join("backups");
        fs::create_dir_all(&backups).map_err(|error| {
            StoreError::Other(format!(
                "trading backup directory could not be created: {error}"
            ))
        })?;
        let unix = crate::clock::system_unix_nanos() / 1_000_000;
        let backup = backups.join(format!("trading-v{old_version}-{unix}.sqlite3"));
        self.connection
            .execute("VACUUM INTO ?1", params![backup.to_string_lossy().as_ref()])
            .map_err(|error| {
                StoreError::Other(format!(
                    "trading migration backup could not be created: {error}"
                ))
            })?;
        retain_newest_backups(&backups, 3).map_err(StoreError::Other)
    }
}

impl TradingStore {
    pub(super) fn apply_migration(&mut self, version: u32, sql: &str) -> Result<(), String> {
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
}
