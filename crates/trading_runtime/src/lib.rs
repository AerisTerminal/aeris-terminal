//! Single in-process owner for broker-neutral trading state and simulated execution.
//!
//! The service owns its bounded command queue, canonical orders/fills/positions, and the one
//! embedded user-record store. Callers must invoke blocking request methods from background work.

mod copier;
mod discipline;
mod plan;
mod risk;
mod store;
mod strategy;

use aeris_instruments::{ContractMetadata, InstrumentId};
use aeris_trading::{
    AccountEnvironment, AccountPnl, ClientOrderId, Fill, FillId, FixedPoint, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position,
    TimeInForce, TradingAccount, TradingAccountId, TradingProvenance, project_unrealized_pnl,
};
pub use copier::{MAXIMUM_COPIER_TARGETS, TradeCopierConfig, TradeCopierTarget, TradeCopyDispatch};
pub use discipline::{
    DisciplineState, FAST_STOP_REENTRY_NANOS, RAPID_LOSS_COOLDOWN_NANOS, RAPID_LOSS_THRESHOLD,
    RAPID_LOSS_WINDOW_NANOS,
};
pub use plan::{
    MAXIMUM_ALLOWED_SETUPS, MAXIMUM_CHECKLIST_ITEMS, MAXIMUM_PLAN_LEVELS, SessionAdherenceReview,
    SessionBias, SessionChecklistItem, SessionPlan, SessionPlanLevel,
};
use risk::RiskTradeCycleState;
pub use risk::{
    EconomicEventRiskAction, EconomicEventRiskImportance, EconomicEventRiskOutcome,
    EconomicEventRiskRule, EconomicEventRiskTrigger, RiskEvaluation, RiskLock, RiskMeter,
    RiskProfile, RiskRuleState, TrailingDrawdownMode,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};
use store::{FillPolicyPersistence, SCHEMA_VERSION, StoredState, TradingStore};
pub use strategy::{
    BracketStrategyTemplate, BracketTarget, BreakEvenRule, MAXIMUM_BRACKET_TARGETS,
    MAXIMUM_MANAGED_BRACKETS, MAXIMUM_STRATEGY_TEMPLATES, ManagedBracket, ManagedBracketStatus,
    ProtectiveOrder, ProtectiveOrderRole, TrailingStopRule,
};

const COMMAND_CAPACITY: usize = 256;
const REPLY_CAPACITY: usize = 1;
const MAXIMUM_OPEN_ORDERS: usize = 4_096;
const MAXIMUM_SNAPSHOT_ITEMS: usize = 10_000;
const MAXIMUM_COPY_DISPATCHES: usize = 128;
const MAXIMUM_USER_RECORD_BYTES: usize = 1024 * 1024;
type Reply<T> = SyncSender<Result<T, String>>;

/// One local-store retention policy. These values are user-visible runtime settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TradingRetention {
    pub maximum_orders: usize,
    pub maximum_fills: usize,
    pub maximum_order_events: usize,
    pub maximum_user_records_per_kind: usize,
}

impl Default for TradingRetention {
    fn default() -> Self {
        Self {
            maximum_orders: 250_000,
            maximum_fills: 250_000,
            maximum_order_events: 500_000,
            maximum_user_records_per_kind: 25_000,
        }
    }
}

impl TradingRetention {
    fn validate(self) -> Result<Self, String> {
        if self.maximum_orders < MAXIMUM_OPEN_ORDERS
            || self.maximum_fills == 0
            || self.maximum_order_events == 0
            || self.maximum_user_records_per_kind == 0
        {
            return Err(format!(
                "trading retention limits must be positive and retain at least {MAXIMUM_OPEN_ORDERS} orders"
            ));
        }
        Ok(self)
    }
}

/// Startup configuration for the single trading owner.
#[derive(Clone, Debug)]
pub struct TradingServiceConfig {
    pub database_path: PathBuf,
    pub retention: TradingRetention,
}

/// Fixed-point contract terms retained by the trading owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingInstrument {
    pub instrument_id: InstrumentId,
    pub price_scale: u8,
    pub quantity_scale: u8,
    pub contract: ContractMetadata,
}

/// A new order command. Canonical identifiers and event ordering are assigned by the owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceOrder {
    pub client_order_id: ClientOrderId,
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub quantity: FixedPoint,
    pub limit_price: Option<FixedPoint>,
    pub stop_price: Option<FixedPoint>,
    pub submitted_unix_nanos: i64,
    pub provenance: TradingProvenance,
}

/// Entry command for one durable locally managed bracket strategy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceBracket {
    pub entry: PlaceOrder,
    pub template_id: String,
}

/// One chart-authored bracket whose exact stop and target distances are snapshotted inline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceInlineBracket {
    pub entry: PlaceOrder,
    pub template: BracketStrategyTemplate,
}

/// One standalone reduce-only protection command from a chart or order surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceProtectiveOrder {
    pub order: PlaceOrder,
    pub role: ProtectiveOrderRole,
}

/// Modification of one working order without changing its stable client identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModifyOrder {
    pub client_order_id: ClientOrderId,
    pub time_in_force: TimeInForce,
    pub limit_price: Option<FixedPoint>,
    pub stop_price: Option<FixedPoint>,
    pub modified_unix_nanos: i64,
    pub provenance: TradingProvenance,
}

/// One complete top-of-book observation used by the touch-fill simulator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulatedMarketObservation {
    pub instrument_id: InstrumentId,
    pub bid: FixedPoint,
    pub ask: FixedPoint,
    pub provenance: TradingProvenance,
}

/// User-owned record classes stored by PF7.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum UserRecordKind {
    JournalEntry,
    Tag,
    Note,
    Screenshot,
    RuleProfile,
    RuleEvaluation,
    SessionPlan,
    AnalyticsCache,
}

impl UserRecordKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JournalEntry => "journal_entry",
            Self::Tag => "tag",
            Self::Note => "note",
            Self::Screenshot => "screenshot",
            Self::RuleProfile => "rule_profile",
            Self::RuleEvaluation => "rule_evaluation",
            Self::SessionPlan => "session_plan",
            Self::AnalyticsCache => "analytics_cache",
        }
    }
}

/// One bounded JSON record. Screenshot records contain a local path and metadata, not image bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserRecord {
    pub id: String,
    pub kind: UserRecordKind,
    pub revision: u64,
    pub updated_unix_nanos: i64,
    pub json: String,
}

impl UserRecord {
    fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.id.len() > 256 {
            return Err("user record identity is invalid".to_string());
        }
        if self.revision == 0 || self.updated_unix_nanos <= 0 {
            return Err("user record revision and timestamp must be positive".to_string());
        }
        if self.json.len() > MAXIMUM_USER_RECORD_BYTES {
            return Err("user record exceeds the byte limit".to_string());
        }
        serde_json::from_str::<serde_json::Value>(&self.json)
            .map_err(|_| "user record must contain valid JSON".to_string())?;
        Ok(())
    }
}

/// Bounded immutable projection of current trading state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingSnapshot {
    pub revision: u64,
    pub accounts: Vec<TradingAccount>,
    pub orders: Vec<Order>,
    pub order_events: Vec<OrderEvent>,
    pub fills: Vec<Fill>,
    pub positions: Vec<Position>,
    pub position_pnl: Vec<PositionPnl>,
    pub account_pnl: Vec<AccountPnl>,
    pub risk_profiles: Vec<RiskProfile>,
    pub risk_meters: Vec<RiskMeter>,
    pub risk_locks: Vec<RiskLock>,
    pub risk_rule_states: Vec<RiskRuleState>,
    pub discipline_states: Vec<DisciplineState>,
    pub session_plans: Vec<SessionPlan>,
    pub session_adherence_reviews: Vec<SessionAdherenceReview>,
    pub trade_copiers: Vec<TradeCopierConfig>,
    pub copy_dispatches: Vec<TradeCopyDispatch>,
    pub strategy_templates: Vec<BracketStrategyTemplate>,
    pub managed_brackets: Vec<ManagedBracket>,
    pub protective_orders: Vec<ProtectiveOrder>,
}

/// Position-level currency and tick P/L projection for bounded desktop panels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionPnl {
    pub position: Position,
    /// Total closed P/L expressed as tick-contracts when contract metadata permits an exact value.
    pub realized_ticks: Option<FixedPoint>,
    /// Mark-to-market P/L expressed as tick-contracts when contract metadata permits an exact value.
    pub unrealized_ticks: Option<FixedPoint>,
    /// Exact contract multiplier used by bounded price-ladder projections.
    pub point_value: Option<FixedPoint>,
    /// Account currency precision for price-ladder projections.
    pub currency_scale: u8,
}

/// Runtime and store health visible to diagnostics/readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TradingServiceStatus {
    pub schema_version: u32,
    pub revision: u64,
    pub account_count: usize,
    pub working_order_count: usize,
    pub position_count: usize,
    pub retention: TradingRetention,
}

/// Cloneable command handle for the one in-process trading owner.
#[derive(Clone)]
pub struct TradingService {
    commands: SyncSender<Command>,
    runtime: Arc<TradingRuntime>,
}

struct TradingRuntime {
    stopping: AtomicBool,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

enum Command {
    Status(Reply<TradingServiceStatus>),
    RegisterAccount(TradingAccount, Reply<()>),
    RegisterInstrument(TradingInstrument, Reply<()>),
    Place(PlaceOrder, Reply<Order>),
    PlaceBracket(PlaceBracket, Reply<ManagedBracket>),
    PlaceInlineBracket(PlaceInlineBracket, Reply<ManagedBracket>),
    PlaceProtective(PlaceProtectiveOrder, Reply<Order>),
    EvaluateRisk(PlaceOrder, Reply<RiskEvaluation>),
    Modify(ModifyOrder, Reply<Order>),
    Cancel(ClientOrderId, Reply<Order>),
    CancelAll(Option<TradingAccountId>, Reply<Vec<Order>>),
    RegisterRiskProfile(RiskProfile, Reply<()>),
    RegisterSessionPlan(SessionPlan, Reply<()>),
    RegisterTradeCopier(TradeCopierConfig, Reply<()>),
    RegisterStrategyTemplate(BracketStrategyTemplate, Reply<()>),
    LockAccount(TradingAccountId, String, i64, Reply<()>),
    UnlockAccount(TradingAccountId, Reply<()>),
    KillSwitch(Option<TradingAccountId>, String, i64, Reply<usize>),
    Flatten(
        TradingAccountId,
        SimulatedMarketObservation,
        Reply<Vec<Fill>>,
    ),
    FlattenAll(SimulatedMarketObservation, Reply<Vec<Fill>>),
    ApplyEconomicEventRisk(
        EconomicEventRiskTrigger,
        Option<SimulatedMarketObservation>,
        Reply<EconomicEventRiskOutcome>,
    ),
    Observe(SimulatedMarketObservation, Reply<Vec<Fill>>),
    PutUserRecord(UserRecord, Reply<()>),
    Snapshot(Reply<TradingSnapshot>),
    Export(PathBuf, Reply<()>),
    Shutdown,
}

struct Coordinator {
    store: TradingStore,
    state: StoredState,
    retention: TradingRetention,
    copy_dispatches: std::collections::VecDeque<TradeCopyDispatch>,
}

struct FillMemoryUpdate {
    position: Position,
    position_key: (TradingAccountId, InstrumentId),
    policy_update: FillPolicyUpdate,
    next_sequence: u64,
}

struct FillPolicyUpdate {
    rule_state: Option<RiskRuleState>,
    trade_cycle: RiskTradeCycleState,
    discipline_state: DisciplineState,
}

impl TradingService {
    /// Opens/migrates the local store and starts the single trading owner.
    ///
    /// This performs disk I/O and must run before GPUI starts or on background work.
    ///
    /// # Errors
    /// Returns an error when configuration, storage, migration, or worker startup fails.
    pub fn start(config: TradingServiceConfig) -> Result<Self, String> {
        let retention = config.retention.validate()?;
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (started_tx, started_rx) = mpsc::sync_channel(REPLY_CAPACITY);
        let database_path = config.database_path;
        let worker = thread::Builder::new()
            .name("aeris-trading-owner".to_string())
            .spawn(move || {
                let coordinator =
                    TradingStore::open(&database_path, retention).and_then(|mut store| {
                        store.load_state().map(|state| Coordinator {
                            store,
                            state,
                            retention,
                            copy_dispatches: std::collections::VecDeque::new(),
                        })
                    });
                match coordinator {
                    Ok(mut coordinator) => {
                        if started_tx.send(Ok(())).is_ok() {
                            coordinator.run(&receiver);
                        }
                    }
                    Err(error) => {
                        let _ = started_tx.send(Err(error));
                    }
                }
            })
            .map_err(|error| format!("trading owner could not start: {error}"))?;
        started_rx
            .recv()
            .map_err(|_| "trading owner stopped during startup".to_string())??;
        Ok(Self {
            commands,
            runtime: Arc::new(TradingRuntime {
                stopping: AtomicBool::new(false),
                worker: Mutex::new(Some(worker)),
            }),
        })
    }

    /// Returns a bounded health snapshot.
    ///
    /// # Errors
    /// Returns an error when the bounded owner queue is full or disconnected.
    pub fn status(&self) -> Result<TradingServiceStatus, String> {
        self.request(Command::Status)
    }

    /// Adds or replaces one canonical account.
    ///
    /// # Errors
    /// Returns an error for invalid account data, overload, or a storage failure.
    pub fn register_account(&self, account: TradingAccount) -> Result<(), String> {
        self.request(|reply| Command::RegisterAccount(account, reply))
    }

    /// Adds or replaces provider-sourced contract terms.
    ///
    /// # Errors
    /// Returns an error for invalid metadata, overload, or a storage failure.
    pub fn register_instrument(&self, instrument: TradingInstrument) -> Result<(), String> {
        self.request(|reply| Command::RegisterInstrument(instrument, reply))
    }

    /// Submits one order through the authoritative trading command path.
    ///
    /// # Errors
    /// Returns an error when validation, routing, capacity, or persistence fails.
    pub fn place_order(&self, order: PlaceOrder) -> Result<Order, String> {
        self.request(|reply| Command::Place(order, reply))
    }

    /// Submits one entry whose stop and scale-out targets activate after its fill.
    ///
    /// # Errors
    /// Returns an error when the template, entry, allocation, risk, or persistence is invalid.
    pub fn place_bracket(&self, bracket: PlaceBracket) -> Result<ManagedBracket, String> {
        self.request(|reply| Command::PlaceBracket(bracket, reply))
    }

    /// Submits a chart-authored bracket with exact, inline tick offsets.
    ///
    /// # Errors
    /// Returns an error when the entry, template, risk, or persistence is invalid.
    pub fn place_inline_bracket(
        &self,
        bracket: PlaceInlineBracket,
    ) -> Result<ManagedBracket, String> {
        self.request(|reply| Command::PlaceInlineBracket(bracket, reply))
    }

    /// Submits one reduce-only protective stop or target for an existing position.
    ///
    /// # Errors
    /// Returns an error unless the order strictly reduces a canonical open position.
    pub fn place_protective_order(&self, order: PlaceProtectiveOrder) -> Result<Order, String> {
        self.request(|reply| Command::PlaceProtective(order, reply))
    }

    /// Runs the same authoritative pre-trade checks used by order placement.
    ///
    /// # Errors
    /// Returns an error when the account is locked, the order violates a rule, or the owner queue
    /// is unavailable.
    pub fn evaluate_risk(&self, order: PlaceOrder) -> Result<RiskEvaluation, String> {
        self.request(|reply| Command::EvaluateRisk(order, reply))
    }

    /// Modifies a working order through the same bounded owner queue.
    ///
    /// # Errors
    /// Returns an error when the order is missing, not working, invalid, or the owner is busy.
    pub fn modify_order(&self, order: ModifyOrder) -> Result<Order, String> {
        self.request(|reply| Command::Modify(order, reply))
    }

    /// Cancels one working order through the authoritative command path.
    ///
    /// # Errors
    /// Returns an error when the order is missing, storage fails, or the owner is unavailable.
    pub fn cancel_order(&self, client_order_id: ClientOrderId) -> Result<Order, String> {
        self.request(|reply| Command::Cancel(client_order_id, reply))
    }

    /// Cancels every working order for one account, or all accounts when no account is supplied.
    ///
    /// # Errors
    /// Returns an error when cancellation persistence fails or the owner queue is unavailable.
    pub fn cancel_all(&self, account_id: Option<TradingAccountId>) -> Result<Vec<Order>, String> {
        self.request(|reply| Command::CancelAll(account_id, reply))
    }

    /// Installs a versioned deterministic pre-trade rule profile.
    ///
    /// # Errors
    /// Returns an error when the profile is invalid, its account is unknown, or storage fails.
    pub fn register_risk_profile(&self, profile: RiskProfile) -> Result<(), String> {
        self.request(|reply| Command::RegisterRiskProfile(profile, reply))
    }

    /// Installs one revisioned session plan and pre-trade checklist for an account.
    ///
    /// # Errors
    /// Returns an error when the plan is invalid, stale, references an unknown account, or fails
    /// to persist.
    pub fn register_session_plan(&self, plan: SessionPlan) -> Result<(), String> {
        self.request(|reply| Command::RegisterSessionPlan(plan, reply))
    }

    /// Installs one durable bounded multi-account copier configuration.
    ///
    /// # Errors
    /// Returns an error when accounts, multipliers, target bounds, or persistence are invalid.
    pub fn register_trade_copier(&self, config: TradeCopierConfig) -> Result<(), String> {
        self.request(|reply| Command::RegisterTradeCopier(config, reply))
    }

    /// Adds or replaces one durable local bracket-strategy template.
    ///
    /// # Errors
    /// Returns an error when validation, revision ordering, persistence, or the owner queue fails.
    pub fn register_strategy_template(
        &self,
        template: BracketStrategyTemplate,
    ) -> Result<(), String> {
        self.request(|reply| Command::RegisterStrategyTemplate(template, reply))
    }

    /// Persists a hard account lock. Every order surface observes the same lock.
    ///
    /// # Errors
    /// Returns an error when the account is unknown, the lock is invalid, or storage fails.
    pub fn lock_account(
        &self,
        account_id: TradingAccountId,
        reason: String,
        locked_at_unix_nanos: i64,
    ) -> Result<(), String> {
        self.request(|reply| Command::LockAccount(account_id, reason, locked_at_unix_nanos, reply))
    }

    /// Clears an account lock through the owner. Callers still need an explicit user action.
    ///
    /// # Errors
    /// Returns an error when storage fails or the owner is unavailable.
    pub fn unlock_account(&self, account_id: TradingAccountId) -> Result<(), String> {
        self.request(|reply| Command::UnlockAccount(account_id, reply))
    }

    /// Locks one account or every account and cancels its working orders atomically on the owner.
    ///
    /// # Errors
    /// Returns an error when no target account exists, locking fails, or cancellation fails.
    pub fn kill_switch(
        &self,
        account_id: Option<TradingAccountId>,
        reason: String,
        locked_at_unix_nanos: i64,
    ) -> Result<usize, String> {
        self.request(|reply| Command::KillSwitch(account_id, reason, locked_at_unix_nanos, reply))
    }

    /// Cancels working orders and closes one account's simulated positions at the observed BBO.
    ///
    /// # Errors
    /// Returns an error when the observation is invalid, the account is unknown, or persistence
    /// fails. Flattening is an administrative action and remains available while risk-locked.
    pub fn flatten_account(
        &self,
        account_id: TradingAccountId,
        observation: SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        self.request(|reply| Command::Flatten(account_id, observation, reply))
    }

    /// Cancels working orders and closes simulated positions for every account at the observed BBO.
    ///
    /// # Errors
    /// Returns an error when the observation is invalid, no accounts are registered, or
    /// persistence fails. This administrative action remains available while risk-locked.
    pub fn flatten_all(
        &self,
        observation: SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        self.request(|reply| Command::FlattenAll(observation, reply))
    }

    /// Applies every enabled account event rule once, durably fenced by event/profile identity.
    /// A flatten rule remains pending until the caller supplies a valid market observation.
    ///
    /// # Errors
    /// Returns an error when the trigger or observation is invalid, a complete flatten cannot be
    /// priced, the runtime is overloaded, or durable rule state cannot be written.
    pub fn apply_economic_event_risk(
        &self,
        event: EconomicEventRiskTrigger,
        observation: Option<SimulatedMarketObservation>,
    ) -> Result<EconomicEventRiskOutcome, String> {
        self.request(|reply| Command::ApplyEconomicEventRisk(event, observation, reply))
    }

    /// Applies one market observation and returns fills produced by the simulated venue.
    ///
    /// # Errors
    /// Returns an error when the observation is invalid or a fill cannot be persisted exactly.
    pub fn observe_market(
        &self,
        observation: SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        self.request(|reply| Command::Observe(observation, reply))
    }

    /// Stores one user-owned JSON record on the background owner.
    ///
    /// # Errors
    /// Returns an error for invalid JSON, overload, or a storage failure.
    pub fn put_user_record(&self, record: UserRecord) -> Result<(), String> {
        self.request(|reply| Command::PutUserRecord(record, reply))
    }

    /// Returns a bounded immutable view.
    ///
    /// # Errors
    /// Returns an error when the owner is unavailable or the snapshot bound is exceeded.
    pub fn snapshot(&self) -> Result<TradingSnapshot, String> {
        self.request(Command::Snapshot)
    }

    /// Exports user-owned records to CSV and JSON files in `directory`.
    ///
    /// # Errors
    /// Returns an error when the owner is unavailable or export I/O fails.
    pub fn export(&self, directory: PathBuf) -> Result<(), String> {
        self.request(|reply| Command::Export(directory, reply))
    }

    /// Stops and joins the owner within a bounded deadline.
    ///
    /// # Errors
    /// Returns an error for overload, a repeated shutdown, timeout, or worker panic.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        if self.runtime.stopping.swap(true, Ordering::AcqRel) {
            return Err("trading owner shutdown is already in progress".to_string());
        }
        match self.commands.try_send(Command::Shutdown) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.runtime.stopping.store(false, Ordering::Release);
                return Err("trading command queue is full during shutdown".to_string());
            }
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "trading shutdown deadline overflowed".to_string())?;
        loop {
            let finished = self
                .runtime
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_none_or(thread::JoinHandle::is_finished);
            if finished {
                let worker = self
                    .runtime
                    .worker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                return if worker.is_some_and(|worker| worker.join().is_err()) {
                    Err("trading owner panicked during shutdown".to_string())
                } else {
                    Ok(())
                };
            }
            let now = Instant::now();
            if now >= deadline {
                return Err("trading owner shutdown deadline expired".to_string());
            }
            thread::sleep(Duration::from_millis(5).min(deadline.duration_since(now)));
        }
    }

    fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        if self.runtime.stopping.load(Ordering::Acquire) {
            return Err("trading owner is stopping".to_string());
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(REPLY_CAPACITY);
        match self.commands.try_send(command(reply_tx)) {
            Ok(()) => reply_rx
                .recv()
                .map_err(|_| "trading owner stopped before replying".to_string())?,
            Err(TrySendError::Full(_)) => Err("trading command queue is full".to_string()),
            Err(TrySendError::Disconnected(_)) => Err("trading owner is unavailable".to_string()),
        }
    }
}

impl Drop for TradingRuntime {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
    }
}

impl Coordinator {
    fn run(&mut self, commands: &Receiver<Command>) {
        while let Ok(command) = commands.recv() {
            match command {
                Command::Status(reply) => {
                    let _ = reply.send(Ok(self.status()));
                }
                Command::RegisterAccount(account, reply) => {
                    let _ = reply.send(self.register_account(account));
                }
                Command::RegisterInstrument(instrument, reply) => {
                    let _ = reply.send(self.register_instrument(instrument));
                }
                Command::Place(order, reply) => {
                    let _ = reply.send(self.place_order(&order));
                }
                Command::PlaceBracket(bracket, reply) => {
                    let _ = reply.send(self.place_bracket(&bracket));
                }
                Command::PlaceInlineBracket(bracket, reply) => {
                    let _ = reply
                        .send(self.place_bracket_with_template(&bracket.entry, bracket.template));
                }
                Command::PlaceProtective(order, reply) => {
                    let _ = reply.send(self.place_protective_order(order));
                }
                Command::EvaluateRisk(order, reply) => {
                    let _ = reply.send(self.evaluate_order_risk(&order));
                }
                Command::Modify(order, reply) => {
                    let _ = reply.send(self.modify_order(order));
                }
                Command::Cancel(client_order_id, reply) => {
                    let _ = reply.send(self.cancel_order(&client_order_id));
                }
                Command::CancelAll(account_id, reply) => {
                    let _ = reply.send(self.cancel_all(account_id.as_ref()));
                }
                Command::RegisterRiskProfile(profile, reply) => {
                    let _ = reply.send(self.register_risk_profile(profile));
                }
                Command::RegisterSessionPlan(plan, reply) => {
                    let _ = reply.send(self.register_session_plan(plan));
                }
                Command::RegisterTradeCopier(config, reply) => {
                    let _ = reply.send(self.register_trade_copier(config));
                }
                Command::RegisterStrategyTemplate(template, reply) => {
                    let _ = reply.send(self.register_strategy_template(template));
                }
                Command::LockAccount(account_id, reason, locked_at, reply) => {
                    let _ = reply.send(self.lock_account(account_id, reason, locked_at));
                }
                Command::UnlockAccount(account_id, reply) => {
                    let _ = reply.send(self.unlock_account(&account_id));
                }
                Command::KillSwitch(account_id, reason, locked_at, reply) => {
                    let _ = reply.send(self.kill_switch(account_id.as_ref(), &reason, locked_at));
                }
                Command::Flatten(account_id, observation, reply) => {
                    let _ = reply.send(self.flatten_account(&account_id, &observation));
                }
                Command::FlattenAll(observation, reply) => {
                    let _ = reply.send(self.flatten_all(&observation));
                }
                Command::ApplyEconomicEventRisk(event, observation, reply) => {
                    let _ =
                        reply.send(self.apply_economic_event_risk(&event, observation.as_ref()));
                }
                Command::Observe(observation, reply) => {
                    let _ = reply.send(self.observe_market(&observation));
                }
                Command::PutUserRecord(record, reply) => {
                    let result = record
                        .validate()
                        .and_then(|()| self.store.put_user_record(&record))
                        .and_then(|()| self.store.enforce_retention());
                    let _ = reply.send(result);
                }
                Command::Snapshot(reply) => {
                    let _ = reply.send(self.snapshot());
                }
                Command::Export(directory, reply) => {
                    let _ = reply.send(self.store.export(&directory));
                }
                Command::Shutdown => break,
            }
        }
    }

    fn status(&self) -> TradingServiceStatus {
        TradingServiceStatus {
            schema_version: SCHEMA_VERSION,
            revision: self.state.revision,
            account_count: self.state.accounts.len(),
            working_order_count: self
                .state
                .orders
                .values()
                .filter(|order| order.status.is_open())
                .count(),
            position_count: self.state.positions.len(),
            retention: self.retention,
        }
    }

    fn register_account(&mut self, account: TradingAccount) -> Result<(), String> {
        account.validate().map_err(|error| error.to_string())?;
        self.store.put_account(&account)?;
        self.state.accounts.insert(account.id.clone(), account);
        self.bump_revision()
    }

    fn register_instrument(&mut self, instrument: TradingInstrument) -> Result<(), String> {
        if instrument.price_scale > 18 || instrument.quantity_scale > 18 {
            return Err("instrument trading scale exceeds 18".to_string());
        }
        instrument
            .contract
            .validate()
            .map_err(|error| error.to_string())?;
        self.store.put_instrument(&instrument)?;
        self.state
            .instruments
            .insert(instrument.instrument_id.clone(), instrument);
        self.bump_revision()
    }

    fn register_risk_profile(&mut self, profile: RiskProfile) -> Result<(), String> {
        profile.validate()?;
        if !self.state.accounts.contains_key(&profile.account_id) {
            return Err("risk profile account is not registered".to_string());
        }
        if self
            .state
            .risk_profiles
            .get(&profile.account_id)
            .is_some_and(|current| {
                current.profile_id == profile.profile_id && profile.version <= current.version
            })
        {
            return Err("risk profile revision must advance".to_string());
        }
        let current = self.session_pnl(&profile)?;
        let zero = FixedPoint::try_new(0, profile.daily_loss_limit.scale())
            .map_err(|error| error.to_string())?;
        let rule_state = RiskRuleState {
            account_id: profile.account_id.clone(),
            profile_id: profile.profile_id.clone(),
            profile_version: profile.version,
            peak_session_pnl: if current.units() > 0 { current } else { zero },
            total_winning_pnl: zero,
            largest_winning_trade_pnl: zero,
        };
        self.store
            .put_risk_profile_and_state(&profile, &rule_state)?;
        self.state
            .risk_rule_states
            .insert(profile.account_id.clone(), rule_state);
        self.state
            .risk_trade_cycles
            .retain(|(account_id, _), _| account_id != &profile.account_id);
        self.state
            .risk_profiles
            .insert(profile.account_id.clone(), profile);
        self.bump_revision()
    }

    fn register_session_plan(&mut self, plan: SessionPlan) -> Result<(), String> {
        plan.validate()?;
        if !self.state.accounts.contains_key(&plan.account_id) {
            return Err("session plan account is not registered".to_string());
        }
        if self
            .state
            .session_plans
            .get(&plan.account_id)
            .is_some_and(|current| {
                current.plan_id == plan.plan_id && plan.revision <= current.revision
            })
        {
            return Err("session plan revision must advance".to_string());
        }
        self.store.put_session_plan(&plan)?;
        self.state
            .session_plans
            .insert(plan.account_id.clone(), plan);
        self.bump_revision()
    }

    fn register_trade_copier(&mut self, config: TradeCopierConfig) -> Result<(), String> {
        config.validate()?;
        if self
            .state
            .trade_copiers
            .get(&config.source_account_id)
            .is_some_and(|current| config.revision <= current.revision)
        {
            return Err("trade copier revision must advance".to_string());
        }
        let source = self
            .state
            .accounts
            .get(&config.source_account_id)
            .ok_or_else(|| "trade copier account is not registered".to_string())?;
        for target in &config.targets {
            let account = self
                .state
                .accounts
                .get(&target.account_id)
                .ok_or_else(|| "trade copier account is not registered".to_string())?;
            if account.environment != source.environment {
                return Err("trade copier cannot mix simulated and live accounts".to_string());
            }
        }
        self.store.put_trade_copier(&config)?;
        self.state
            .trade_copiers
            .insert(config.source_account_id.clone(), config);
        self.bump_revision()
    }

    fn register_strategy_template(
        &mut self,
        template: BracketStrategyTemplate,
    ) -> Result<(), String> {
        template.validate()?;
        if !self
            .state
            .strategy_templates
            .contains_key(&template.template_id)
            && self.state.strategy_templates.len() >= MAXIMUM_STRATEGY_TEMPLATES
        {
            return Err("strategy template limit reached".to_string());
        }
        if self
            .state
            .strategy_templates
            .get(&template.template_id)
            .is_some_and(|current| template.revision <= current.revision)
        {
            return Err("strategy template revision must advance".to_string());
        }
        self.store.put_strategy_template(&template)?;
        self.state
            .strategy_templates
            .insert(template.template_id.clone(), template);
        self.bump_revision()
    }

    fn lock_account(
        &mut self,
        account_id: TradingAccountId,
        reason: String,
        locked_at_unix_nanos: i64,
    ) -> Result<(), String> {
        if !self.state.accounts.contains_key(&account_id) {
            return Err("risk lock account is not registered".to_string());
        }
        let profile = self.state.risk_profiles.get(&account_id);
        let lock = RiskLock {
            account_id: account_id.clone(),
            reason,
            locked_at_unix_nanos,
            profile_id: profile.map(|profile| profile.profile_id.clone()),
            profile_version: profile.map(|profile| profile.version),
        };
        self.store.put_risk_lock(&lock)?;
        self.state.risk_locks.insert(account_id, lock);
        self.bump_revision()
    }

    fn unlock_account(&mut self, account_id: &TradingAccountId) -> Result<(), String> {
        self.store.delete_risk_lock(account_id)?;
        self.state.risk_locks.remove(account_id);
        self.bump_revision()
    }

    fn kill_switch(
        &mut self,
        account_id: Option<&TradingAccountId>,
        reason: &str,
        locked_at_unix_nanos: i64,
    ) -> Result<usize, String> {
        let accounts = self
            .state
            .accounts
            .keys()
            .filter(|candidate| {
                account_id
                    .as_ref()
                    .is_none_or(|account_id| *candidate == *account_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return Err("kill switch account is not registered".to_string());
        }
        let cancelled = self.cancel_all(account_id)?;
        for account in &accounts {
            self.lock_account(account.clone(), reason.to_string(), locked_at_unix_nanos)?;
        }
        Ok(cancelled.len())
    }

    fn cancel_order(&mut self, client_order_id: &ClientOrderId) -> Result<Order, String> {
        self.cancel_order_internal(client_order_id, true)
    }

    fn cancel_order_internal(
        &mut self,
        client_order_id: &ClientOrderId,
        reconcile_bracket: bool,
    ) -> Result<Order, String> {
        let order = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == *client_order_id)
            .cloned()
            .ok_or_else(|| "order client identifier is not registered".to_string())?;
        if !order.status.is_open() {
            return Ok(order);
        }
        let sequence = self.state.next_sequence;
        let provenance = order.provenance.clone();
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Cancelled,
            event_unix_nanos: provenance.observed_unix_nanos,
            detail: Some("cancelled by local command".to_string()),
            provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        self.store.cancel_order(&order, &event, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        let cancelled = Order {
            status: OrderStatus::Cancelled,
            ..order
        };
        self.state
            .orders
            .insert(cancelled.id.clone(), cancelled.clone());
        if reconcile_bracket {
            self.reconcile_managed_cancellation(client_order_id)?;
        }
        self.bump_revision()?;
        Ok(cancelled)
    }

    fn reconcile_managed_cancellation(
        &mut self,
        client_order_id: &ClientOrderId,
    ) -> Result<(), String> {
        let affected = self
            .state
            .managed_brackets
            .iter()
            .find_map(|(id, bracket)| {
                let entry_cancelled = bracket.status == ManagedBracketStatus::AwaitingEntry
                    && bracket.entry_client_order_id == *client_order_id;
                let stop_cancelled = bracket.status == ManagedBracketStatus::Active
                    && bracket.stop_client_order_id.as_ref() == Some(client_order_id);
                (entry_cancelled || stop_cancelled).then(|| {
                    (
                        id.clone(),
                        stop_cancelled.then(|| bracket.target_client_order_ids.clone()),
                    )
                })
            });
        let Some((bracket_id, targets)) = affected else {
            return Ok(());
        };
        let mut bracket = self
            .state
            .managed_brackets
            .get(&bracket_id)
            .cloned()
            .ok_or_else(|| "managed bracket disappeared".to_string())?;
        bracket.status = ManagedBracketStatus::Cancelled;
        self.store.put_managed_bracket(&bracket)?;
        self.state.managed_brackets.insert(bracket_id, bracket);
        for target in targets.into_iter().flatten() {
            self.cancel_order(&target)?;
        }
        Ok(())
    }

    fn modify_order(&mut self, command: ModifyOrder) -> Result<Order, String> {
        let order = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == command.client_order_id)
            .cloned()
            .ok_or_else(|| "order client identifier is not registered".to_string())?;
        if !matches!(
            order.status,
            OrderStatus::Working | OrderStatus::PartiallyFilled
        ) {
            return Err("only working orders can be modified".to_string());
        }
        let instrument = self
            .state
            .instruments
            .get(&order.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        if command.modified_unix_nanos <= 0 {
            return Err("order modification timestamp must be positive".to_string());
        }
        command
            .provenance
            .validate()
            .map_err(|error| error.to_string())?;
        if command
            .limit_price
            .into_iter()
            .chain(command.stop_price)
            .any(|price| price.scale() != instrument.price_scale)
        {
            return Err("modified order scales do not match the instrument".to_string());
        }
        let modified = Order {
            time_in_force: command.time_in_force,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            provenance: command.provenance.clone(),
            ..order.clone()
        };
        modified.validate().map_err(|error| error.to_string())?;
        let sequence = self.state.next_sequence;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Modified,
            event_unix_nanos: command.modified_unix_nanos,
            detail: Some("modified by local command".to_string()),
            provenance: command.provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        self.store.modify_order(&modified, &event, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        self.state
            .orders
            .insert(modified.id.clone(), modified.clone());
        self.bump_revision()?;
        Ok(modified)
    }

    fn cancel_all(&mut self, account_id: Option<&TradingAccountId>) -> Result<Vec<Order>, String> {
        let client_order_ids = self
            .state
            .orders
            .values()
            .filter(|order| order.status.is_open())
            .filter(|order| {
                account_id
                    .as_ref()
                    .is_none_or(|account_id| &order.account_id == *account_id)
            })
            .map(|order| order.client_order_id.clone())
            .collect::<Vec<_>>();
        client_order_ids
            .into_iter()
            .map(|client_order_id| self.cancel_order(&client_order_id))
            .collect()
    }

    fn place_order(&mut self, command: &PlaceOrder) -> Result<Order, String> {
        if let Some(existing) = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == command.client_order_id)
        {
            return Ok(existing.clone());
        }
        let copier = self
            .state
            .trade_copiers
            .get(&command.account_id)
            .filter(|config| config.enabled)
            .cloned();
        let source = self.place_single_order(command.clone())?;
        if let Some(copier) = copier {
            for target in copier.targets.into_iter().filter(|target| target.enabled) {
                let client_id =
                    mirrored_client_order_id(&command.client_order_id, &target.account_id);
                let mirrored_client_order_id = client_id
                    .as_ref()
                    .map_or_else(|_| String::new(), |id| id.as_str().to_string());
                let result = client_id.and_then(|client_order_id| {
                    let quantity = multiply_quantity(command.quantity, target.quantity_multiplier)?;
                    self.place_single_order(PlaceOrder {
                        client_order_id,
                        account_id: target.account_id.clone(),
                        quantity,
                        ..command.clone()
                    })
                });
                self.copy_dispatches.push_back(TradeCopyDispatch {
                    source_client_order_id: command.client_order_id.as_str().to_string(),
                    target_account_id: target.account_id,
                    mirrored_client_order_id,
                    accepted: result.is_ok(),
                    detail: result.err(),
                });
                while self.copy_dispatches.len() > MAXIMUM_COPY_DISPATCHES {
                    self.copy_dispatches.pop_front();
                }
            }
        }
        Ok(source)
    }

    fn place_bracket(&mut self, command: &PlaceBracket) -> Result<ManagedBracket, String> {
        let template = self
            .state
            .strategy_templates
            .get(&command.template_id)
            .filter(|template| template.enabled)
            .cloned()
            .ok_or_else(|| "enabled bracket strategy template is not registered".to_string())?;
        self.place_bracket_with_template(&command.entry, template)
    }

    fn place_bracket_with_template(
        &mut self,
        entry_command: &PlaceOrder,
        template: BracketStrategyTemplate,
    ) -> Result<ManagedBracket, String> {
        template.validate()?;
        let bracket_id = entry_command.client_order_id.as_str().to_string();
        if let Some(existing) = self.state.managed_brackets.get(&bracket_id) {
            return Ok(existing.clone());
        }
        self.make_room_for_managed_bracket()?;
        allocated_target_quantities(entry_command.quantity, &template.targets)?;
        self.check_bracket_stop_risk(entry_command, &template)?;
        if self
            .state
            .orders
            .values()
            .any(|order| order.client_order_id == entry_command.client_order_id)
        {
            return Err(
                "entry client identifier already belongs to a non-bracket order".to_string(),
            );
        }
        let sequence = self.state.next_sequence;
        let (entry, event) = self.prepare_single_order(entry_command.clone(), true, sequence, 0)?;
        let bracket = ManagedBracket {
            bracket_id: bracket_id.clone(),
            template,
            entry_client_order_id: entry_command.client_order_id.clone(),
            stop_client_order_id: None,
            target_client_order_ids: Vec::new(),
            status: ManagedBracketStatus::AwaitingEntry,
            entry_price: None,
        };
        bracket.validate()?;
        self.store
            .insert_order_and_managed_bracket(&entry, &event, &bracket, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        self.state.orders.insert(entry.id.clone(), entry);
        self.state
            .managed_brackets
            .insert(bracket_id, bracket.clone());
        self.bump_revision()?;
        Ok(bracket)
    }

    fn check_bracket_stop_risk(
        &self,
        entry: &PlaceOrder,
        template: &BracketStrategyTemplate,
    ) -> Result<(), String> {
        let Some(profile) = self.state.risk_profiles.get(&entry.account_id) else {
            return Ok(());
        };
        if !profile.enabled {
            return Ok(());
        }
        let instrument = self
            .state
            .instruments
            .get(&entry.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        let account = self
            .state
            .accounts
            .get(&entry.account_id)
            .ok_or_else(|| "trading account is not registered".to_string())?;
        let per_contract_tick = tick_value(instrument, account.currency_scale)?;
        let potential_loss = scale_currency_by_quantity_and_ticks(
            per_contract_tick,
            entry.quantity,
            template.stop_offset_ticks,
        )?
        .exact_rescale(profile.daily_loss_limit.scale())
        .map_err(|error| error.to_string())?;
        let current = self.session_pnl(profile)?;
        let daily_remaining = remaining_limit(profile.daily_loss_limit, negative_loss(current)?)?;
        if potential_loss.units() >= daily_remaining.units() {
            return Err("bracket loss at stop would reach the daily loss limit".to_string());
        }
        if let Some(drawdown) = profile.trailing_drawdown {
            let state = self
                .state
                .risk_rule_states
                .get(&entry.account_id)
                .ok_or_else(|| "risk rule state is unavailable".to_string())?;
            let floor = subtract_fixed(state.peak_session_pnl, drawdown)?;
            let trailing_remaining = nonnegative_difference(current, floor)?;
            if potential_loss.units() >= trailing_remaining.units() {
                return Err("bracket loss at stop would reach the trailing drawdown".to_string());
            }
        }
        Ok(())
    }

    fn make_room_for_managed_bracket(&mut self) -> Result<(), String> {
        if self.state.managed_brackets.len() < MAXIMUM_MANAGED_BRACKETS {
            return Ok(());
        }
        let retired = self
            .state
            .managed_brackets
            .iter()
            .filter(|(_, bracket)| {
                matches!(
                    bracket.status,
                    ManagedBracketStatus::Completed | ManagedBracketStatus::Cancelled
                )
            })
            .min_by_key(|(_, bracket)| {
                self.state
                    .orders
                    .values()
                    .find(|order| order.client_order_id == bracket.entry_client_order_id)
                    .map_or(i64::MIN, |order| order.submitted_unix_nanos)
            })
            .map(|(id, _)| id.clone())
            .ok_or_else(|| "active managed bracket limit reached".to_string())?;
        self.store.delete_managed_bracket(&retired)?;
        self.state.managed_brackets.remove(&retired);
        Ok(())
    }

    fn place_single_order(&mut self, command: PlaceOrder) -> Result<Order, String> {
        self.place_single_order_with_risk(command, true)
    }

    fn place_single_order_with_risk(
        &mut self,
        command: PlaceOrder,
        enforce_risk: bool,
    ) -> Result<Order, String> {
        if let Some(existing) = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == command.client_order_id)
        {
            return Ok(existing.clone());
        }
        let sequence = self.state.next_sequence;
        let (order, event) = self.prepare_single_order(command, enforce_risk, sequence, 0)?;
        self.store.insert_order(&order, &event, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        self.state.orders.insert(order.id.clone(), order.clone());
        self.bump_revision()?;
        Ok(order)
    }

    fn place_protective_order(&mut self, command: PlaceProtectiveOrder) -> Result<Order, String> {
        if let Some(existing) = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == command.order.client_order_id)
        {
            let role_matches = self
                .state
                .protective_orders
                .get(&command.order.client_order_id)
                .is_some_and(|protective| protective.role == command.role);
            return role_matches.then(|| existing.clone()).ok_or_else(|| {
                "protective client identifier has conflicting semantics".to_string()
            });
        }
        let shape_matches = matches!(
            (command.role, command.order.order_type),
            (ProtectiveOrderRole::StopLoss, OrderType::Stop)
                | (ProtectiveOrderRole::TakeProfit, OrderType::Limit)
        );
        if !shape_matches {
            return Err("protective order role does not match its order type".to_string());
        }
        let sequence = self.state.next_sequence;
        let (order, mut event) = self.prepare_single_order(command.order, false, sequence, 0)?;
        event.detail = Some(format!(
            "{} standalone {}",
            ManagedBracket::MANAGEMENT_LABEL,
            command.role.as_str()
        ));
        event.validate().map_err(|error| error.to_string())?;
        let protective = ProtectiveOrder {
            client_order_id: order.client_order_id.clone(),
            role: command.role,
        };
        self.store
            .insert_protective_order(&order, &event, &protective, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        self.state.orders.insert(order.id.clone(), order.clone());
        self.state
            .protective_orders
            .insert(protective.client_order_id.clone(), protective);
        self.bump_revision()?;
        Ok(order)
    }

    fn prepare_single_order(
        &mut self,
        command: PlaceOrder,
        enforce_risk: bool,
        sequence: u64,
        additional_open_orders: usize,
    ) -> Result<(Order, OrderEvent), String> {
        let account = self
            .state
            .accounts
            .get(&command.account_id)
            .ok_or_else(|| "trading account is not registered".to_string())?;
        if account.environment != AccountEnvironment::Simulated {
            return Err("T1 routes orders only to the simulated venue".to_string());
        }
        let risk_warnings = if enforce_risk {
            self.evaluate_order_risk(&command)?.warnings
        } else {
            self.validate_managed_exit(&command)?;
            Vec::new()
        };
        let instrument = self
            .state
            .instruments
            .get(&command.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        if command.quantity.scale() != instrument.quantity_scale
            || command
                .limit_price
                .into_iter()
                .chain(command.stop_price)
                .any(|price| price.scale() != instrument.price_scale)
        {
            return Err("order scales do not match the instrument".to_string());
        }
        if self
            .state
            .orders
            .values()
            .filter(|order| order.status.is_open())
            .count()
            .saturating_add(additional_open_orders)
            >= MAXIMUM_OPEN_ORDERS
        {
            return Err("simulated venue open-order limit reached".to_string());
        }
        let order = Order {
            id: OrderId::try_new(format!("sim-order-{sequence}"))
                .map_err(|error| error.to_string())?,
            client_order_id: command.client_order_id,
            account_id: command.account_id,
            instrument_id: command.instrument_id,
            side: command.side,
            order_type: command.order_type,
            time_in_force: command.time_in_force,
            quantity: command.quantity,
            filled_quantity: FixedPoint::try_new(0, command.quantity.scale())
                .map_err(|error| error.to_string())?,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            status: OrderStatus::Working,
            submitted_unix_nanos: command.submitted_unix_nanos,
            provenance: command.provenance.clone(),
        };
        order.validate().map_err(|error| error.to_string())?;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Accepted,
            event_unix_nanos: command.submitted_unix_nanos,
            detail: Some(order_acceptance_detail(&risk_warnings)),
            provenance: command.provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        Ok((order, event))
    }

    fn validate_managed_exit(&self, command: &PlaceOrder) -> Result<(), String> {
        let position = self
            .state
            .positions
            .get(&(command.account_id.clone(), command.instrument_id.clone()))
            .ok_or_else(|| "managed exit requires an open position".to_string())?;
        let net_units = position
            .net_quantity
            .exact_rescale(command.quantity.scale())
            .map_err(|error| error.to_string())?
            .units();
        if net_units == 0
            || net_units.signum() == command.side.sign()
            || command.quantity.units().unsigned_abs() > net_units.unsigned_abs()
        {
            return Err("managed child order must reduce the open position".to_string());
        }
        Ok(())
    }

    fn evaluate_order_risk(&mut self, command: &PlaceOrder) -> Result<RiskEvaluation, String> {
        if let Some(lock) = self.state.risk_locks.get(&command.account_id) {
            return Err(format!("account is risk-locked: {}", lock.reason));
        }
        self.check_session_plan(command)?;
        let discipline_warnings = self.check_discipline(command)?;
        let Some(profile) = self.state.risk_profiles.get(&command.account_id).cloned() else {
            return Ok(RiskEvaluation {
                warnings: discipline_warnings,
                projected_contracts: command.quantity,
                current_realized_pnl: FixedPoint::try_new(0, 2)
                    .map_err(|error| error.to_string())?,
            });
        };
        if !profile.enabled {
            return Ok(RiskEvaluation {
                warnings: discipline_warnings,
                projected_contracts: command.quantity,
                current_realized_pnl: FixedPoint::try_new(0, profile.daily_loss_limit.scale())
                    .map_err(|error| error.to_string())?,
            });
        }
        self.check_restriction(command, &profile)?;
        let current_session_pnl = self.session_pnl(&profile)?;
        self.check_loss_limits(command, &profile, current_session_pnl)?;
        let projected_contracts = self.projected_contracts(command)?;
        let maximum_contracts = profile
            .max_contracts
            .exact_rescale(command.quantity.scale())
            .map_err(|error| error.to_string())?;
        if projected_contracts > maximum_contracts.units().unsigned_abs() {
            return Err("order exceeds the account maximum-contract rule".to_string());
        }
        let mut warnings = self.risk_warnings(command, &profile, current_session_pnl)?;
        warnings.extend(discipline_warnings);
        Ok(RiskEvaluation {
            warnings,
            projected_contracts: FixedPoint::try_new(
                i64::try_from(projected_contracts)
                    .map_err(|_| "projected contract quantity overflowed".to_string())?,
                command.quantity.scale(),
            )
            .map_err(|error| error.to_string())?,
            current_realized_pnl: current_session_pnl,
        })
    }

    fn check_session_plan(&mut self, command: &PlaceOrder) -> Result<(), String> {
        if self.strictly_reduces_position(command)? {
            return Ok(());
        }
        let Some(plan) = self.state.session_plans.get(&command.account_id).cloned() else {
            return Ok(());
        };
        if !plan.contains_time(command.submitted_unix_nanos) {
            return Err("order is outside the session plan trading hours".to_string());
        }
        if !plan.is_ready() {
            return Err("session plan checklist or allowed setup is incomplete".to_string());
        }
        let pnl = self.plan_session_pnl(&plan)?;
        if pnl.units() < 0 && pnl.units().unsigned_abs() >= plan.maximum_loss.units().unsigned_abs()
        {
            self.lock_account(
                command.account_id.clone(),
                "session plan maximum loss reached".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: session plan maximum loss reached".to_string());
        }
        Ok(())
    }

    fn check_discipline(&mut self, command: &PlaceOrder) -> Result<Vec<String>, String> {
        if self.strictly_reduces_position(command)? {
            return Ok(Vec::new());
        }
        let Some(mut state) = self
            .state
            .discipline_states
            .get(&command.account_id)
            .cloned()
        else {
            return Ok(Vec::new());
        };
        if let Some(until) = state.cooldown_until_unix_nanos {
            if command.submitted_unix_nanos < until {
                return Err(format!(
                    "account is risk-locked: rapid-loss cooldown active until {until}"
                ));
            }
            state.cooldown_until_unix_nanos = None;
            state.rapid_loss_count = 0;
            state.loss_window_started_unix_nanos = None;
            self.store.put_discipline_state(&state)?;
            self.state
                .discipline_states
                .insert(command.account_id.clone(), state.clone());
        }
        if let Some(cap) = state.post_loss_quantity_cap {
            let cap = cap
                .exact_rescale(command.quantity.scale())
                .map_err(|error| error.to_string())?;
            if command.quantity.units() > cap.units() {
                return Err(format!(
                    "tilt size reduction limits the next entry to {} units at scale {}",
                    cap.units(),
                    cap.scale()
                ));
            }
        }
        let mut warnings = Vec::new();
        if state.last_stop_fill_unix_nanos.is_some_and(|last_stop| {
            let elapsed = command.submitted_unix_nanos.saturating_sub(last_stop);
            (0..=FAST_STOP_REENTRY_NANOS).contains(&elapsed)
        }) {
            warnings.push("fast re-entry within 60 seconds of a filled stop".to_string());
        }
        Ok(warnings)
    }

    fn strictly_reduces_position(&self, command: &PlaceOrder) -> Result<bool, String> {
        let Some(position) = self
            .state
            .positions
            .get(&(command.account_id.clone(), command.instrument_id.clone()))
        else {
            return Ok(false);
        };
        let net = position
            .net_quantity
            .exact_rescale(command.quantity.scale())
            .map_err(|error| error.to_string())?
            .units();
        Ok(net != 0
            && net.signum() != command.side.sign()
            && command.quantity.units().unsigned_abs() <= net.unsigned_abs())
    }

    fn plan_session_pnl(&self, plan: &SessionPlan) -> Result<FixedPoint, String> {
        let current = self
            .state
            .positions
            .values()
            .filter(|position| position.account_id == plan.account_id)
            .try_fold(
                FixedPoint::try_new(0, plan.maximum_loss.scale())
                    .map_err(|error| error.to_string())?,
                |total, position| {
                    let realized = position
                        .realized_pnl
                        .exact_rescale(plan.maximum_loss.scale())
                        .map_err(|error| error.to_string())?;
                    let unrealized = position
                        .unrealized_pnl
                        .exact_rescale(plan.maximum_loss.scale())
                        .map_err(|error| error.to_string())?;
                    total
                        .checked_add(realized)
                        .and_then(|value| value.checked_add(unrealized))
                        .map_err(|error| error.to_string())
                },
            )?;
        subtract_fixed(current, plan.session_start_realized_pnl)
    }

    fn check_restriction(
        &mut self,
        command: &PlaceOrder,
        profile: &RiskProfile,
    ) -> Result<(), String> {
        if profile
            .restricted_until_unix_nanos
            .is_some_and(|until| command.submitted_unix_nanos < until)
        {
            self.lock_account(
                command.account_id.clone(),
                "news/session restriction is active".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: news/session restriction is active".to_string());
        }
        Ok(())
    }

    fn check_loss_limits(
        &mut self,
        command: &PlaceOrder,
        profile: &RiskProfile,
        current_session_pnl: FixedPoint,
    ) -> Result<(), String> {
        let loss = current_session_pnl.units().unsigned_abs();
        if current_session_pnl.units() < 0
            && loss >= profile.daily_loss_limit.units().unsigned_abs()
        {
            self.lock_account(
                command.account_id.clone(),
                "daily loss limit reached".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: daily loss limit reached".to_string());
        }
        if let Some(drawdown) = profile.trailing_drawdown {
            let state = self
                .state
                .risk_rule_states
                .get(&profile.account_id)
                .ok_or_else(|| "risk rule state is unavailable".to_string())?;
            let floor = subtract_fixed(state.peak_session_pnl, drawdown)?;
            if current_session_pnl.units() <= floor.units() {
                self.lock_account(
                    command.account_id.clone(),
                    format!(
                        "{} trailing drawdown reached",
                        profile.trailing_mode.as_str()
                    ),
                    command.submitted_unix_nanos,
                )?;
                return Err("account is risk-locked: trailing drawdown reached".to_string());
            }
        }
        Ok(())
    }

    fn projected_contracts(&self, command: &PlaceOrder) -> Result<u64, String> {
        self.projected_contracts_for_account(
            &command.account_id,
            command.quantity.scale(),
            Some(command),
        )
    }

    fn projected_contracts_for_account(
        &self,
        account_id: &TradingAccountId,
        quantity_scale: u8,
        command: Option<&PlaceOrder>,
    ) -> Result<u64, String> {
        let mut scenarios = BTreeMap::<InstrumentId, (i128, i128, i128)>::new();
        for position in self
            .state
            .positions
            .values()
            .filter(|position| &position.account_id == account_id)
        {
            let current = position
                .net_quantity
                .exact_rescale(quantity_scale)
                .map_err(|error| error.to_string())?;
            scenarios
                .entry(position.instrument_id.clone())
                .or_default()
                .0 = i128::from(current.units());
        }
        for order in self.state.orders.values().filter(|order| {
            &order.account_id == account_id
                && order.status.is_open()
                && !self.is_managed_protection(&order.client_order_id)
        }) {
            let quantity = order
                .quantity
                .exact_rescale(quantity_scale)
                .map_err(|error| error.to_string())?;
            let scenario = scenarios.entry(order.instrument_id.clone()).or_default();
            match order.side {
                OrderSide::Buy => scenario.1 += i128::from(quantity.units()),
                OrderSide::Sell => scenario.2 += i128::from(quantity.units()),
            }
        }
        if let Some(command) = command {
            let quantity = command
                .quantity
                .exact_rescale(quantity_scale)
                .map_err(|error| error.to_string())?;
            let scenario = scenarios.entry(command.instrument_id.clone()).or_default();
            match command.side {
                OrderSide::Buy => scenario.1 += i128::from(quantity.units()),
                OrderSide::Sell => scenario.2 += i128::from(quantity.units()),
            }
        }
        let total = scenarios
            .values()
            .try_fold(0_u128, |total, (current, buys, sells)| {
                let long = current
                    .checked_add(*buys)
                    .ok_or_else(|| "risk contract quantity overflowed".to_string())?;
                let short = current
                    .checked_sub(*sells)
                    .ok_or_else(|| "risk contract quantity overflowed".to_string())?;
                total
                    .checked_add(long.unsigned_abs().max(short.unsigned_abs()))
                    .ok_or_else(|| "risk contract quantity overflowed".to_string())
            })?;
        u64::try_from(total).map_err(|_| "risk contract quantity overflowed".to_string())
    }

    fn is_managed_protection(&self, client_order_id: &ClientOrderId) -> bool {
        self.state.protective_orders.contains_key(client_order_id)
            || self.state.managed_brackets.values().any(|bracket| {
                bracket.stop_client_order_id.as_ref() == Some(client_order_id)
                    || bracket.target_client_order_ids.contains(client_order_id)
            })
    }

    fn risk_warnings(
        &self,
        command: &PlaceOrder,
        profile: &RiskProfile,
        current_realized: FixedPoint,
    ) -> Result<Vec<String>, String> {
        let mut warnings = Vec::new();
        if current_realized.units() < 0
            && current_realized.units().unsigned_abs() * 100
                >= profile.daily_loss_limit.units().unsigned_abs() * 80
        {
            warnings.push("account is within 20% of its daily loss limit".to_string());
        }
        if command.stop_price.is_none() {
            warnings.push("order has no attached stop; loss at stop is unbounded".to_string());
        }
        if let Some(limit) = profile.consistency_max_single_trade_percent {
            let state = self
                .state
                .risk_rule_states
                .get(&profile.account_id)
                .ok_or_else(|| "risk rule state is unavailable".to_string())?;
            let (current, _) = consistency_projection(state, limit)?;
            if current > limit {
                warnings.push(format!(
                    "largest winning trade is {current}% of gross winning P/L; profile limit is {limit}%"
                ));
            }
        }
        Ok(warnings)
    }

    fn realized_since_session(&self, profile: &RiskProfile) -> Result<FixedPoint, String> {
        let current = self
            .state
            .positions
            .values()
            .filter(|position| position.account_id == profile.account_id)
            .map(|position| position.realized_pnl)
            .try_fold(
                FixedPoint::try_new(0, profile.daily_loss_limit.scale())
                    .map_err(|error| error.to_string())?,
                |total, value| {
                    let value = value
                        .exact_rescale(profile.daily_loss_limit.scale())
                        .map_err(|error| error.to_string())?;
                    total.checked_add(value).map_err(|error| error.to_string())
                },
            )?;
        let baseline = profile
            .session_start_realized_pnl
            .exact_rescale(profile.daily_loss_limit.scale())
            .map_err(|error| error.to_string())?;
        let negative_baseline = FixedPoint::try_new(
            baseline
                .units()
                .checked_neg()
                .ok_or_else(|| "risk session baseline overflowed".to_string())?,
            baseline.scale(),
        )
        .map_err(|error| error.to_string())?;
        current
            .checked_add(negative_baseline)
            .map_err(|error| error.to_string())
    }

    fn session_pnl(&self, profile: &RiskProfile) -> Result<FixedPoint, String> {
        self.state
            .positions
            .values()
            .filter(|position| position.account_id == profile.account_id)
            .map(|position| position.unrealized_pnl)
            .try_fold(
                self.realized_since_session(profile)?,
                |total, unrealized| {
                    let unrealized = unrealized
                        .exact_rescale(profile.daily_loss_limit.scale())
                        .map_err(|error| error.to_string())?;
                    total
                        .checked_add(unrealized)
                        .map_err(|error| error.to_string())
                },
            )
    }

    fn observe_market(
        &mut self,
        observation: &SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        observation
            .provenance
            .validate()
            .map_err(|error| error.to_string())?;
        if observation.bid.units() <= 0
            || observation.ask.units() <= observation.bid.units()
            || observation.bid.scale() != observation.ask.scale()
        {
            return Err("simulated market observation is invalid".to_string());
        }
        let instrument = self
            .state
            .instruments
            .get(&observation.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?
            .clone();
        if observation.bid.scale() != instrument.price_scale {
            return Err("market observation scale does not match the instrument".to_string());
        }
        self.recover_managed_brackets(observation, &instrument)?;
        self.update_managed_stops(observation, &instrument)?;
        let candidates = market_fill_candidates(&self.state.orders, observation);
        let candidate_ids = candidates
            .iter()
            .map(|(order_id, _)| order_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let fills = self.execute_market_fills(candidates, observation, &instrument)?;
        self.reconcile_managed_brackets(&fills, observation, &instrument)?;
        let mut positions_changed = self.update_mark_to_market(observation, &instrument)?;
        self.evaluate_live_risk(observation.provenance.observed_unix_nanos)?;
        positions_changed |= self.cancel_unfilled_immediate_orders(observation, &candidate_ids)?;
        if positions_changed {
            self.store.enforce_retention()?;
            self.enforce_memory_retention();
            self.bump_revision()?;
        }
        Ok(fills)
    }

    fn evaluate_live_risk(&mut self, observed_unix_nanos: i64) -> Result<(), String> {
        let profiles = self
            .state
            .risk_profiles
            .values()
            .filter(|profile| profile.enabled)
            .cloned()
            .collect::<Vec<_>>();
        for profile in profiles {
            if self.state.risk_locks.contains_key(&profile.account_id) {
                continue;
            }
            let current = self.session_pnl(&profile)?;
            if current.units() < 0
                && current.units().unsigned_abs() >= profile.daily_loss_limit.units().unsigned_abs()
            {
                self.lock_account(
                    profile.account_id.clone(),
                    "daily loss limit reached".to_string(),
                    observed_unix_nanos,
                )?;
                continue;
            }
            let Some(drawdown) = profile.trailing_drawdown else {
                continue;
            };
            let mut state = self
                .state
                .risk_rule_states
                .get(&profile.account_id)
                .cloned()
                .ok_or_else(|| "risk rule state is unavailable".to_string())?;
            if profile.trailing_mode == TrailingDrawdownMode::Intraday
                && current.units() > state.peak_session_pnl.units()
            {
                state.peak_session_pnl = current;
                self.store.put_risk_rule_state(&state)?;
                self.state
                    .risk_rule_states
                    .insert(profile.account_id.clone(), state.clone());
            }
            let floor = subtract_fixed(state.peak_session_pnl, drawdown)?;
            if current.units() <= floor.units() {
                self.lock_account(
                    profile.account_id,
                    format!(
                        "{} trailing drawdown reached",
                        profile.trailing_mode.as_str()
                    ),
                    observed_unix_nanos,
                )?;
            }
        }
        let plans = self
            .state
            .session_plans
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for plan in plans {
            if self.state.risk_locks.contains_key(&plan.account_id) {
                continue;
            }
            let pnl = self.plan_session_pnl(&plan)?;
            if pnl.units() < 0
                && pnl.units().unsigned_abs() >= plan.maximum_loss.units().unsigned_abs()
            {
                self.lock_account(
                    plan.account_id,
                    "session plan maximum loss reached".to_string(),
                    observed_unix_nanos,
                )?;
            }
        }
        Ok(())
    }

    fn recover_managed_brackets(
        &mut self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let awaiting = self
            .state
            .managed_brackets
            .iter()
            .filter(|(_, bracket)| bracket.status == ManagedBracketStatus::AwaitingEntry)
            .filter_map(|(id, bracket)| {
                let order = self
                    .order_by_client_id(&bracket.entry_client_order_id)
                    .ok()?;
                (order.status == OrderStatus::Filled).then(|| {
                    self.state
                        .fills
                        .iter()
                        .rev()
                        .find(|fill| fill.order_id == order.id)
                        .cloned()
                        .map(|fill| (id.clone(), fill))
                })?
            })
            .collect::<Vec<_>>();
        for (bracket_id, fill) in awaiting {
            self.activate_managed_bracket(&bracket_id, &fill, observation, instrument)?;
        }

        let interrupted = self
            .state
            .managed_brackets
            .iter()
            .filter(|(_, bracket)| bracket.status == ManagedBracketStatus::Active)
            .filter_map(|(id, bracket)| {
                let stop_id = bracket.stop_client_order_id.as_ref()?;
                let stop = self.order_by_client_id(stop_id).ok()?;
                if stop.status == OrderStatus::Filled {
                    return Some((id.clone(), stop_id.clone()));
                }
                let entry = self
                    .order_by_client_id(&bracket.entry_client_order_id)
                    .ok()?;
                let position = self
                    .state
                    .positions
                    .get(&(entry.account_id.clone(), entry.instrument_id.clone()))?;
                let remaining = position.net_quantity.units().unsigned_abs();
                let stop_quantity = stop.quantity.units().unsigned_abs();
                (remaining != stop_quantity).then(|| {
                    bracket
                        .target_client_order_ids
                        .iter()
                        .find_map(|target_id| {
                            self.order_by_client_id(target_id)
                                .ok()
                                .filter(|target| target.status == OrderStatus::Filled)
                                .map(|_| (id.clone(), target_id.clone()))
                        })
                })?
            })
            .collect::<Vec<_>>();
        for (bracket_id, child_id) in interrupted {
            self.complete_or_resize_managed_bracket(&bracket_id, &child_id, observation)?;
        }
        Ok(())
    }

    fn execute_market_fills(
        &mut self,
        candidates: Vec<(OrderId, FixedPoint)>,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<Vec<Fill>, String> {
        let mut fills = Vec::with_capacity(candidates.len());
        for (order_id, price) in candidates {
            let order = self
                .state
                .orders
                .get(&order_id)
                .cloned()
                .ok_or_else(|| "working order disappeared".to_string())?;
            let sequence = self.state.next_sequence;
            let fill = Fill {
                id: FillId::try_new(format!("sim-fill-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: order.id.clone(),
                account_id: order.account_id.clone(),
                instrument_id: order.instrument_id.clone(),
                side: order.side,
                price,
                quantity: order.quantity,
                execution_unix_nanos: observation.provenance.observed_unix_nanos,
                provenance: observation.provenance.clone(),
            };
            fill.validate().map_err(|error| error.to_string())?;
            let event = OrderEvent {
                id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: order.id.clone(),
                sequence,
                kind: OrderEventKind::Filled,
                event_unix_nanos: fill.execution_unix_nanos,
                detail: Some("touch fill".to_string()),
                provenance: fill.provenance.clone(),
            };
            let position_key = (order.account_id.clone(), order.instrument_id.clone());
            let previous_position = self.state.positions.get(&position_key).cloned();
            let position = next_position(
                previous_position.as_ref(),
                &fill,
                self.state
                    .accounts
                    .get(&order.account_id)
                    .ok_or_else(|| "fill account disappeared".to_string())?,
                instrument,
            )?;
            let policy_update =
                self.next_fill_policy_state(&fill, previous_position.as_ref(), &position)?;
            self.store.insert_fill(
                &order,
                &event,
                &fill,
                &position,
                fill_policy_persistence(&policy_update),
                sequence + 1,
            )?;
            self.apply_fill_to_memory(
                order,
                event,
                &fill,
                FillMemoryUpdate {
                    position,
                    position_key,
                    policy_update,
                    next_sequence: sequence + 1,
                },
            );
            fills.push(fill);
        }
        Ok(fills)
    }

    fn next_fill_policy_state(
        &self,
        fill: &Fill,
        previous_position: Option<&Position>,
        next_position: &Position,
    ) -> Result<FillPolicyUpdate, String> {
        let currency_scale = self
            .state
            .accounts
            .get(&fill.account_id)
            .ok_or_else(|| "fill account disappeared".to_string())?
            .currency_scale;
        let zero = FixedPoint::try_new(0, currency_scale).map_err(|error| error.to_string())?;
        let zero_quantity =
            FixedPoint::try_new(0, fill.quantity.scale()).map_err(|error| error.to_string())?;
        let mut trade_cycle = self
            .state
            .risk_trade_cycles
            .get(&(fill.account_id.clone(), fill.instrument_id.clone()))
            .cloned()
            .unwrap_or_else(|| RiskTradeCycleState {
                account_id: fill.account_id.clone(),
                instrument_id: fill.instrument_id.clone(),
                realized_pnl: zero,
                peak_quantity: zero_quantity,
            });
        let previous_realized = previous_position.map_or(zero, |position| position.realized_pnl);
        let realized_change = subtract_fixed(next_position.realized_pnl, previous_realized)?;
        trade_cycle.realized_pnl = trade_cycle
            .realized_pnl
            .checked_add(
                realized_change
                    .exact_rescale(trade_cycle.realized_pnl.scale())
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        let previous_quantity =
            previous_position.map_or(0, |position| position.net_quantity.units());
        let next_quantity = next_position.net_quantity.units();
        let peak_units = previous_quantity
            .unsigned_abs()
            .max(next_quantity.unsigned_abs())
            .max(fill.quantity.units().unsigned_abs());
        let peak_quantity = FixedPoint::try_new(
            i64::try_from(peak_units).map_err(|_| "discipline quantity overflowed".to_string())?,
            fill.quantity.scale(),
        )
        .map_err(|error| error.to_string())?;
        if peak_quantity.units()
            > trade_cycle
                .peak_quantity
                .exact_rescale(peak_quantity.scale())
                .map_err(|error| error.to_string())?
                .units()
        {
            trade_cycle.peak_quantity = peak_quantity;
        }
        let completed = previous_quantity != 0
            && (next_quantity == 0 || previous_quantity.signum() != next_quantity.signum());
        let completed_pnl = completed.then_some(trade_cycle.realized_pnl);
        let completed_quantity = completed.then_some(trade_cycle.peak_quantity);
        let mut rule_state = self
            .state
            .risk_profiles
            .get(&fill.account_id)
            .map(|_| {
                self.state
                    .risk_rule_states
                    .get(&fill.account_id)
                    .cloned()
                    .ok_or_else(|| "risk rule state is unavailable".to_string())
            })
            .transpose()?;
        if completed {
            if trade_cycle.realized_pnl.units() > 0
                && let Some(state) = rule_state.as_mut()
            {
                let winner = trade_cycle
                    .realized_pnl
                    .exact_rescale(state.total_winning_pnl.scale())
                    .map_err(|error| error.to_string())?;
                state.total_winning_pnl = state
                    .total_winning_pnl
                    .checked_add(winner)
                    .map_err(|error| error.to_string())?;
                let winner = winner
                    .exact_rescale(state.largest_winning_trade_pnl.scale())
                    .map_err(|error| error.to_string())?;
                if winner.units() > state.largest_winning_trade_pnl.units() {
                    state.largest_winning_trade_pnl = winner;
                }
            }
            trade_cycle.realized_pnl = zero;
            trade_cycle.peak_quantity = zero_quantity;
        }
        let discipline_state =
            self.next_discipline_state(fill, completed_pnl.zip(completed_quantity))?;
        Ok(FillPolicyUpdate {
            rule_state,
            trade_cycle,
            discipline_state,
        })
    }

    fn next_discipline_state(
        &self,
        fill: &Fill,
        completed_trade: Option<(FixedPoint, FixedPoint)>,
    ) -> Result<DisciplineState, String> {
        let mut state = self
            .state
            .discipline_states
            .get(&fill.account_id)
            .cloned()
            .unwrap_or_else(|| DisciplineState::new(fill.account_id.clone()));
        if self.is_stop_fill(fill) {
            state.last_stop_fill_unix_nanos = Some(fill.execution_unix_nanos);
        }
        let Some((pnl, peak_quantity)) = completed_trade else {
            return Ok(state);
        };
        if pnl.units() >= 0 {
            state.rapid_loss_count = 0;
            state.loss_window_started_unix_nanos = None;
            state.post_loss_quantity_cap = None;
            return Ok(state);
        }
        let inside_window = state.loss_window_started_unix_nanos.is_some_and(|started| {
            fill.execution_unix_nanos.saturating_sub(started) <= RAPID_LOSS_WINDOW_NANOS
        });
        if inside_window {
            state.rapid_loss_count = state.rapid_loss_count.saturating_add(1);
        } else {
            state.rapid_loss_count = 1;
            state.loss_window_started_unix_nanos = Some(fill.execution_unix_nanos);
        }
        state.last_loss_unix_nanos = Some(fill.execution_unix_nanos);
        state.post_loss_quantity_cap = Some(peak_quantity);
        if state.rapid_loss_count >= RAPID_LOSS_THRESHOLD {
            state.cooldown_until_unix_nanos = Some(
                fill.execution_unix_nanos
                    .checked_add(RAPID_LOSS_COOLDOWN_NANOS)
                    .ok_or_else(|| "tilt cooldown timestamp overflowed".to_string())?,
            );
        }
        Ok(state)
    }

    fn is_stop_fill(&self, fill: &Fill) -> bool {
        let Some(order) = self.state.orders.get(&fill.order_id) else {
            return false;
        };
        self.state
            .protective_orders
            .get(&order.client_order_id)
            .is_some_and(|protective| protective.role == ProtectiveOrderRole::StopLoss)
            || self.state.managed_brackets.values().any(|bracket| {
                bracket.stop_client_order_id.as_ref() == Some(&order.client_order_id)
            })
    }

    fn apply_fill_to_memory(
        &mut self,
        order: Order,
        event: OrderEvent,
        fill: &Fill,
        update: FillMemoryUpdate,
    ) {
        self.state.next_sequence = update.next_sequence;
        self.state.orders.insert(
            order.id.clone(),
            Order {
                status: OrderStatus::Filled,
                filled_quantity: order.quantity,
                ..order
            },
        );
        self.state.order_events.push_back(event);
        self.state.fills.push_back(fill.clone());
        self.state
            .positions
            .insert(update.position_key, update.position);
        if let Some(rule_state) = update.policy_update.rule_state {
            self.state
                .risk_rule_states
                .insert(fill.account_id.clone(), rule_state);
        }
        self.state.risk_trade_cycles.insert(
            (fill.account_id.clone(), fill.instrument_id.clone()),
            update.policy_update.trade_cycle,
        );
        self.state.discipline_states.insert(
            fill.account_id.clone(),
            update.policy_update.discipline_state,
        );
    }

    fn reconcile_managed_brackets(
        &mut self,
        fills: &[Fill],
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        for fill in fills {
            let filled_client_order_id = self
                .state
                .orders
                .get(&fill.order_id)
                .map(|order| order.client_order_id.clone())
                .ok_or_else(|| "filled bracket order disappeared".to_string())?;
            let action = self
                .state
                .managed_brackets
                .iter()
                .find_map(|(id, bracket)| {
                    if bracket.status == ManagedBracketStatus::AwaitingEntry
                        && bracket.entry_client_order_id == filled_client_order_id
                    {
                        Some((id.clone(), true))
                    } else if bracket.status == ManagedBracketStatus::Active
                        && (bracket.stop_client_order_id.as_ref() == Some(&filled_client_order_id)
                            || bracket
                                .target_client_order_ids
                                .contains(&filled_client_order_id))
                    {
                        Some((id.clone(), false))
                    } else {
                        None
                    }
                });
            let Some((bracket_id, is_entry)) = action else {
                continue;
            };
            if is_entry {
                self.activate_managed_bracket(&bracket_id, fill, observation, instrument)?;
            } else {
                self.complete_or_resize_managed_bracket(
                    &bracket_id,
                    &filled_client_order_id,
                    observation,
                )?;
            }
        }
        Ok(())
    }

    fn activate_managed_bracket(
        &mut self,
        bracket_id: &str,
        entry_fill: &Fill,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let mut bracket = self
            .state
            .managed_brackets
            .get(bracket_id)
            .cloned()
            .ok_or_else(|| "managed bracket disappeared".to_string())?;
        let entry_order = self
            .state
            .orders
            .get(&entry_fill.order_id)
            .cloned()
            .ok_or_else(|| "managed bracket entry order disappeared".to_string())?;
        let tick_units = instrument_tick_units(instrument)?;
        let child_side = opposite_side(entry_order.side);
        let mut sequence = self.state.next_sequence;
        let mut prepared = Vec::with_capacity(bracket.template.targets.len() + 1);
        let (stop_order, stop_event, stop_client_order_id) = self.prepare_initial_managed_stop(
            &entry_order,
            entry_fill,
            observation,
            bracket.template.stop_offset_ticks,
            tick_units,
            sequence,
        )?;
        prepared.push((stop_order, stop_event));
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| "managed bracket sequence overflowed".to_string())?;
        let target_quantities =
            allocated_target_quantities(entry_fill.quantity, &bracket.template.targets)?;
        let mut target_client_order_ids = Vec::with_capacity(bracket.template.targets.len());
        for (index, (target, quantity)) in bracket
            .template
            .targets
            .iter()
            .zip(target_quantities)
            .enumerate()
        {
            let client_order_id = managed_child_client_order_id(
                &entry_order.client_order_id,
                "target",
                u32::try_from(index).map_err(|_| "bracket target index overflowed".to_string())?,
            )?;
            let limit_price = shifted_price(
                entry_fill.price,
                target.offset_ticks,
                entry_order.side.sign(),
                tick_units,
            )?;
            let (target_order, mut target_event) = self.prepare_single_order(
                PlaceOrder {
                    client_order_id: client_order_id.clone(),
                    account_id: entry_order.account_id.clone(),
                    instrument_id: entry_order.instrument_id.clone(),
                    side: child_side,
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GoodTillCancelled,
                    quantity,
                    limit_price: Some(limit_price),
                    stop_price: None,
                    submitted_unix_nanos: observation.provenance.observed_unix_nanos,
                    provenance: observation.provenance.clone(),
                },
                false,
                sequence,
                prepared.len(),
            )?;
            target_event.detail = Some(format!(
                "{} scale-out target {}",
                ManagedBracket::MANAGEMENT_LABEL,
                index + 1
            ));
            target_event.validate().map_err(|error| error.to_string())?;
            prepared.push((target_order, target_event));
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| "managed bracket sequence overflowed".to_string())?;
            target_client_order_ids.push(client_order_id);
        }
        bracket.stop_client_order_id = Some(stop_client_order_id);
        bracket.target_client_order_ids = target_client_order_ids;
        bracket.status = ManagedBracketStatus::Active;
        bracket.entry_price = Some(entry_fill.price);
        bracket.validate()?;
        self.store
            .insert_managed_child_orders(&prepared, &bracket, sequence)?;
        self.state.next_sequence = sequence;
        for (order, event) in prepared {
            self.state.order_events.push_back(event);
            self.state.orders.insert(order.id.clone(), order);
        }
        self.state
            .managed_brackets
            .insert(bracket_id.to_string(), bracket);
        self.bump_revision()
    }

    fn prepare_initial_managed_stop(
        &mut self,
        entry_order: &Order,
        entry_fill: &Fill,
        observation: &SimulatedMarketObservation,
        stop_offset_ticks: u32,
        tick_units: i64,
        sequence: u64,
    ) -> Result<(Order, OrderEvent, ClientOrderId), String> {
        let stop_price = shifted_price(
            entry_fill.price,
            stop_offset_ticks,
            -entry_order.side.sign(),
            tick_units,
        )?;
        let client_order_id =
            managed_child_client_order_id(&entry_order.client_order_id, "stop", 0)?;
        let (order, mut event) = self.prepare_single_order(
            PlaceOrder {
                client_order_id: client_order_id.clone(),
                account_id: entry_order.account_id.clone(),
                instrument_id: entry_order.instrument_id.clone(),
                side: opposite_side(entry_order.side),
                order_type: OrderType::Stop,
                time_in_force: TimeInForce::GoodTillCancelled,
                quantity: entry_fill.quantity,
                limit_price: None,
                stop_price: Some(stop_price),
                submitted_unix_nanos: observation.provenance.observed_unix_nanos,
                provenance: observation.provenance.clone(),
            },
            false,
            sequence,
            0,
        )?;
        event.detail = Some(format!(
            "{} protective stop",
            ManagedBracket::MANAGEMENT_LABEL
        ));
        event.validate().map_err(|error| error.to_string())?;
        Ok((order, event, client_order_id))
    }

    fn complete_or_resize_managed_bracket(
        &mut self,
        bracket_id: &str,
        filled_client_order_id: &ClientOrderId,
        observation: &SimulatedMarketObservation,
    ) -> Result<(), String> {
        let mut bracket = self
            .state
            .managed_brackets
            .get(bracket_id)
            .cloned()
            .ok_or_else(|| "managed bracket disappeared".to_string())?;
        let entry_order = self
            .order_by_client_id(&bracket.entry_client_order_id)?
            .clone();
        if bracket.stop_client_order_id.as_ref() == Some(filled_client_order_id) {
            for target in &bracket.target_client_order_ids {
                self.cancel_order(target)?;
            }
            bracket.status = ManagedBracketStatus::Completed;
        } else {
            let remaining_targets = bracket
                .target_client_order_ids
                .iter()
                .filter(|id| {
                    self.order_by_client_id(id)
                        .is_ok_and(|order| order.status.is_open())
                })
                .count();
            let position = self
                .state
                .positions
                .get(&(
                    entry_order.account_id.clone(),
                    entry_order.instrument_id.clone(),
                ))
                .cloned();
            if remaining_targets == 0
                || position
                    .as_ref()
                    .is_none_or(|position| position.net_quantity.units() == 0)
            {
                if let Some(stop) = &bracket.stop_client_order_id {
                    self.cancel_order_internal(stop, false)?;
                }
                bracket.status = ManagedBracketStatus::Completed;
            } else {
                let current_stop = bracket
                    .stop_client_order_id
                    .as_ref()
                    .ok_or_else(|| "active managed bracket stop is missing".to_string())?;
                let stop_order = self.order_by_client_id(current_stop)?.clone();
                let quantity = FixedPoint::try_new(
                    i64::try_from(
                        position
                            .as_ref()
                            .ok_or_else(|| "managed bracket position disappeared".to_string())?
                            .net_quantity
                            .units()
                            .unsigned_abs(),
                    )
                    .map_err(|_| "managed bracket quantity overflowed".to_string())?,
                    stop_order.quantity.scale(),
                )
                .map_err(|error| error.to_string())?;
                if stop_order.quantity == quantity {
                    return Ok(());
                }
                self.cancel_order_internal(current_stop, false)?;
                let replacement = managed_child_client_order_id(
                    &entry_order.client_order_id,
                    "stop",
                    u32::try_from(self.state.next_sequence)
                        .map_err(|_| "managed bracket sequence overflowed".to_string())?,
                )?;
                self.place_single_order_with_risk(
                    PlaceOrder {
                        client_order_id: replacement.clone(),
                        account_id: stop_order.account_id,
                        instrument_id: stop_order.instrument_id,
                        side: stop_order.side,
                        order_type: OrderType::Stop,
                        time_in_force: stop_order.time_in_force,
                        quantity,
                        limit_price: None,
                        stop_price: stop_order.stop_price,
                        submitted_unix_nanos: observation.provenance.observed_unix_nanos,
                        provenance: observation.provenance.clone(),
                    },
                    false,
                )?;
                bracket.stop_client_order_id = Some(replacement);
            }
        }
        self.store.put_managed_bracket(&bracket)?;
        self.state
            .managed_brackets
            .insert(bracket_id.to_string(), bracket);
        self.bump_revision()
    }

    fn update_managed_stops(
        &mut self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let active = self
            .state
            .managed_brackets
            .values()
            .filter(|bracket| bracket.status == ManagedBracketStatus::Active)
            .cloned()
            .collect::<Vec<_>>();
        let tick_units = instrument_tick_units(instrument)?;
        for bracket in active {
            let entry = self
                .order_by_client_id(&bracket.entry_client_order_id)?
                .clone();
            if entry.instrument_id != observation.instrument_id {
                continue;
            }
            let stop_id = bracket
                .stop_client_order_id
                .as_ref()
                .ok_or_else(|| "active managed bracket stop is missing".to_string())?;
            let stop = self.order_by_client_id(stop_id)?.clone();
            if !stop.status.is_open() {
                continue;
            }
            let entry_price = bracket
                .entry_price
                .ok_or_else(|| "active managed bracket entry price is missing".to_string())?;
            let favorable_price = match entry.side {
                OrderSide::Buy => observation.bid,
                OrderSide::Sell => observation.ask,
            };
            let favorable_ticks =
                favorable_tick_distance(entry_price, favorable_price, entry.side, tick_units)?;
            let mut next_stop = stop
                .stop_price
                .ok_or_else(|| "managed bracket stop price is missing".to_string())?;
            if let Some(rule) = bracket.template.break_even
                && favorable_ticks >= i64::from(rule.activation_ticks)
            {
                let price = shifted_price(
                    entry_price,
                    rule.offset_ticks.unsigned_abs(),
                    entry.side.sign() * i64::from(rule.offset_ticks.signum()),
                    tick_units,
                )?;
                next_stop = tighter_stop(next_stop, price, entry.side);
            }
            if let Some(rule) = bracket.template.trailing_stop
                && favorable_ticks >= i64::from(rule.activation_ticks)
            {
                let price = shifted_price(
                    favorable_price,
                    rule.distance_ticks,
                    -entry.side.sign(),
                    tick_units,
                )?;
                next_stop = tighter_stop(next_stop, price, entry.side);
            }
            if Some(next_stop) != stop.stop_price {
                self.modify_order(ModifyOrder {
                    client_order_id: stop.client_order_id,
                    time_in_force: stop.time_in_force,
                    limit_price: None,
                    stop_price: Some(next_stop),
                    modified_unix_nanos: observation.provenance.observed_unix_nanos,
                    provenance: observation.provenance.clone(),
                })?;
            }
        }
        Ok(())
    }

    fn order_by_client_id(&self, client_order_id: &ClientOrderId) -> Result<&Order, String> {
        self.state
            .orders
            .values()
            .find(|order| order.client_order_id == *client_order_id)
            .ok_or_else(|| "managed bracket order disappeared".to_string())
    }

    fn update_mark_to_market(
        &mut self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<bool, String> {
        let mut positions_changed = false;
        let position_keys = self
            .state
            .positions
            .keys()
            .filter(|(_, instrument_id)| instrument_id == &observation.instrument_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in position_keys {
            let Some(current) = self.state.positions.get(&key).cloned() else {
                continue;
            };
            if current.net_quantity.units() == 0 {
                continue;
            }
            let mark = if current.net_quantity.units() > 0 {
                observation.bid
            } else {
                observation.ask
            };
            let point_value = instrument
                .contract
                .point_value
                .ok_or_else(|| "point value is unavailable for unrealized PnL".to_string())?;
            let point_value = FixedPoint::try_new(point_value.units(), point_value.scale())
                .map_err(|error| error.to_string())?;
            let unrealized = project_unrealized_pnl(
                current.average_entry_price.ok_or_else(|| {
                    "position average is unavailable for unrealized PnL".to_string()
                })?,
                mark,
                current.net_quantity,
                point_value,
                current.realized_pnl.scale(),
            )
            .map_err(|error| error.to_string())?;
            if current.unrealized_pnl != unrealized {
                let updated = Position {
                    unrealized_pnl: unrealized,
                    ..current
                };
                self.store.update_position(&updated)?;
                self.state.positions.insert(key, updated);
                positions_changed = true;
            }
        }
        Ok(positions_changed)
    }

    fn cancel_unfilled_immediate_orders(
        &mut self,
        observation: &SimulatedMarketObservation,
        candidate_ids: &std::collections::BTreeSet<OrderId>,
    ) -> Result<bool, String> {
        let immediate_or_cancel = self
            .state
            .orders
            .values()
            .filter(|order| {
                order.status.is_executable()
                    && order.instrument_id == observation.instrument_id
                    && matches!(
                        order.time_in_force,
                        TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill
                    )
                    && !candidate_ids.contains(&order.id)
            })
            .map(|order| order.client_order_id.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for client_order_id in immediate_or_cancel {
            self.cancel_order(&client_order_id)?;
            changed = true;
        }
        Ok(changed)
    }

    fn flatten_account(
        &mut self,
        account_id: &TradingAccountId,
        observation: &SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        if !self.state.accounts.contains_key(account_id) {
            return Err("flatten account is not registered".to_string());
        }
        let mut fills = self.observe_market(observation)?;
        self.cancel_all(Some(account_id))?;
        fills.extend(self.flatten_positions(account_id, observation)?);
        self.store.enforce_retention()?;
        self.enforce_memory_retention();
        self.bump_revision()?;
        Ok(fills)
    }

    fn apply_economic_event_risk(
        &mut self,
        event: &EconomicEventRiskTrigger,
        observation: Option<&SimulatedMarketObservation>,
    ) -> Result<EconomicEventRiskOutcome, String> {
        const EVENT_GRACE_NANOS: i64 = 15 * 60 * 1_000_000_000;
        event.validate()?;
        let profiles = self
            .state
            .risk_profiles
            .values()
            .filter_map(|profile| {
                profile
                    .enabled
                    .then_some(profile.economic_event_rule)
                    .flatten()
                    .map(|rule| (profile.clone(), rule))
            })
            .collect::<Vec<_>>();
        let mut outcome = EconomicEventRiskOutcome::default();
        for (profile, rule) in profiles {
            if event.importance < rule.minimum_importance {
                continue;
            }
            let lead_nanos = i64::from(rule.lead_seconds)
                .checked_mul(1_000_000_000)
                .ok_or_else(|| "economic event lead time overflowed".to_string())?;
            let window_start = event
                .scheduled_unix_nanos
                .checked_sub(lead_nanos)
                .ok_or_else(|| "economic event lead window overflowed".to_string())?;
            let window_end = event
                .scheduled_unix_nanos
                .checked_add(EVENT_GRACE_NANOS)
                .ok_or_else(|| "economic event grace window overflowed".to_string())?;
            if !(window_start..=window_end).contains(&event.observed_unix_nanos)
                || self.store.economic_event_action_exists(
                    &profile.account_id,
                    &event.event_id,
                    profile.version,
                )?
            {
                continue;
            }
            match rule.action {
                EconomicEventRiskAction::Lock => {
                    let reason = format!("Economic event lock: {} ({})", event.title, event.source);
                    self.kill_switch(
                        Some(&profile.account_id),
                        &reason,
                        event.observed_unix_nanos,
                    )?;
                    outcome.locked_accounts += 1;
                }
                EconomicEventRiskAction::Flatten => {
                    let observation = observation.ok_or_else(|| {
                        "economic event flatten requires a current market observation".to_string()
                    })?;
                    if self
                        .state
                        .positions
                        .iter()
                        .any(|((account_id, instrument_id), position)| {
                            account_id == &profile.account_id
                                && position.net_quantity.units() != 0
                                && instrument_id != &observation.instrument_id
                        })
                    {
                        return Err(
                            "economic event flatten requires current observations for every open instrument"
                                .to_string(),
                        );
                    }
                    outcome
                        .fills
                        .extend(self.flatten_account(&profile.account_id, observation)?);
                    outcome.flattened_accounts += 1;
                }
            }
            self.store.put_economic_event_action(
                &profile.account_id,
                &event.event_id,
                profile.version,
                rule.action,
                event.observed_unix_nanos,
            )?;
        }
        Ok(outcome)
    }

    fn flatten_all(
        &mut self,
        observation: &SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        if self.state.accounts.is_empty() {
            return Err("flatten requires at least one registered account".to_string());
        }
        let mut fills = self.observe_market(observation)?;
        self.cancel_all(None)?;
        let accounts = self.state.accounts.keys().cloned().collect::<Vec<_>>();
        for account_id in accounts {
            fills.extend(self.flatten_positions(&account_id, observation)?);
        }
        self.store.enforce_retention()?;
        self.enforce_memory_retention();
        self.bump_revision()?;
        Ok(fills)
    }

    fn flatten_positions(
        &mut self,
        account_id: &TradingAccountId,
        observation: &SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        let position_keys = self
            .state
            .positions
            .keys()
            .filter(|(candidate, instrument_id)| {
                candidate == account_id && instrument_id == &observation.instrument_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut fills = Vec::new();
        for key in position_keys {
            if let Some(fill) = self.flatten_position(account_id, &key, observation)? {
                fills.push(fill);
            }
        }
        Ok(fills)
    }

    fn flatten_position(
        &mut self,
        account_id: &TradingAccountId,
        key: &(TradingAccountId, InstrumentId),
        observation: &SimulatedMarketObservation,
    ) -> Result<Option<Fill>, String> {
        let Some(current) = self.state.positions.get(key).cloned() else {
            return Ok(None);
        };
        if current.net_quantity.units() == 0 {
            return Ok(None);
        }
        let instrument = self
            .state
            .instruments
            .get(&observation.instrument_id)
            .ok_or_else(|| "flatten instrument is not registered".to_string())?
            .clone();
        let account = self
            .state
            .accounts
            .get(account_id)
            .ok_or_else(|| "flatten account is not registered".to_string())?
            .clone();
        let sequence = self.state.next_sequence;
        let side = if current.net_quantity.units() > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let quantity = FixedPoint::try_new(
            i64::try_from(current.net_quantity.units().unsigned_abs())
                .map_err(|_| "flatten quantity overflowed".to_string())?,
            current.net_quantity.scale(),
        )
        .map_err(|error| error.to_string())?;
        let price = if side == OrderSide::Sell {
            observation.bid
        } else {
            observation.ask
        };
        let provenance = observation.provenance.clone();
        let order = Order {
            id: OrderId::try_new(format!("sim-flatten-order-{sequence}"))
                .map_err(|error| error.to_string())?,
            client_order_id: ClientOrderId::try_new(format!("sim-flatten-{sequence}"))
                .map_err(|error| error.to_string())?,
            account_id: account_id.clone(),
            instrument_id: observation.instrument_id.clone(),
            side,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            quantity,
            filled_quantity: FixedPoint::try_new(0, quantity.scale())
                .map_err(|error| error.to_string())?,
            limit_price: None,
            stop_price: None,
            status: OrderStatus::Working,
            submitted_unix_nanos: provenance.observed_unix_nanos,
            provenance: provenance.clone(),
        };
        order.validate().map_err(|error| error.to_string())?;
        let accepted = flatten_acceptance_event(&order, sequence)?;
        self.store.insert_order(&order, &accepted, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(accepted);
        self.state.orders.insert(order.id.clone(), order.clone());
        let fill = Fill {
            id: FillId::try_new(format!("sim-flatten-fill-{}", sequence + 1))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            account_id: account_id.clone(),
            instrument_id: observation.instrument_id.clone(),
            side,
            price,
            quantity,
            execution_unix_nanos: provenance.observed_unix_nanos,
            provenance: provenance.clone(),
        };
        fill.validate().map_err(|error| error.to_string())?;
        let event = flatten_fill_event(&order, &fill, sequence + 1, provenance)?;
        let position = next_position(Some(&current), &fill, &account, &instrument)?;
        let policy_update = self.next_fill_policy_state(&fill, Some(&current), &position)?;
        self.store.insert_fill(
            &order,
            &event,
            &fill,
            &position,
            fill_policy_persistence(&policy_update),
            sequence + 2,
        )?;
        self.apply_fill_to_memory(
            order,
            event,
            &fill,
            FillMemoryUpdate {
                position,
                position_key: key.clone(),
                policy_update,
                next_sequence: sequence + 2,
            },
        );
        Ok(Some(fill))
    }

    fn snapshot(&self) -> Result<TradingSnapshot, String> {
        if self.state.accounts.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.orders.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.positions.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.protective_orders.len() > MAXIMUM_SNAPSHOT_ITEMS
        {
            return Err("trading snapshot item limit exceeded".to_string());
        }
        Ok(TradingSnapshot {
            revision: self.state.revision,
            accounts: self.state.accounts.values().cloned().collect(),
            orders: self.state.orders.values().cloned().collect(),
            order_events: self
                .state
                .order_events
                .iter()
                .rev()
                .take(MAXIMUM_SNAPSHOT_ITEMS)
                .cloned()
                .collect(),
            fills: self
                .state
                .fills
                .iter()
                .rev()
                .take(MAXIMUM_SNAPSHOT_ITEMS)
                .cloned()
                .collect(),
            positions: self.state.positions.values().cloned().collect(),
            position_pnl: self.position_pnl()?,
            account_pnl: self.account_pnl()?,
            risk_profiles: self.state.risk_profiles.values().cloned().collect(),
            risk_meters: self.risk_meters()?,
            risk_locks: self.projected_risk_locks(),
            risk_rule_states: self.state.risk_rule_states.values().cloned().collect(),
            discipline_states: self.state.discipline_states.values().cloned().collect(),
            session_plans: self.state.session_plans.values().cloned().collect(),
            session_adherence_reviews: self.session_adherence_reviews()?,
            trade_copiers: self.state.trade_copiers.values().cloned().collect(),
            copy_dispatches: self.copy_dispatches.iter().cloned().collect(),
            strategy_templates: self.state.strategy_templates.values().cloned().collect(),
            managed_brackets: self.state.managed_brackets.values().cloned().collect(),
            protective_orders: self.state.protective_orders.values().cloned().collect(),
        })
    }

    fn account_pnl(&self) -> Result<Vec<AccountPnl>, String> {
        self.state
            .accounts
            .values()
            .map(|account| -> Result<AccountPnl, String> {
                let zero = FixedPoint::try_new(0, account.currency_scale)
                    .map_err(|error| error.to_string())?;
                let (realized, unrealized) = self
                    .state
                    .positions
                    .values()
                    .filter(|position| position.account_id == account.id)
                    .try_fold(
                        (zero, zero),
                        |(realized, unrealized), position| -> Result<_, String> {
                            let realized_value = position
                                .realized_pnl
                                .exact_rescale(account.currency_scale)
                                .map_err(|error| error.to_string())?;
                            let unrealized_value = position
                                .unrealized_pnl
                                .exact_rescale(account.currency_scale)
                                .map_err(|error| error.to_string())?;
                            Ok((
                                realized
                                    .checked_add(realized_value)
                                    .map_err(|error| error.to_string())?,
                                unrealized
                                    .checked_add(unrealized_value)
                                    .map_err(|error| error.to_string())?,
                            ))
                        },
                    )?;
                Ok(AccountPnl {
                    account_id: account.id.clone(),
                    currency: account.currency.clone(),
                    realized,
                    unrealized,
                })
            })
            .collect()
    }

    fn session_adherence_reviews(&self) -> Result<Vec<SessionAdherenceReview>, String> {
        self.state
            .session_plans
            .values()
            .map(|plan| {
                let fills = self
                    .state
                    .fills
                    .iter()
                    .filter(|fill| fill.account_id == plan.account_id)
                    .collect::<Vec<_>>();
                let pnl = self.plan_session_pnl(plan)?;
                Ok(SessionAdherenceReview {
                    account_id: plan.account_id.clone(),
                    plan_id: plan.plan_id.clone(),
                    plan_revision: plan.revision,
                    checklist_completed: plan
                        .checklist
                        .iter()
                        .filter(|item| item.completed)
                        .count(),
                    checklist_total: plan.checklist.len(),
                    active_setup: plan.active_setup.clone(),
                    retained_fill_count: fills.len(),
                    fills_outside_planned_hours: fills
                        .iter()
                        .filter(|fill| !plan.contains_time(fill.execution_unix_nanos))
                        .count(),
                    maximum_loss_respected: pnl.units() > -plan.maximum_loss.units(),
                })
            })
            .collect()
    }

    fn projected_risk_locks(&self) -> Vec<RiskLock> {
        let mut locks = self.state.risk_locks.values().cloned().collect::<Vec<_>>();
        for state in self.state.discipline_states.values() {
            let Some(until) = state.cooldown_until_unix_nanos else {
                continue;
            };
            if locks.iter().any(|lock| lock.account_id == state.account_id) {
                continue;
            }
            let profile = self.state.risk_profiles.get(&state.account_id);
            locks.push(RiskLock {
                account_id: state.account_id.clone(),
                reason: format!("rapid-loss cooldown active until {until}"),
                locked_at_unix_nanos: state.last_loss_unix_nanos.unwrap_or(1),
                profile_id: profile.map(|profile| profile.profile_id.clone()),
                profile_version: profile.map(|profile| profile.version),
            });
        }
        locks.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        locks
    }

    fn position_pnl(&self) -> Result<Vec<PositionPnl>, String> {
        self.state
            .positions
            .values()
            .map(|position| {
                let account = self
                    .state
                    .accounts
                    .get(&position.account_id)
                    .ok_or_else(|| "position account is not registered".to_string())?;
                let instrument = self
                    .state
                    .instruments
                    .get(&position.instrument_id)
                    .ok_or_else(|| "position instrument is not registered".to_string())?;
                let point_value = instrument
                    .contract
                    .point_value
                    .and_then(|value| FixedPoint::try_new(value.units(), value.scale()).ok());
                let ticks = tick_value(instrument, account.currency_scale).ok();
                Ok(PositionPnl {
                    realized_ticks: ticks
                        .and_then(|tick_value| pnl_ticks(position.realized_pnl, tick_value)),
                    unrealized_ticks: ticks
                        .and_then(|tick_value| pnl_ticks(position.unrealized_pnl, tick_value)),
                    point_value,
                    currency_scale: account.currency_scale,
                    position: position.clone(),
                })
            })
            .collect()
    }

    fn risk_meters(&self) -> Result<Vec<RiskMeter>, String> {
        self.state
            .risk_profiles
            .values()
            .map(|profile| {
                let current_realized_pnl = self.realized_since_session(profile)?;
                let current_session_pnl = self.session_pnl(profile)?;
                let loss_used = negative_loss(current_session_pnl)?;
                let daily_loss_remaining = remaining_limit(profile.daily_loss_limit, loss_used)?;
                let trailing_drawdown_remaining = profile
                    .trailing_drawdown
                    .map(|drawdown| {
                        let state = self
                            .state
                            .risk_rule_states
                            .get(&profile.account_id)
                            .ok_or_else(|| "risk rule state is unavailable".to_string())?;
                        let floor = subtract_fixed(state.peak_session_pnl, drawdown)?;
                        nonnegative_difference(current_session_pnl, floor)
                    })
                    .transpose()?;
                let projected_contracts = FixedPoint::try_new(
                    i64::try_from(self.projected_contracts_for_account(
                        &profile.account_id,
                        profile.max_contracts.scale(),
                        None,
                    )?)
                    .map_err(|_| "risk contract quantity overflowed".to_string())?,
                    profile.max_contracts.scale(),
                )
                .map_err(|error| error.to_string())?;
                let contracts_remaining =
                    remaining_limit(profile.max_contracts, projected_contracts)?;
                let (consistency_current_percent, consistency_additional_profit_required) = profile
                    .consistency_max_single_trade_percent
                    .map(|limit| {
                        let state = self
                            .state
                            .risk_rule_states
                            .get(&profile.account_id)
                            .ok_or_else(|| "risk rule state is unavailable".to_string())?;
                        let (current, additional) = consistency_projection(state, limit)?;
                        Ok::<_, String>((Some(current), Some(additional)))
                    })
                    .transpose()?
                    .unwrap_or((None, None));
                Ok(RiskMeter {
                    account_id: profile.account_id.clone(),
                    profile_id: profile.profile_id.clone(),
                    profile_version: profile.version,
                    enabled: profile.enabled,
                    current_realized_pnl,
                    daily_loss_remaining,
                    trailing_drawdown_remaining,
                    contracts_remaining,
                    consistency_max_single_trade_percent: profile
                        .consistency_max_single_trade_percent,
                    consistency_current_percent,
                    consistency_additional_profit_required,
                    restricted_until_unix_nanos: profile.restricted_until_unix_nanos,
                    lock_reason: self
                        .state
                        .risk_locks
                        .get(&profile.account_id)
                        .map(|lock| lock.reason.clone())
                        .or_else(|| {
                            self.state
                                .discipline_states
                                .get(&profile.account_id)
                                .and_then(|state| state.cooldown_until_unix_nanos)
                                .map(|until| format!("rapid-loss cooldown active until {until}"))
                        }),
                })
            })
            .collect()
    }

    fn bump_revision(&mut self) -> Result<(), String> {
        self.state.revision = self.state.revision.saturating_add(1).max(1);
        self.store.set_revision(self.state.revision)
    }

    fn enforce_memory_retention(&mut self) {
        while self.state.order_events.len() > self.retention.maximum_order_events {
            self.state.order_events.pop_front();
        }
        while self.state.fills.len() > self.retention.maximum_fills {
            self.state.fills.pop_front();
        }
        while self.state.orders.len() > self.retention.maximum_orders {
            let Some(retired) = self
                .state
                .orders
                .values()
                .filter(|order| !order.status.is_open())
                .min_by_key(|order| order.submitted_unix_nanos)
                .map(|order| order.id.clone())
            else {
                break;
            };
            if let Some(order) = self.state.orders.remove(&retired) {
                self.state.protective_orders.remove(&order.client_order_id);
            }
        }
    }
}

fn fill_policy_persistence(update: &FillPolicyUpdate) -> FillPolicyPersistence<'_> {
    FillPolicyPersistence {
        rule_state: update.rule_state.as_ref(),
        trade_cycle: &update.trade_cycle,
        discipline_state: &update.discipline_state,
    }
}

fn order_acceptance_detail(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return "accepted by local simulated venue".to_string();
    }
    format!("RISK WARNING · {}", warnings.join(" · "))
        .chars()
        .take(256)
        .collect()
}

fn flatten_acceptance_event(order: &Order, sequence: u64) -> Result<OrderEvent, String> {
    let event = OrderEvent {
        id: OrderEventId::try_new(format!("sim-flatten-event-{sequence}"))
            .map_err(|error| error.to_string())?,
        order_id: order.id.clone(),
        sequence,
        kind: OrderEventKind::Accepted,
        event_unix_nanos: order.submitted_unix_nanos,
        detail: Some("accepted by local flatten command".to_string()),
        provenance: order.provenance.clone(),
    };
    event.validate().map_err(|error| error.to_string())?;
    Ok(event)
}

fn flatten_fill_event(
    order: &Order,
    fill: &Fill,
    sequence: u64,
    provenance: TradingProvenance,
) -> Result<OrderEvent, String> {
    let event = OrderEvent {
        id: OrderEventId::try_new(format!("sim-flatten-event-{sequence}"))
            .map_err(|error| error.to_string())?,
        order_id: order.id.clone(),
        sequence,
        kind: OrderEventKind::Filled,
        event_unix_nanos: fill.execution_unix_nanos,
        detail: Some("filled by local flatten command".to_string()),
        provenance,
    };
    event.validate().map_err(|error| error.to_string())?;
    Ok(event)
}

fn touch_price(order: &Order, observation: &SimulatedMarketObservation) -> Option<FixedPoint> {
    match (order.side, order.order_type) {
        (OrderSide::Buy, OrderType::Market) => Some(observation.ask),
        (OrderSide::Sell, OrderType::Market) => Some(observation.bid),
        (OrderSide::Buy, OrderType::Limit)
            if observation.ask.units() <= order.limit_price?.units() =>
        {
            Some(observation.ask)
        }
        (OrderSide::Sell, OrderType::Limit)
            if observation.bid.units() >= order.limit_price?.units() =>
        {
            Some(observation.bid)
        }
        (OrderSide::Buy, OrderType::Stop)
            if observation.ask.units() >= order.stop_price?.units() =>
        {
            Some(observation.ask)
        }
        (OrderSide::Sell, OrderType::Stop)
            if observation.bid.units() <= order.stop_price?.units() =>
        {
            Some(observation.bid)
        }
        (OrderSide::Buy, OrderType::StopLimit)
            if observation.ask.units() >= order.stop_price?.units()
                && observation.ask.units() <= order.limit_price?.units() =>
        {
            Some(observation.ask)
        }
        (OrderSide::Sell, OrderType::StopLimit)
            if observation.bid.units() <= order.stop_price?.units()
                && observation.bid.units() >= order.limit_price?.units() =>
        {
            Some(observation.bid)
        }
        _ => None,
    }
}

fn market_fill_candidates(
    orders: &BTreeMap<OrderId, Order>,
    observation: &SimulatedMarketObservation,
) -> Vec<(OrderId, FixedPoint)> {
    orders
        .values()
        .filter(|order| {
            order.status.is_executable() && order.instrument_id == observation.instrument_id
        })
        .filter_map(|order| touch_price(order, observation).map(|price| (order.id.clone(), price)))
        .collect()
}

fn next_position(
    current: Option<&Position>,
    fill: &Fill,
    account: &TradingAccount,
    instrument: &TradingInstrument,
) -> Result<Position, String> {
    let signed_fill = fill
        .quantity
        .units()
        .checked_mul(fill.side.sign())
        .ok_or_else(|| "position quantity overflowed".to_string())?;
    let previous_quantity = current.map_or(0, |position| position.net_quantity.units());
    let next_quantity = previous_quantity
        .checked_add(signed_fill)
        .ok_or_else(|| "position quantity overflowed".to_string())?;
    let same_direction =
        previous_quantity == 0 || previous_quantity.signum() == signed_fill.signum();
    let average_entry_price = if next_quantity == 0 {
        None
    } else if same_direction {
        match current.and_then(|position| position.average_entry_price) {
            None => Some(fill.price),
            Some(previous_average) => {
                let previous_weight = i128::from(previous_average.units())
                    .checked_mul(i128::from(previous_quantity.unsigned_abs()))
                    .ok_or_else(|| "position cost overflowed".to_string())?;
                let fill_weight = i128::from(fill.price.units())
                    .checked_mul(i128::from(signed_fill.unsigned_abs()))
                    .ok_or_else(|| "position cost overflowed".to_string())?;
                let numerator = previous_weight
                    .checked_add(fill_weight)
                    .ok_or_else(|| "position cost overflowed".to_string())?;
                let denominator = i128::from(next_quantity.unsigned_abs());
                let average = numerator / denominator;
                if numerator % denominator == 0 {
                    i64::try_from(average)
                        .ok()
                        .and_then(|units| FixedPoint::try_new(units, fill.price.scale()).ok())
                } else {
                    None
                }
            }
        }
    } else if next_quantity.signum() == previous_quantity.signum() {
        current.and_then(|position| position.average_entry_price)
    } else {
        Some(fill.price)
    };
    let prior_realized = current.map_or(
        FixedPoint::try_new(0, account.currency_scale).map_err(|error| error.to_string())?,
        |position| position.realized_pnl,
    );
    let closed_quantity = if same_direction {
        0
    } else {
        previous_quantity
            .unsigned_abs()
            .min(signed_fill.unsigned_abs())
    };
    let realized_change = if closed_quantity == 0 {
        FixedPoint::try_new(0, account.currency_scale).map_err(|error| error.to_string())?
    } else {
        let entry = current
            .and_then(|position| position.average_entry_price)
            .ok_or_else(|| "position average is unavailable for realized PnL".to_string())?;
        realized_pnl(
            entry,
            fill.price,
            previous_quantity.signum(),
            closed_quantity,
            account.currency_scale,
            instrument,
        )?
    };
    let realized_pnl = prior_realized
        .checked_add(realized_change)
        .map_err(|error| error.to_string())?;
    Ok(Position {
        account_id: fill.account_id.clone(),
        instrument_id: fill.instrument_id.clone(),
        net_quantity: FixedPoint::try_new(next_quantity, fill.quantity.scale())
            .map_err(|error| error.to_string())?,
        average_entry_price,
        realized_pnl,
        unrealized_pnl: FixedPoint::try_new(0, account.currency_scale)
            .map_err(|error| error.to_string())?,
        last_fill_unix_nanos: fill.execution_unix_nanos,
    })
}

fn realized_pnl(
    entry: FixedPoint,
    exit: FixedPoint,
    position_sign: i64,
    quantity: u64,
    currency_scale: u8,
    instrument: &TradingInstrument,
) -> Result<FixedPoint, String> {
    let point_value = instrument
        .contract
        .point_value
        .ok_or_else(|| "point value is unavailable for realized PnL".to_string())?;
    let price_difference = i128::from(exit.units()) - i128::from(entry.units());
    let raw = price_difference
        .checked_mul(i128::from(position_sign))
        .and_then(|value| value.checked_mul(i128::from(quantity)))
        .and_then(|value| value.checked_mul(i128::from(point_value.units())))
        .ok_or_else(|| "realized PnL overflowed".to_string())?;
    let raw_scale = entry
        .scale()
        .checked_add(point_value.scale())
        .ok_or_else(|| "realized PnL scale overflowed".to_string())?;
    let raw = i64::try_from(raw).map_err(|_| "realized PnL overflowed".to_string())?;
    FixedPoint::try_new(raw, raw_scale)
        .map_err(|error| error.to_string())?
        .exact_rescale(currency_scale)
        .map_err(|error| error.to_string())
}

fn negative_loss(value: FixedPoint) -> Result<FixedPoint, String> {
    let units = if value.units() < 0 {
        i64::try_from(value.units().unsigned_abs())
            .map_err(|_| "risk loss projection overflowed".to_string())?
    } else {
        0
    };
    FixedPoint::try_new(units, value.scale()).map_err(|error| error.to_string())
}

fn remaining_limit(limit: FixedPoint, used: FixedPoint) -> Result<FixedPoint, String> {
    let used = used
        .exact_rescale(limit.scale())
        .map_err(|error| error.to_string())?;
    let negative_used = FixedPoint::try_new(
        used.units()
            .checked_neg()
            .ok_or_else(|| "risk limit projection overflowed".to_string())?,
        used.scale(),
    )
    .map_err(|error| error.to_string())?;
    let remaining = limit
        .checked_add(negative_used)
        .map_err(|error| error.to_string())?;
    if remaining.units() < 0 {
        FixedPoint::try_new(0, limit.scale()).map_err(|error| error.to_string())
    } else {
        Ok(remaining)
    }
}

fn subtract_fixed(left: FixedPoint, right: FixedPoint) -> Result<FixedPoint, String> {
    let right = right
        .exact_rescale(left.scale())
        .map_err(|error| error.to_string())?;
    let negative = FixedPoint::try_new(
        right
            .units()
            .checked_neg()
            .ok_or_else(|| "fixed-point subtraction overflowed".to_string())?,
        right.scale(),
    )
    .map_err(|error| error.to_string())?;
    left.checked_add(negative)
        .map_err(|error| error.to_string())
}

fn nonnegative_difference(left: FixedPoint, right: FixedPoint) -> Result<FixedPoint, String> {
    let difference = subtract_fixed(left, right)?;
    if difference.units() < 0 {
        FixedPoint::try_new(0, difference.scale()).map_err(|error| error.to_string())
    } else {
        Ok(difference)
    }
}

fn consistency_projection(
    state: &RiskRuleState,
    maximum_percent: u8,
) -> Result<(u8, FixedPoint), String> {
    let total = state.total_winning_pnl;
    let largest = state
        .largest_winning_trade_pnl
        .exact_rescale(total.scale())
        .map_err(|error| error.to_string())?;
    if total.units() <= 0 || largest.units() <= 0 {
        return Ok((
            0,
            FixedPoint::try_new(0, total.scale()).map_err(|error| error.to_string())?,
        ));
    }
    let total_units = i128::from(total.units());
    let largest_hundred = i128::from(largest.units())
        .checked_mul(100)
        .ok_or_else(|| "consistency projection overflowed".to_string())?;
    let current = largest_hundred
        .checked_add(total_units - 1)
        .ok_or_else(|| "consistency projection overflowed".to_string())?
        / total_units;
    let divisor = i128::from(maximum_percent);
    let required_total = largest_hundred
        .checked_add(divisor - 1)
        .ok_or_else(|| "consistency projection overflowed".to_string())?
        / divisor;
    let additional = required_total.saturating_sub(total_units);
    Ok((
        u8::try_from(current.min(100))
            .map_err(|_| "consistency percentage overflowed".to_string())?,
        FixedPoint::try_new(
            i64::try_from(additional)
                .map_err(|_| "consistency projection overflowed".to_string())?,
            total.scale(),
        )
        .map_err(|error| error.to_string())?,
    ))
}

fn tick_value(instrument: &TradingInstrument, currency_scale: u8) -> Result<FixedPoint, String> {
    let tick_size = instrument
        .contract
        .tick_size
        .ok_or_else(|| "tick size is unavailable for tick PnL".to_string())?;
    let point_value = instrument
        .contract
        .point_value
        .ok_or_else(|| "point value is unavailable for tick PnL".to_string())?;
    let units = i128::from(tick_size.units())
        .checked_mul(i128::from(point_value.units()))
        .ok_or_else(|| "tick value overflowed".to_string())?;
    let scale = tick_size
        .scale()
        .checked_add(point_value.scale())
        .ok_or_else(|| "tick value scale overflowed".to_string())?;
    FixedPoint::try_new(
        i64::try_from(units).map_err(|_| "tick value overflowed".to_string())?,
        scale,
    )
    .map_err(|error| error.to_string())?
    .exact_rescale(currency_scale)
    .map_err(|error| error.to_string())
}

fn pnl_ticks(pnl: FixedPoint, tick_value: FixedPoint) -> Option<FixedPoint> {
    let pnl = pnl.exact_rescale(tick_value.scale()).ok()?;
    let divisor = tick_value.units();
    (divisor != 0 && pnl.units() % divisor == 0)
        .then(|| FixedPoint::try_new(pnl.units() / divisor, 0).ok())
        .flatten()
}

fn scale_currency_by_quantity_and_ticks(
    per_contract_tick: FixedPoint,
    quantity: FixedPoint,
    ticks: u32,
) -> Result<FixedPoint, String> {
    let divisor = 10_i128
        .checked_pow(u32::from(quantity.scale()))
        .ok_or_else(|| "bracket stop loss scale overflowed".to_string())?;
    let numerator = i128::from(per_contract_tick.units())
        .checked_mul(i128::from(quantity.units()))
        .and_then(|value| value.checked_mul(i128::from(ticks)))
        .ok_or_else(|| "bracket stop loss overflowed".to_string())?;
    if numerator % divisor != 0 {
        return Err("bracket stop loss cannot be represented exactly".to_string());
    }
    FixedPoint::try_new(
        i64::try_from(numerator / divisor)
            .map_err(|_| "bracket stop loss overflowed".to_string())?,
        per_contract_tick.scale(),
    )
    .map_err(|error| error.to_string())
}

fn multiply_quantity(quantity: FixedPoint, multiplier: FixedPoint) -> Result<FixedPoint, String> {
    let units = i128::from(quantity.units())
        .checked_mul(i128::from(multiplier.units()))
        .ok_or_else(|| "trade copier quantity overflowed".to_string())?;
    let scale = quantity
        .scale()
        .checked_add(multiplier.scale())
        .ok_or_else(|| "trade copier quantity scale overflowed".to_string())?;
    FixedPoint::try_new(
        i64::try_from(units).map_err(|_| "trade copier quantity overflowed".to_string())?,
        scale,
    )
    .map_err(|error| error.to_string())?
    .exact_rescale(quantity.scale())
    .map_err(|error| format!("trade copier multiplier is inexact: {error}"))
}

fn allocated_target_quantities(
    quantity: FixedPoint,
    targets: &[BracketTarget],
) -> Result<Vec<FixedPoint>, String> {
    targets
        .iter()
        .map(|target| {
            let scaled = i128::from(quantity.units())
                .checked_mul(i128::from(target.quantity_percent))
                .ok_or_else(|| "bracket target quantity overflowed".to_string())?;
            if scaled % 100 != 0 {
                return Err(
                    "entry quantity cannot be divided exactly across bracket targets".to_string(),
                );
            }
            let units = i64::try_from(scaled / 100)
                .map_err(|_| "bracket target quantity overflowed".to_string())?;
            if units <= 0 {
                return Err("every bracket target requires positive quantity".to_string());
            }
            FixedPoint::try_new(units, quantity.scale()).map_err(|error| error.to_string())
        })
        .collect()
}

fn instrument_tick_units(instrument: &TradingInstrument) -> Result<i64, String> {
    let tick = instrument
        .contract
        .tick_size
        .ok_or_else(|| "tick size is required for a bracket strategy".to_string())?;
    FixedPoint::try_new(tick.units(), tick.scale())
        .map_err(|error| error.to_string())?
        .exact_rescale(instrument.price_scale)
        .map(FixedPoint::units)
        .map_err(|error| format!("instrument tick size is not exact at its price scale: {error}"))
}

fn shifted_price(
    base: FixedPoint,
    ticks: u32,
    direction: i64,
    tick_units: i64,
) -> Result<FixedPoint, String> {
    let offset = i128::from(tick_units)
        .checked_mul(i128::from(ticks))
        .and_then(|value| value.checked_mul(i128::from(direction)))
        .ok_or_else(|| "bracket price offset overflowed".to_string())?;
    let units = i128::from(base.units())
        .checked_add(offset)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| "bracket price overflowed".to_string())?;
    if units <= 0 {
        return Err("bracket price must remain positive".to_string());
    }
    FixedPoint::try_new(units, base.scale()).map_err(|error| error.to_string())
}

fn favorable_tick_distance(
    entry: FixedPoint,
    market: FixedPoint,
    side: OrderSide,
    tick_units: i64,
) -> Result<i64, String> {
    if entry.scale() != market.scale() || tick_units <= 0 {
        return Err("bracket price scales do not match".to_string());
    }
    market
        .units()
        .checked_sub(entry.units())
        .and_then(|distance| distance.checked_mul(side.sign()))
        .map(|distance| distance / tick_units)
        .ok_or_else(|| "bracket favorable-distance overflowed".to_string())
}

fn tighter_stop(current: FixedPoint, candidate: FixedPoint, entry_side: OrderSide) -> FixedPoint {
    if (entry_side == OrderSide::Buy && current.units() >= candidate.units())
        || (entry_side == OrderSide::Sell && current.units() <= candidate.units())
    {
        current
    } else {
        candidate
    }
}

const fn opposite_side(side: OrderSide) -> OrderSide {
    match side {
        OrderSide::Buy => OrderSide::Sell,
        OrderSide::Sell => OrderSide::Buy,
    }
}

fn managed_child_client_order_id(
    entry: &ClientOrderId,
    role: &str,
    index: u32,
) -> Result<ClientOrderId, String> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in entry
        .as_str()
        .bytes()
        .chain([0])
        .chain(role.bytes())
        .chain(index.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ClientOrderId::try_new(format!("bracket-{role}-{hash:016x}"))
        .map_err(|error| format!("managed bracket client identity is invalid: {error}"))
}

fn mirrored_client_order_id(
    source: &ClientOrderId,
    target: &TradingAccountId,
) -> Result<ClientOrderId, String> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source
        .as_str()
        .bytes()
        .chain([0])
        .chain(target.as_str().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ClientOrderId::try_new(format!("copy-{hash:016x}"))
        .map_err(|error| format!("trade copier client identity is invalid: {error}"))
}

#[cfg(test)]
mod tests;
