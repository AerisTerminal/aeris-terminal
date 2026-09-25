//! Single in-process owner for broker-neutral trading state and simulated execution.
//!
//! The service owns its bounded command queue, canonical orders/fills/positions, and the one
//! embedded user-record store. Callers must invoke blocking request methods from background work.

mod risk;
mod store;

use aeris_instruments::{ContractMetadata, InstrumentId};
use aeris_trading::{
    AccountEnvironment, AccountPnl, ClientOrderId, Fill, FillId, FixedPoint, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position,
    TimeInForce, TradingAccount, TradingAccountId, TradingProvenance,
};
pub use risk::{RiskEvaluation, RiskLock, RiskProfile, TrailingDrawdownMode};
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
use store::{StoredState, TradingStore};

const COMMAND_CAPACITY: usize = 256;
const REPLY_CAPACITY: usize = 1;
const MAXIMUM_OPEN_ORDERS: usize = 4_096;
const MAXIMUM_SNAPSHOT_ITEMS: usize = 10_000;
const MAXIMUM_USER_RECORD_BYTES: usize = 1024 * 1024;
const SCHEMA_VERSION: u32 = 3;

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
    pub account_pnl: Vec<AccountPnl>,
    pub risk_profiles: Vec<RiskProfile>,
    pub risk_locks: Vec<RiskLock>,
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
    EvaluateRisk(PlaceOrder, Reply<RiskEvaluation>),
    Modify(ModifyOrder, Reply<Order>),
    Cancel(ClientOrderId, Reply<Order>),
    CancelAll(Option<TradingAccountId>, Reply<Vec<Order>>),
    RegisterRiskProfile(RiskProfile, Reply<()>),
    LockAccount(TradingAccountId, String, i64, Reply<()>),
    UnlockAccount(TradingAccountId, Reply<()>),
    KillSwitch(Option<TradingAccountId>, String, i64, Reply<usize>),
    Flatten(
        TradingAccountId,
        SimulatedMarketObservation,
        Reply<Vec<Fill>>,
    ),
    FlattenAll(SimulatedMarketObservation, Reply<Vec<Fill>>),
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
                    let _ = reply.send(self.place_order(order));
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
                .filter(|order| order.status == OrderStatus::Working)
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
        self.store.put_risk_profile(&profile)?;
        self.state
            .risk_profiles
            .insert(profile.account_id.clone(), profile);
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
        let order = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == *client_order_id)
            .cloned()
            .ok_or_else(|| "order client identifier is not registered".to_string())?;
        if order.status != OrderStatus::Working {
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
        self.bump_revision()?;
        Ok(cancelled)
    }

    fn modify_order(&mut self, command: ModifyOrder) -> Result<Order, String> {
        let order = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == command.client_order_id)
            .cloned()
            .ok_or_else(|| "order client identifier is not registered".to_string())?;
        if order.status != OrderStatus::Working {
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
            .filter(|order| order.status == OrderStatus::Working)
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

    fn place_order(&mut self, command: PlaceOrder) -> Result<Order, String> {
        if let Some(existing) = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == command.client_order_id)
        {
            return Ok(existing.clone());
        }
        let account = self
            .state
            .accounts
            .get(&command.account_id)
            .ok_or_else(|| "trading account is not registered".to_string())?;
        if account.environment != AccountEnvironment::Simulated {
            return Err("T1 routes orders only to the simulated venue".to_string());
        }
        self.evaluate_order_risk(&command)?;
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
            .filter(|order| order.status == OrderStatus::Working)
            .count()
            >= MAXIMUM_OPEN_ORDERS
        {
            return Err("simulated venue open-order limit reached".to_string());
        }
        let sequence = self.state.next_sequence;
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
            detail: Some("accepted by local simulated venue".to_string()),
            provenance: command.provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        self.store.insert_order(&order, &event, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        self.state.orders.insert(order.id.clone(), order.clone());
        self.bump_revision()?;
        Ok(order)
    }

    fn evaluate_order_risk(&mut self, command: &PlaceOrder) -> Result<RiskEvaluation, String> {
        if let Some(lock) = self.state.risk_locks.get(&command.account_id) {
            return Err(format!("account is risk-locked: {}", lock.reason));
        }
        let Some(profile) = self.state.risk_profiles.get(&command.account_id).cloned() else {
            return Ok(RiskEvaluation {
                warnings: Vec::new(),
                projected_contracts: command.quantity,
                current_realized_pnl: FixedPoint::try_new(0, 2)
                    .map_err(|error| error.to_string())?,
            });
        };
        if !profile.enabled {
            return Ok(RiskEvaluation {
                warnings: Vec::new(),
                projected_contracts: command.quantity,
                current_realized_pnl: FixedPoint::try_new(0, profile.daily_loss_limit.scale())
                    .map_err(|error| error.to_string())?,
            });
        }
        self.check_restriction(command, &profile)?;
        let current_realized = self.realized_since_session(&profile)?;
        self.check_loss_limits(command, &profile, current_realized)?;
        let projected_contracts = self.projected_contracts(command)?;
        let maximum_contracts = profile
            .max_contracts
            .exact_rescale(command.quantity.scale())
            .map_err(|error| error.to_string())?;
        if projected_contracts > maximum_contracts.units().unsigned_abs() {
            return Err("order exceeds the account maximum-contract rule".to_string());
        }
        let warnings = Self::risk_warnings(&profile, current_realized);
        Ok(RiskEvaluation {
            warnings,
            projected_contracts: FixedPoint::try_new(
                i64::try_from(projected_contracts)
                    .map_err(|_| "projected contract quantity overflowed".to_string())?,
                command.quantity.scale(),
            )
            .map_err(|error| error.to_string())?,
            current_realized_pnl: current_realized,
        })
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
        current_realized: FixedPoint,
    ) -> Result<(), String> {
        let loss = current_realized.units().unsigned_abs();
        if current_realized.units() < 0 && loss >= profile.daily_loss_limit.units().unsigned_abs() {
            self.lock_account(
                command.account_id.clone(),
                "daily loss limit reached".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: daily loss limit reached".to_string());
        }
        if let Some(drawdown) = profile.trailing_drawdown {
            let drawdown = drawdown
                .exact_rescale(current_realized.scale())
                .map_err(|error| error.to_string())?;
            if current_realized.units() < 0 && loss >= drawdown.units().unsigned_abs() {
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
        let current = self
            .state
            .positions
            .values()
            .filter(|position| position.account_id == command.account_id)
            .try_fold(0_u64, |total, position| {
                let quantity = position
                    .net_quantity
                    .exact_rescale(command.quantity.scale())
                    .map_err(|error| error.to_string())?;
                total
                    .checked_add(quantity.units().unsigned_abs())
                    .ok_or_else(|| "risk contract quantity overflowed".to_string())
            })?;
        command
            .quantity
            .units()
            .unsigned_abs()
            .checked_add(current)
            .ok_or_else(|| "risk contract quantity overflowed".to_string())
    }

    fn risk_warnings(profile: &RiskProfile, current_realized: FixedPoint) -> Vec<String> {
        if current_realized.units() < 0
            && current_realized.units().unsigned_abs() * 100
                >= profile.daily_loss_limit.units().unsigned_abs() * 80
        {
            vec!["account is within 20% of its daily loss limit".to_string()]
        } else {
            Vec::new()
        }
    }

    fn realized_since_session(&self, profile: &RiskProfile) -> Result<FixedPoint, String> {
        let account = self
            .state
            .accounts
            .get(&profile.account_id)
            .ok_or_else(|| "risk profile account is not registered".to_string())?;
        let mut positions = BTreeMap::new();
        for fill in self.state.fills.iter().filter(|fill| {
            fill.account_id == profile.account_id
                && fill.execution_unix_nanos >= profile.session_start_unix_nanos
        }) {
            let instrument = self
                .state
                .instruments
                .get(&fill.instrument_id)
                .ok_or_else(|| "risk fill instrument is not registered".to_string())?;
            let key = (fill.account_id.clone(), fill.instrument_id.clone());
            let position = next_position(positions.get(&key), fill, account, instrument)?;
            positions.insert(key, position);
        }
        positions
            .values()
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
        let candidates = market_fill_candidates(&self.state.orders, observation);
        let candidate_ids = candidates
            .iter()
            .map(|(order_id, _)| order_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let fills = self.execute_market_fills(candidates, observation, &instrument)?;
        let mut positions_changed = self.update_mark_to_market(observation, &instrument)?;
        positions_changed |= self.cancel_unfilled_immediate_orders(observation, &candidate_ids)?;
        if positions_changed {
            self.store.enforce_retention()?;
            self.enforce_memory_retention();
            self.bump_revision()?;
        }
        Ok(fills)
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
            let position = next_position(
                self.state
                    .positions
                    .get(&(order.account_id.clone(), order.instrument_id.clone())),
                &fill,
                self.state
                    .accounts
                    .get(&order.account_id)
                    .ok_or_else(|| "fill account disappeared".to_string())?,
                instrument,
            )?;
            self.store
                .insert_fill(&order, &event, &fill, &position, sequence + 1)?;
            self.state.next_sequence = sequence + 1;
            self.state.orders.insert(
                order.id.clone(),
                Order {
                    status: OrderStatus::Filled,
                    ..order
                },
            );
            self.state.order_events.push_back(event);
            self.state.fills.push_back(fill.clone());
            self.state.positions.insert(
                (fill.account_id.clone(), fill.instrument_id.clone()),
                position,
            );
            fills.push(fill);
        }
        Ok(fills)
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
            let unrealized = unrealized_pnl(&current, mark, instrument)?;
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
                order.status == OrderStatus::Working
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
        self.store
            .insert_fill(&order, &event, &fill, &position, sequence + 2)?;
        self.state.next_sequence = sequence + 2;
        self.state.order_events.push_back(event);
        self.state.fills.push_back(fill.clone());
        self.state.orders.insert(
            order.id.clone(),
            Order {
                status: OrderStatus::Filled,
                ..order
            },
        );
        self.state.positions.insert(key.clone(), position);
        Ok(Some(fill))
    }

    fn snapshot(&self) -> Result<TradingSnapshot, String> {
        if self.state.accounts.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.orders.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.positions.len() > MAXIMUM_SNAPSHOT_ITEMS
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
            account_pnl: self.account_pnl()?,
            risk_profiles: self.state.risk_profiles.values().cloned().collect(),
            risk_locks: self.state.risk_locks.values().cloned().collect(),
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
                .filter(|order| order.status != OrderStatus::Working)
                .min_by_key(|order| order.submitted_unix_nanos)
                .map(|order| order.id.clone())
            else {
                break;
            };
            self.state.orders.remove(&retired);
        }
    }
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
            order.status == OrderStatus::Working && order.instrument_id == observation.instrument_id
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

fn unrealized_pnl(
    position: &Position,
    mark: FixedPoint,
    instrument: &TradingInstrument,
) -> Result<FixedPoint, String> {
    let entry = position
        .average_entry_price
        .ok_or_else(|| "position average is unavailable for unrealized PnL".to_string())?;
    let point_value = instrument
        .contract
        .point_value
        .ok_or_else(|| "point value is unavailable for unrealized PnL".to_string())?;
    let price_difference = i128::from(mark.units()) - i128::from(entry.units());
    let raw = price_difference
        .checked_mul(i128::from(position.net_quantity.units().signum()))
        .and_then(|value| {
            value.checked_mul(i128::from(position.net_quantity.units().unsigned_abs()))
        })
        .and_then(|value| value.checked_mul(i128::from(point_value.units())))
        .ok_or_else(|| "unrealized PnL overflowed".to_string())?;
    let raw_scale = entry
        .scale()
        .checked_add(point_value.scale())
        .ok_or_else(|| "unrealized PnL scale overflowed".to_string())?;
    let raw = i64::try_from(raw).map_err(|_| "unrealized PnL overflowed".to_string())?;
    FixedPoint::try_new(raw, raw_scale)
        .map_err(|error| error.to_string())?
        .exact_rescale(position.realized_pnl.scale())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
