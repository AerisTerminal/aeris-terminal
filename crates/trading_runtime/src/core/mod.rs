//! Single in-process owner for broker-neutral trading state and simulated execution.
//!
//! The service owns its bounded command queue, canonical orders/fills/positions, and the one
//! embedded user-record store. Callers must invoke blocking request methods from background work.

use crate::{clock, round_trip, service, venue};
#[path = "../admission/mod.rs"]
mod admission;
#[path = "../emergency.rs"]
mod emergency;
#[path = "../execution/mod.rs"]
mod execution;
#[path = "../groups/mod.rs"]
mod groups;
#[path = "../ledger/mod.rs"]
pub(crate) mod ledger;
#[path = "../market/mod.rs"]
pub(crate) mod market;
#[path = "../risk/owner.rs"]
mod risk_owner;
#[cfg(test)]
pub(crate) use groups::chart_bracket_template;
use groups::{planned_leg, protection};
#[path = "../venue/broker.rs"]
mod broker;

use self::ledger::{FillAccounting, TradeCycle};
use self::market::feed::{MarketFeedInbox, QuoteBook};
pub use self::market::gating::ExecutionPolicy;
use crate::book::OrderBook;
pub use crate::copier::{
    MAXIMUM_COPIER_TARGETS, TradeCopierConfig, TradeCopierTarget, TradeCopyDispatch,
};
pub use crate::discipline::{
    DisciplineState, FAST_STOP_REENTRY_NANOS, RAPID_LOSS_COOLDOWN_NANOS, RAPID_LOSS_THRESHOLD,
    RAPID_LOSS_WINDOW_NANOS,
};
pub use crate::plan::{
    MAXIMUM_ALLOWED_SETUPS, MAXIMUM_CHECKLIST_ITEMS, MAXIMUM_PLAN_LEVELS, SessionAdherenceReview,
    SessionBias, SessionChecklistItem, SessionPlan, SessionPlanLevel,
};
pub use crate::risk::{
    EconomicEventRiskAction, EconomicEventRiskImportance, EconomicEventRiskOutcome,
    EconomicEventRiskRule, EconomicEventRiskTrigger, RiskEvaluation, RiskLock, RiskMeter,
    RiskProfile, RiskRuleState, TrailingDrawdownMode,
};
pub use crate::round_trip::{TradeLeg, TradeRoundTrip};
use crate::service::Command;
use crate::store::{FillPolicyPersistence, OrderStep, StoredState, TradingStore};
pub use crate::store::{SCHEMA_VERSION, StoreError};
pub use crate::strategy::{
    BracketStrategyTemplate, BracketTarget, BreakEvenRule, MAXIMUM_BRACKET_TARGETS,
    MAXIMUM_MANAGED_BRACKETS, MAXIMUM_STRATEGY_TEMPLATES, ManagedBracket, ManagedBracketStatus,
    ProtectiveOrder, ProtectiveOrderRole, TrailingStopRule,
};
use crate::venue::VenueInbox;
use aeris_instruments::{ContractMetadata, InstrumentId};
use aeris_trading::market::{MarketRoute, QuoteInterestSink};
use aeris_trading::venue_outbox::{VenueRequestReceiver, VenueRequestSender};
use aeris_trading::{
    AccountEnvironment, AccountPnl, BrokerPosition, ClientOrderId, Fill, FillId, FixedPoint, Order,
    OrderEvent, OrderEventId, OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position,
    RiskWarning, TimeInForce, TradingAccount, TradingAccountId, TradingProvenance,
    project_unrealized_pnl,
};
pub use groups::planned_leg::{MovePlannedBracketLeg, PlannedBracketLeg};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};

pub(crate) const COMMAND_CAPACITY: usize = 256;
pub(crate) const REPLY_CAPACITY: usize = 1;
pub(crate) const MAXIMUM_OPEN_ORDERS: usize = 4_096;
pub(crate) const MAXIMUM_SNAPSHOT_ITEMS: usize = 10_000;
pub(crate) const MAXIMUM_COPY_DISPATCHES: usize = 128;
pub(crate) const MAXIMUM_USER_RECORD_BYTES: usize = 1024 * 1024;
pub(crate) const LIVE_TRADING_DISABLED: &str =
    "cTrader live accounts are data-only; trading is disabled";
pub(crate) const DEMO_VENUE_UNAVAILABLE: &str = "cTrader demo venue is not connected";
pub(crate) const BROKER_PNL_UNAVAILABLE: &str =
    "the broker account is unreachable, so its open P&L for the loss rules is unknown";
/// One request per connected account with open broker positions, well inside cTrader's
/// 50 requests per second.
pub(crate) const UNREALIZED_PNL_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const RETENTION_INTERVAL: Duration = Duration::from_secs(3_600);
pub(crate) type Reply<T> = SyncSender<Result<T, String>>;

/// The owner derives the venue from the registered account, never from order provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VenueRoute {
    Simulated,
    CtraderDemo,
}

impl VenueRoute {
    /// Rejects live accounts before a trading command can reach any venue.
    ///
    /// # Errors
    /// Returns an error for invalid account routing or a data-only live account.
    pub fn from_account(account: &TradingAccount) -> Result<Self, String> {
        account.validate().map_err(|error| error.to_string())?;
        match (account.venue_id.as_str(), account.environment) {
            ("aeris-sim", AccountEnvironment::Simulated) => Ok(Self::Simulated),
            ("ctrader", AccountEnvironment::Demo) => Ok(Self::CtraderDemo),
            ("ctrader", AccountEnvironment::Live) => Err(LIVE_TRADING_DISABLED.into()),
            _ => Err("trading account route is invalid".into()),
        }
    }
}

/// How a bracket was placed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BracketPlacement {
    /// Simulated accounts: the owner manages the stop and target child orders locally.
    Managed(ManagedBracket),
    /// Broker accounts: the entry order carries server-side stop-loss and take-profit, which
    /// the broker holds and enforces.
    BrokerProtected(Order),
}

impl BracketPlacement {
    #[must_use]
    pub const fn entry_client_order_id(&self) -> &ClientOrderId {
        match self {
            Self::Managed(bracket) => &bracket.entry_client_order_id,
            Self::BrokerProtected(order) => &order.client_order_id,
        }
    }
}

/// Simulated closes fill immediately; broker closes are asynchronous requests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FlattenOutcome {
    pub fills: Vec<Fill>,
    /// Broker position IDs whose close requests have been submitted but not yet filled.
    pub pending_close_requests: Vec<String>,
    /// What could not be requested (for example with a broker venue disconnected); every
    /// other order and position in scope was still handled.
    pub incomplete: Option<String>,
}

pub(crate) fn respond<T>(reply: &Reply<T>, result: Result<T, String>) {
    // A retired caller cannot affect the authoritative owner.
    let _ = reply.send(result);
}

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
    pub(crate) fn validate(self) -> Result<Self, String> {
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
    pub clock: Arc<dyn clock::Clock>,
    pub execution: ExecutionPolicy,
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
/// Chart-authored prices converted to a bracket template on the trading owner's verified grid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceChartBracket {
    pub entry: PlaceOrder,
    pub stop_price: FixedPoint,
    pub target_price: FixedPoint,
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

/// One order modification with an optional owner-side optimistic revision fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionedModifyOrder {
    pub order: ModifyOrder,
    pub expected_revision: Option<u32>,
}

/// One cancellation with an optional owner-side optimistic revision fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionedCancelOrder {
    pub client_order_id: ClientOrderId,
    pub expected_revision: Option<u32>,
}

/// A stale command carries the authoritative order for rebase without another snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderCommandError {
    StaleRevision { current: Box<Order> },
    Other(String),
}

impl std::fmt::Display for OrderCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleRevision { current } => write!(
                formatter,
                "stale order revision; current revision is {}",
                current.revision
            ),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for OrderCommandError {}

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
    pub market_observation_error: Option<String>,
    pub accounts: Vec<TradingAccount>,
    /// Registered contract terms, including each instrument's provider symbol.
    pub instruments: Vec<TradingInstrument>,
    pub orders: Vec<Order>,
    pub order_events: Vec<OrderEvent>,
    pub fills: Vec<Fill>,
    /// Final realized P&L keyed by the fill that closed each trade cycle.
    pub completed_trade_pnl: BTreeMap<FillId, FixedPoint>,
    /// Realized P&L of each fill that reduced or closed a position, partial exits included.
    pub fill_realized_pnl: BTreeMap<FillId, FixedPoint>,
    /// Trades grouped from `fills`, newest activity first: one closed round trip per exit fill,
    /// partial or final, of simulated and broker trades, plus each open simulated trade (open
    /// broker trades are `broker_positions`).
    pub round_trips: Vec<TradeRoundTrip>,
    pub positions: Vec<Position>,
    pub broker_positions: Vec<BrokerPosition>,
    /// The last balance each broker account reported, in its currency.
    pub broker_balances: BTreeMap<TradingAccountId, FixedPoint>,
    /// Broker accounts the attached venue can reach now.
    pub connected_broker_accounts: BTreeSet<TradingAccountId>,
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
    /// The stop and targets each awaiting bracket places once its working entry fills.
    pub planned_bracket_legs: Vec<PlannedBracketLeg>,
    pub protective_orders: Vec<ProtectiveOrder>,
}

/// Trigger prices of the open orders protecting one practice position.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PositionProtection {
    pub stop_loss: Option<FixedPoint>,
    pub take_profit: Option<FixedPoint>,
}

impl TradingSnapshot {
    /// The protective role of an order: a managed bracket's stop or target, or a standalone
    /// stop loss or take profit.
    #[must_use]
    pub fn protective_role(&self, client_order_id: &ClientOrderId) -> Option<ProtectiveOrderRole> {
        self.managed_brackets
            .iter()
            .find_map(|bracket| protection::managed_bracket_role(bracket, client_order_id))
            .or_else(|| {
                self.protective_orders
                    .iter()
                    .find(|protective| &protective.client_order_id == client_order_id)
                    .map(|protective| protective.role)
            })
    }

    /// The open stop loss and take profit on an account's position in an instrument. With
    /// several of one role, such as scaled targets, the one that triggers first is reported.
    #[must_use]
    pub fn position_protection(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
    ) -> PositionProtection {
        let mut stop_loss: Option<(i64, FixedPoint)> = None;
        let mut take_profit: Option<(i64, FixedPoint)> = None;
        for order in self.orders.iter().filter(|order| {
            order.status.is_open()
                && &order.account_id == account_id
                && &order.instrument_id == instrument_id
        }) {
            let Some(role) = self.protective_role(&order.client_order_id) else {
                continue;
            };
            let Some((key, price)) = protection::protection_trigger(order, role) else {
                continue;
            };
            let slot = match role {
                ProtectiveOrderRole::StopLoss => &mut stop_loss,
                ProtectiveOrderRole::TakeProfit => &mut take_profit,
            };
            if slot.is_none_or(|(first, _)| key < first) {
                *slot = Some((key, price));
            }
        }
        PositionProtection {
            stop_loss: stop_loss.map(|(_, price)| price),
            take_profit: take_profit.map(|(_, price)| price),
        }
    }
}

/// User-authored definition for one durable local practice account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatePracticeAccount {
    pub display_name: String,
    pub starting_equity: FixedPoint,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingServiceStatus {
    pub schema_version: u32,
    pub revision: u64,
    pub account_count: usize,
    pub working_order_count: usize,
    pub position_count: usize,
    pub retention: TradingRetention,
    /// Last broker-event persistence or validation failure, if any.
    pub venue_error: Option<String>,
}

/// Cloneable command handle for the one in-process trading owner.
#[derive(Clone)]
pub struct TradingService {
    pub(crate) commands: SyncSender<Command>,
    pub(crate) runtime: Arc<TradingRuntime>,
}

pub(crate) struct TradingRuntime {
    stopping: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    market_feed: Arc<MarketFeedInbox>,
    pub(crate) venue_inbox: Arc<VenueInbox>,
}

pub(crate) struct TradingCore {
    pub(crate) store: TradingStore,
    state: StoredState,
    retention: TradingRetention,
    pub(crate) clock: Arc<dyn clock::Clock>,
    command_now: i64,
    next_retention_at: i64,
    copy_dispatches: std::collections::VecDeque<TradeCopyDispatch>,
    market_observation_error: Option<String>,
    /// Quote computation runs on a private projection; only its completed facts are persisted.
    quote_planning: bool,
    quote_commit_failed: bool,
    venue_error: Option<String>,
    market_feed: Arc<MarketFeedInbox>,
    quote_book: QuoteBook,
    interest_sink: Option<Arc<dyn QuoteInterestSink>>,
    ticket_interest: BTreeMap<InstrumentId, MarketRoute>,
    published_interest: Vec<MarketRoute>,
    interest_revision: u64,
    observation_cursors: BTreeMap<InstrumentId, ObservationCursor>,
    /// Recovery fence for pre-start quotes; each order owns its durable acceptance floor.
    recovery_quote_floor: i64,
    execution: ExecutionPolicy,
    venue_outbound: Option<VenueRequestSender>,
    venue_inbox: Weak<VenueInbox>,
    venue_ingestion: u64,
    /// Broker accounts the current venue generation has announced; the rest are not
    /// reachable now.
    connected_broker_accounts: BTreeSet<TradingAccountId>,
    #[cfg(test)]
    timer_calls: usize,
}

/// Latest canonical BBO applied per registered instrument. Market panes can
/// republish the same book revision, so the trading owner applies each
/// provider revision once and never regresses to an older one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObservationCursor {
    session_generation: u64,
    source_sequence: u64,
    bid_units: i64,
    ask_units: i64,
}

pub(crate) struct FillMemoryUpdate {
    position: Position,
    position_key: (TradingAccountId, InstrumentId),
    policy_update: FillPolicyUpdate,
    next_sequence: u64,
}

pub(crate) struct FillPolicyUpdate {
    rule_state: Option<RiskRuleState>,
    trade_cycle: TradeCycle,
    discipline_state: DisciplineState,
    accounting: FillAccounting,
}

impl TradingService {
    /// Opens/migrates the local store and starts the single trading owner.
    ///
    /// This performs disk I/O and must run before GPUI starts or on background work.
    ///
    /// # Errors
    /// Returns an error when configuration, storage, migration, or worker startup fails.
    pub fn start(config: TradingServiceConfig) -> Result<Self, StoreError> {
        let retention = config.retention.validate()?;
        let execution = config
            .execution
            .validate()
            .map_err(|error| StoreError::Other(error.to_string()))?;
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (started_tx, started_rx) = mpsc::sync_channel(REPLY_CAPACITY);
        let database_path = config.database_path;
        let clock = config.clock;
        let stopping = Arc::new(AtomicBool::new(false));
        let market_feed = Arc::new(MarketFeedInbox::new(
            commands.clone(),
            Arc::clone(&stopping),
        ));
        let coordinator_market_feed = Arc::clone(&market_feed);
        let venue_inbox = VenueInbox::new(commands.clone(), Arc::clone(&stopping));
        let coordinator_venue_inbox = Arc::downgrade(&venue_inbox);
        let worker = thread::Builder::new()
            .name(service::owner::OWNER_THREAD_NAME.to_string())
            .spawn(move || {
                let coordinator = TradingCore::open(
                    &database_path,
                    retention,
                    clock,
                    execution,
                    coordinator_venue_inbox,
                    coordinator_market_feed,
                );
                match coordinator {
                    Ok(mut coordinator) => {
                        if started_tx.send(Ok(())).is_ok() {
                            service::owner::run(&mut coordinator, &receiver);
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
                stopping,
                worker: Mutex::new(Some(worker)),
                market_feed,
                venue_inbox,
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

    /// Creates one durable simulated account with user-selected opening equity.
    ///
    /// # Errors
    /// Returns an error for an invalid name/equity, account-capacity exhaustion,
    /// owner overload, or a storage failure.
    pub fn create_practice_account(
        &self,
        account: CreatePracticeAccount,
    ) -> Result<TradingAccount, String> {
        self.request(|reply| Command::CreatePracticeAccount(account, reply))
    }

    /// Permanently removes one simulated practice account and all of its local trading state.
    ///
    /// This is the destructive counterpart to user confirmation in the desktop UI: working
    /// orders, open positions, fills/history, risk state, session plans, copier references and
    /// locally-managed brackets owned by the account are removed atomically with the account.
    ///
    /// # Errors
    /// Returns an error when the account is missing, is not simulated, persistence fails, or the
    /// owner queue is unavailable.
    pub fn delete_practice_account(
        &self,
        account_id: TradingAccountId,
    ) -> Result<TradingAccount, String> {
        self.request(|reply| Command::DeletePracticeAccount(account_id, reply))
    }

    /// Adds or replaces provider-sourced contract terms.
    ///
    /// # Errors
    /// Returns an error for invalid metadata, overload, or a storage failure.
    pub fn register_instrument(&self, instrument: TradingInstrument) -> Result<(), String> {
        self.request(|reply| Command::RegisterInstrument(instrument, reply))
    }

    /// Attaches the single market interest owner, then republishes recovered routes.
    ///
    /// # Errors
    /// Returns an error when the owner or interest sink cannot accept the attachment.
    pub fn attach_market_feed(
        &self,
        sink: Arc<dyn QuoteInterestSink>,
    ) -> Result<Arc<MarketFeedInbox>, String> {
        self.request(|reply| Command::AttachMarketFeed(sink, reply))?;
        Ok(Arc::clone(&self.runtime.market_feed))
    }

    /// Replaces bounded ticket demand; tickets never become execution quote authorities.
    ///
    /// # Errors
    /// Returns an error for invalid, duplicate or excessive routes, or owner overload.
    pub fn set_ticket_interest(&self, routes: Vec<MarketRoute>) -> Result<(), String> {
        self.request(|reply| Command::SetTicketInterest(routes, reply))
    }

    /// Submits one order through the authoritative trading command path.
    ///
    /// # Errors
    /// Returns an error when validation, routing, capacity, or persistence fails.
    pub fn place_order(&self, order: PlaceOrder) -> Result<Order, String> {
        self.request(|reply| Command::Place(order, reply))
    }

    /// Submits a desktop instruction using only the owner's current validated market quote.
    /// `at_touch` prices a limit or stop instruction at the current touch.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or rejected order.
    pub fn place_order_from_market(
        &self,
        order: PlaceOrder,
        at_touch: bool,
    ) -> Result<Order, String> {
        self.request(|reply| Command::PlaceFromMarket(order, at_touch, reply))
    }

    /// Submits a template entry using the owner's quote.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or rejected bracket.
    pub fn place_bracket_from_market(
        &self,
        bracket: PlaceBracket,
    ) -> Result<BracketPlacement, String> {
        self.request(|reply| Command::PlaceBracketFromMarket(bracket, reply))
    }

    /// Submits an inline bracket using the owner's quote.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or rejected bracket.
    pub fn place_inline_bracket_from_market(
        &self,
        bracket: PlaceInlineBracket,
    ) -> Result<BracketPlacement, String> {
        self.request(|reply| Command::PlaceInlineBracketFromMarket(bracket, reply))
    }
    /// Places chart-authored bracket levels using verified instrument terms and the owner quote.
    ///
    /// # Errors
    /// Returns an error for unavailable terms or quote, off-grid levels, or rejected admission.
    pub fn place_chart_bracket_from_market(
        &self,
        bracket: PlaceChartBracket,
    ) -> Result<BracketPlacement, String> {
        self.request(|reply| Command::PlaceChartBracketFromMarket(bracket, reply))
    }

    /// Submits a protective order using the owner's quote.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or invalid protection.
    pub fn place_protective_order_from_market(
        &self,
        order: PlaceProtectiveOrder,
    ) -> Result<Order, String> {
        self.request(|reply| Command::PlaceProtectiveFromMarket(order, reply))
    }

    /// Modifies an order using the owner's quote provenance.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or invalid modification.
    pub fn modify_order_from_market(&self, order: ModifyOrder) -> Result<Order, String> {
        self.request(|reply| Command::ModifyFromMarket(order, reply))
    }

    /// Attaches a bounded demo-venue writer under the next session generation, which the
    /// owner assigns and persists. The relay stamps every event with that generation, and
    /// the receiver (64 slots) belongs to it alone; any earlier generation is retired.
    ///
    /// # Errors
    /// Returns an error when the owner is unavailable or the generation cannot be stored.
    pub fn attach_demo_venue(&self) -> Result<(u64, VenueRequestReceiver), String> {
        self.request(Command::AttachVenue)
    }

    /// Returns the shared bounded event inbox for the broker reader.
    #[must_use]
    pub fn demo_venue_inbox(&self) -> Arc<VenueInbox> {
        Arc::clone(&self.runtime.venue_inbox)
    }

    /// Submits one entry whose stop and scale-out targets activate after its fill.
    ///
    /// # Errors
    /// Returns an error when the template, entry, allocation, risk, or persistence is invalid.
    pub fn place_bracket(&self, bracket: PlaceBracket) -> Result<BracketPlacement, String> {
        self.request(|reply| Command::PlaceBracket(bracket, reply))
    }

    /// Submits a chart-authored bracket with exact, inline tick offsets.
    ///
    /// # Errors
    /// Returns an error when the entry, template, risk, or persistence is invalid.
    pub fn place_inline_bracket(
        &self,
        bracket: PlaceInlineBracket,
    ) -> Result<BracketPlacement, String> {
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

    /// Checks the revision at the trading owner before modifying either venue.
    ///
    /// # Errors
    /// Returns the current order on stale revision; other command failures remain explicit.
    pub fn modify_order_checked(
        &self,
        command: RevisionedModifyOrder,
    ) -> Result<Order, OrderCommandError> {
        self.request(|reply| Command::ModifyChecked(command, reply))
            .map_err(OrderCommandError::Other)?
            .map_err(|current| OrderCommandError::StaleRevision { current })
    }

    /// Fences a chart/ladder modify against the owner's current revision before reading its
    /// verified market quote. A stale drag cannot reprice a replaced or filled order.
    ///
    /// # Errors
    /// Returns the current order on stale revision; otherwise reports quote or command failure.
    pub fn modify_order_from_market_checked(
        &self,
        command: RevisionedModifyOrder,
    ) -> Result<Order, OrderCommandError> {
        self.request(|reply| Command::ModifyFromMarketChecked(command, reply))
            .map_err(OrderCommandError::Other)?
            .map_err(|current| OrderCommandError::StaleRevision { current })
    }

    /// Moves the stop or a target a pending bracket places once its entry fills.
    ///
    /// # Errors
    /// Returns an error when the order is not a planned leg, the price is on the wrong side of
    /// the entry or off the tick grid, the stop would breach a risk limit, or storage fails.
    pub fn move_planned_bracket_leg(
        &self,
        command: MovePlannedBracketLeg,
    ) -> Result<PlannedBracketLeg, String> {
        self.request(|reply| Command::MovePlannedLeg(command, reply))
    }

    /// Cancels one working order through the authoritative command path.
    ///
    /// # Errors
    /// Returns an error when the order is missing, storage fails, or the owner is unavailable.
    pub fn cancel_order(&self, client_order_id: ClientOrderId) -> Result<Order, String> {
        self.request(|reply| Command::Cancel(client_order_id, reply))
    }

    /// Checks the revision at the trading owner before cancelling either venue.
    ///
    /// # Errors
    /// Returns the current order on stale revision; other command failures remain explicit.
    pub fn cancel_order_checked(
        &self,
        command: RevisionedCancelOrder,
    ) -> Result<Order, OrderCommandError> {
        self.request(|reply| Command::CancelChecked(command, reply))
            .map_err(OrderCommandError::Other)?
            .map_err(|current| OrderCommandError::StaleRevision { current })
    }

    /// Cancels every working order for one account, or all accounts when no account is supplied.
    ///
    /// # Errors
    /// Returns an error when cancellation persistence fails or the owner queue is unavailable.
    pub fn cancel_all(&self, account_id: Option<TradingAccountId>) -> Result<Vec<Order>, String> {
        self.request(|reply| Command::CancelAll(account_id, reply))
    }

    /// Requests an asynchronous close of a broker position. No simulation is fabricated.
    ///
    /// # Errors
    /// Returns an error for non-broker or live accounts, an empty position ID, or an unattached demo venue.
    pub fn close_broker_position(
        &self,
        account_id: TradingAccountId,
        broker_position_id: String,
    ) -> Result<(), String> {
        self.request(|reply| Command::CloseBrokerPosition(account_id, broker_position_id, reply))
    }

    /// Requests broker-native stop-loss and take-profit changes.
    ///
    /// # Errors
    /// Returns an error for non-broker or live accounts, empty inputs, or an unattached demo venue.
    pub fn amend_broker_position_sltp(
        &self,
        account_id: TradingAccountId,
        broker_position_id: String,
        stop_loss: Option<FixedPoint>,
        take_profit: Option<FixedPoint>,
    ) -> Result<(), String> {
        self.request(|reply| {
            Command::AmendBrokerPositionSltp(
                account_id,
                broker_position_id,
                stop_loss,
                take_profit,
                reply,
            )
        })
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
    #[cfg(test)]
    pub fn flatten_account(
        &self,
        account_id: TradingAccountId,
        observation: SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::Flatten(account_id, observation, reply))
    }

    /// Closes and reverses the selected account's position for the observed instrument.
    ///
    /// # Errors
    /// Returns an error when the position is flat, the observation is invalid,
    /// persistence fails, or the replacement side violates account risk.
    #[cfg(test)]
    pub fn reverse_position(
        &self,
        account_id: TradingAccountId,
        observation: SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::Reverse(account_id, observation, reply))
    }

    /// Cancels working orders and closes simulated positions for every account at the observed BBO.
    ///
    /// # Errors
    /// Returns an error when the observation is invalid, no accounts are registered, or
    /// persistence fails. This administrative action remains available while risk-locked.
    #[cfg(test)]
    pub fn flatten_all(
        &self,
        observation: SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::FlattenAll(observation, reply))
    }

    /// Flattens the selected instrument using only the owner's validated quote.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or failed close.
    pub fn flatten_account_at(
        &self,
        account: TradingAccountId,
        instrument: InstrumentId,
    ) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::FlattenAt(account, instrument, reply))
    }

    /// Reverses the selected instrument using only the owner's validated quote.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or failed reversal.
    pub fn reverse_position_at(
        &self,
        account: TradingAccountId,
        instrument: InstrumentId,
    ) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::ReverseAt(account, instrument, reply))
    }

    /// Flattens all accounts for the selected instrument using only the owner's validated quote.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or failed close.
    pub fn flatten_all_at(&self, instrument: InstrumentId) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::FlattenAllAt(instrument, reply))
    }

    /// Applies every enabled account event rule once, durably fenced by event/profile identity.
    /// A flatten rule remains pending until the caller supplies a valid market observation.
    ///
    /// # Errors
    /// Returns an error when the trigger or observation is invalid, a complete flatten cannot be
    /// priced, the runtime is overloaded, or durable rule state cannot be written.
    #[cfg(test)]
    pub fn apply_economic_event_risk(
        &self,
        event: EconomicEventRiskTrigger,
        observation: Option<SimulatedMarketObservation>,
    ) -> Result<EconomicEventRiskOutcome, String> {
        self.request(|reply| Command::ApplyEconomicEventRisk(event, observation, reply))
    }

    /// Applies an event rule using the owner's quote for the selected instrument.
    ///
    /// # Errors
    /// Returns an error for a rejected rule or unavailable owner.
    pub fn apply_economic_event_risk_at(
        &self,
        event: EconomicEventRiskTrigger,
        instrument: Option<InstrumentId>,
    ) -> Result<EconomicEventRiskOutcome, String> {
        self.request(|reply| Command::ApplyEconomicEventRiskAt(event, instrument, reply))
    }

    /// Applies one market observation and returns fills produced by the simulated venue.
    ///
    /// # Errors
    /// Returns an error when the observation is invalid or a fill cannot be persisted exactly.
    #[cfg(test)]
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
        self.runtime.venue_inbox.wake_stopped();
        let (reply_tx, reply_rx) = mpsc::sync_channel(REPLY_CAPACITY);
        match self.commands.try_send(Command::Shutdown(reply_tx)) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.runtime.stopping.store(false, Ordering::Release);
                return Err("trading command queue is full during shutdown".to_string());
            }
        }
        let shutdown_clock = clock::SystemClock::default();
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
                if worker.is_some_and(|worker| worker.join().is_err()) {
                    return Err("trading owner panicked during shutdown".to_string());
                }
                return reply_rx
                    .recv()
                    .map_err(|_| "trading owner stopped before checkpointing".to_string())?;
            }
            let elapsed = clock::Clock::monotonic(&shutdown_clock);
            if elapsed >= timeout {
                return Err("trading owner shutdown deadline expired".to_string());
            }
            thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(elapsed)));
        }
    }

    pub(crate) fn request<T>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, String> {
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
        self.venue_inbox.wake_stopped();
    }
}

pub(crate) fn observation_revision(observation: &SimulatedMarketObservation) -> (u64, u64) {
    (
        observation.provenance.session_generation,
        observation.provenance.source_sequence,
    )
}

impl TradingCore {
    pub(crate) fn open(
        database_path: &std::path::Path,
        retention: TradingRetention,
        clock: Arc<dyn clock::Clock>,
        execution: ExecutionPolicy,
        venue_inbox: Weak<VenueInbox>,
        market_feed: Arc<MarketFeedInbox>,
    ) -> Result<Self, StoreError> {
        let mut store = TradingStore::open(database_path, retention)?;
        let state = store.load_state().map_err(StoreError::Other)?;
        let recovery_quote_floor = clock.wall_unix_nanos();
        let mut core = Self {
            store,
            state,
            retention,
            command_now: clock.wall_unix_nanos(),
            next_retention_at: clock.wall_unix_nanos().saturating_add(3_600_000_000_000),
            clock,
            copy_dispatches: std::collections::VecDeque::new(),
            market_observation_error: None,
            quote_planning: false,
            quote_commit_failed: false,
            venue_error: None,
            market_feed,
            quote_book: QuoteBook::default(),
            interest_sink: None,
            ticket_interest: BTreeMap::new(),
            published_interest: Vec::new(),
            interest_revision: 0,
            observation_cursors: BTreeMap::new(),
            recovery_quote_floor,
            execution,
            venue_outbound: None,
            venue_inbox,
            venue_ingestion: 0,
            connected_broker_accounts: BTreeSet::new(),
            #[cfg(test)]
            timer_calls: 0,
        };
        core.repair_loaded_position_projections()
            .map_err(StoreError::Other)?;
        core.align_loaded_protection().map_err(StoreError::Other)?;
        Ok(core)
    }

    fn repair_loaded_position_projections(&mut self) -> Result<(), String> {
        let keys = self.state.positions.keys().cloned().collect::<Vec<_>>();
        let mut repaired = Vec::new();
        for key in keys {
            let Some(stored) = self.state.positions.get(&key).cloned() else {
                continue;
            };
            if !self.position_projection_needs_repair(&stored) {
                continue;
            }
            let Some(rebuilt) = self.rebuild_position_from_retained_fills(&key) else {
                continue;
            };
            // Fills are the durable ledger and positions are a projection. Only replace a loaded
            // projection when the retained ledger reproduces both the current net quantity and
            // the last-fill boundary. This prevents retention-truncated history from rewriting a
            // still-valid open position while repairing projections produced by older buggy math.
            if rebuilt.net_quantity == stored.net_quantity
                && rebuilt.last_fill_unix_nanos == stored.last_fill_unix_nanos
                && rebuilt != stored
            {
                repaired.push((key, rebuilt));
            }
        }
        if repaired.is_empty() {
            return Ok(());
        }
        for (key, position) in repaired {
            self.store.update_position(&position)?;
            self.state.positions.insert(key, position);
        }
        self.bump_revision()
    }

    fn position_projection_needs_repair(&self, position: &Position) -> bool {
        let account_scale_mismatch =
            self.state
                .accounts
                .get(&position.account_id)
                .is_some_and(|account| {
                    position.realized_pnl.scale() != account.currency_scale
                        || position.unrealized_pnl.scale() != account.currency_scale
                });
        let instrument_scale_mismatch = self
            .state
            .instruments
            .get(&position.instrument_id)
            .is_some_and(|instrument| {
                position.net_quantity.scale() != instrument.quantity_scale
                    || position
                        .average_entry_price
                        .is_some_and(|average| average.scale() != instrument.price_scale)
            });
        let lifecycle_mismatch = if position.net_quantity.units() == 0 {
            position.average_entry_price.is_some() || position.unrealized_pnl.units() != 0
        } else {
            position.average_entry_price.is_none()
        };
        account_scale_mismatch || instrument_scale_mismatch || lifecycle_mismatch
    }

    fn rebuild_position_from_retained_fills(
        &self,
        key: &(TradingAccountId, InstrumentId),
    ) -> Option<Position> {
        let account = self.state.accounts.get(&key.0)?;
        let instrument = self.state.instruments.get(&key.1)?;
        let mut rebuilt: Option<Position> = None;
        let mut saw_fill = false;
        for fill in self
            .state
            .fills
            .iter()
            .filter(|fill| fill.account_id == key.0 && fill.instrument_id == key.1)
        {
            saw_fill = true;
            rebuilt = Some(next_position(rebuilt.as_ref(), fill, account, instrument).ok()?);
        }
        saw_fill.then_some(rebuilt).flatten()
    }

    /// Applies one command on the single owner. Returns false after a shutdown checkpoint.
    pub(crate) fn handle(&mut self, command: Command, now: i64) -> bool {
        self.command_now = self.command_now.max(now);
        match command {
            Command::Status(reply) => respond(&reply, Ok(self.status())),
            Command::RegisterAccount(account, reply) => {
                respond(&reply, self.register_account(account));
            }
            Command::CreatePracticeAccount(account, reply) => {
                respond(&reply, self.create_practice_account(&account));
            }
            Command::DeletePracticeAccount(account_id, reply) => {
                respond(&reply, self.delete_practice_account(&account_id));
            }
            Command::RegisterInstrument(instrument, reply) => {
                respond(&reply, self.register_instrument(instrument));
            }
            Command::AttachMarketFeed(..)
            | Command::SetTicketInterest(..)
            | Command::DrainMarketFeed => self.dispatch_market_command(command),
            #[cfg(test)]
            Command::Observe(..) => self.dispatch_market_command(command),
            Command::Place(..)
            | Command::PlaceFromMarket(..)
            | Command::PlaceBracket(..)
            | Command::PlaceBracketFromMarket(..)
            | Command::PlaceInlineBracket(..)
            | Command::PlaceInlineBracketFromMarket(..)
            | Command::PlaceChartBracketFromMarket(..)
            | Command::PlaceProtective(..)
            | Command::PlaceProtectiveFromMarket(..)
            | Command::EvaluateRisk(..)
            | Command::Modify(..)
            | Command::ModifyChecked(..)
            | Command::ModifyFromMarket(..)
            | Command::ModifyFromMarketChecked(..)
            | Command::MovePlannedLeg(..)
            | Command::Cancel(..)
            | Command::CancelChecked(..)
            | Command::CancelAll(..)
            | Command::CloseBrokerPosition(..)
            | Command::AmendBrokerPositionSltp(..) => {
                self.dispatch_order_command(command);
            }
            Command::RegisterRiskProfile(profile, reply) => {
                respond(&reply, self.register_risk_profile(profile));
            }
            Command::RegisterSessionPlan(plan, reply) => {
                respond(&reply, self.register_session_plan(plan));
            }
            Command::RegisterTradeCopier(config, reply) => {
                respond(&reply, self.register_trade_copier(config));
            }
            Command::RegisterStrategyTemplate(template, reply) => {
                respond(&reply, self.register_strategy_template(template));
            }
            Command::LockAccount(..)
            | Command::UnlockAccount(..)
            | Command::KillSwitch(..)
            | Command::FlattenAt(..)
            | Command::ReverseAt(..)
            | Command::FlattenAllAt(..)
            | Command::ApplyEconomicEventRiskAt(..) => self.dispatch_account_command(command),
            #[cfg(test)]
            Command::Flatten(..)
            | Command::Reverse(..)
            | Command::FlattenAll(..)
            | Command::ApplyEconomicEventRisk(..) => self.dispatch_account_command(command),
            Command::PutUserRecord(record, reply) => {
                respond(&reply, self.put_user_record(&record));
            }
            Command::Snapshot(reply) => {
                let _ = reply.send(self.snapshot());
            }
            Command::Export(directory, reply) => {
                let _ = reply.send(self.store.export(&directory));
            }
            Command::AttachVenue(reply) => respond(&reply, self.attach_venue()),
            Command::DrainVenueEvents => self.drain_venue_events(),
            #[cfg(test)]
            Command::BlockOwnerForTest(ready, barrier) => {
                let _ = ready.send(());
                barrier.wait();
            }
            #[cfg(test)]
            Command::OwnerDiagnostics(reply) => respond(&reply, Ok(self.owner_diagnostics())),
            Command::Shutdown(reply) => {
                let result = self.store.checkpoint_for_shutdown();
                let _ = reply.send(result);
                return false;
            }
        }
        if let Err(error) = self.recompute_quote_interest() {
            self.market_observation_error = Some(error.to_string());
        }
        self.market_feed.wake_if_pending();
        true
    }

    #[cfg(test)]
    fn owner_diagnostics(&self) -> (String, usize) {
        (
            thread::current().name().unwrap_or_default().to_string(),
            self.timer_calls,
        )
    }

    /// Applies the bounded latest-value market batch under the owner clock.
    pub(crate) fn on_market(&mut self, batch: Vec<SimulatedMarketObservation>, now: i64) {
        self.command_now = self.command_now.max(now);
        for observation in batch {
            let previous_cursor = self
                .observation_cursors
                .get(&observation.instrument_id)
                .copied();
            if self.advance_observation_cursor(&observation) {
                match self.observe_market(&observation) {
                    Ok(_) => self.market_observation_error = None,
                    Err(error) if self.quote_commit_failed => {
                        if let Some(previous) = previous_cursor {
                            self.observation_cursors
                                .insert(observation.instrument_id.clone(), previous);
                        } else {
                            self.observation_cursors.remove(&observation.instrument_id);
                        }
                        aeris_observability::diagnostic!("trading quote commit failed: {error}");
                    }
                    Err(error) => self.market_observation_error = Some(error),
                }
            }
        }
        if let Err(error) = self.recompute_quote_interest() {
            self.market_observation_error = Some(error.to_string());
        }
    }

    /// Refreshes broker P&L only at a due deadline, never on an idle command wake.
    pub(crate) fn on_timer(&mut self, now: i64) {
        self.command_now = self.command_now.max(now);
        #[cfg(test)]
        {
            self.timer_calls += 1;
        }
        if let Err(error) = self.cancel_due_markets() {
            aeris_observability::diagnostic!("market order timeout persistence failed: {error}");
        }
        self.request_unrealized_pnl();
        if self.command_now >= self.next_retention_at {
            match self.store.enforce_retention() {
                Ok(()) => {
                    self.next_retention_at = self.command_now.saturating_add(3_600_000_000_000);
                    self.enforce_memory_retention();
                }
                Err(error) => aeris_observability::diagnostic!("trading retention failed: {error}"),
            }
        }
        if let Err(error) = self.recompute_quote_interest() {
            self.market_observation_error = Some(error.to_string());
        }
    }

    fn dispatch_account_command(&mut self, command: Command) {
        match command {
            Command::LockAccount(account_id, reason, locked_at, reply) => {
                respond(&reply, self.lock_account(account_id, reason, locked_at));
            }
            Command::UnlockAccount(account_id, reply) => {
                respond(&reply, self.unlock_account(&account_id));
            }
            Command::KillSwitch(account_id, reason, locked_at, reply) => {
                respond(
                    &reply,
                    self.kill_switch(account_id.as_ref(), &reason, locked_at),
                );
            }
            #[cfg(test)]
            Command::Flatten(account_id, observation, reply) => {
                respond(&reply, self.flatten_account(&account_id, &observation));
            }
            #[cfg(test)]
            Command::Reverse(account_id, observation, reply) => {
                respond(&reply, self.reverse_position(&account_id, &observation));
            }
            #[cfg(test)]
            Command::FlattenAll(observation, reply) => {
                respond(&reply, self.flatten_all(&observation));
            }
            Command::FlattenAt(account, instrument, reply) => {
                respond(
                    &reply,
                    self.market_observation(&instrument)
                        .and_then(|quote| self.flatten_account(&account, &quote)),
                );
            }
            Command::ReverseAt(account, instrument, reply) => {
                respond(
                    &reply,
                    self.market_observation(&instrument)
                        .and_then(|quote| self.reverse_position(&account, &quote)),
                );
            }
            Command::FlattenAllAt(instrument, reply) => {
                respond(
                    &reply,
                    self.market_observation(&instrument)
                        .and_then(|quote| self.flatten_all(&quote)),
                );
            }
            #[cfg(test)]
            Command::ApplyEconomicEventRisk(event, observation, reply) => {
                respond(
                    &reply,
                    self.apply_economic_event_risk(&event, observation.as_ref()),
                );
            }
            Command::ApplyEconomicEventRiskAt(event, instrument, reply) => {
                let quote = instrument
                    .as_ref()
                    .and_then(|id| self.quote_book.observation(id));
                respond(
                    &reply,
                    self.apply_economic_event_risk(&event, quote.as_ref()),
                );
            }
            _ => unreachable!("only account commands are dispatched here"),
        }
    }

    fn put_user_record(&mut self, record: &UserRecord) -> Result<(), String> {
        record
            .validate()
            .and_then(|()| self.store.put_user_record(record))
    }

    fn drain_venue_events(&mut self) {
        if let Some(inbox) = self.venue_inbox.upgrade() {
            for event in inbox.drain() {
                self.venue_error = self
                    .apply_venue_event(&event)
                    .err()
                    .or(self.venue_error.take());
            }
        }
    }

    fn dispatch_order_command(&mut self, command: Command) {
        match command {
            Command::Place(order, reply) => respond(&reply, self.place_order(&order)),
            Command::PlaceFromMarket(order, at_touch, reply) => {
                respond(
                    &reply,
                    self.market_order(order, at_touch)
                        .and_then(|order| self.place_order(&order)),
                );
            }
            Command::PlaceBracket(bracket, reply) => respond(&reply, self.place_bracket(&bracket)),
            Command::PlaceBracketFromMarket(bracket, reply) => {
                respond(
                    &reply,
                    self.market_order(bracket.entry, true).and_then(|entry| {
                        self.place_bracket(&PlaceBracket {
                            entry,
                            template_id: bracket.template_id,
                        })
                    }),
                );
            }
            Command::PlaceInlineBracket(bracket, reply) => respond(
                &reply,
                self.place_bracket_with_template(&bracket.entry, bracket.template),
            ),
            Command::PlaceInlineBracketFromMarket(bracket, reply) => {
                respond(&reply, self.place_inline_bracket_from_market(bracket));
            }
            Command::PlaceChartBracketFromMarket(bracket, reply) => {
                respond(&reply, self.place_chart_bracket(bracket));
            }
            Command::PlaceProtective(order, reply) => {
                respond(&reply, self.place_protective_order(order));
            }
            Command::PlaceProtectiveFromMarket(order, reply) => {
                respond(
                    &reply,
                    self.market_order(order.order, false)
                        .and_then(|order_command| {
                            self.place_protective_order(PlaceProtectiveOrder {
                                order: order_command,
                                role: order.role,
                            })
                        }),
                );
            }
            Command::EvaluateRisk(order, reply) => {
                respond(&reply, self.evaluate_order_risk(&order));
            }
            Command::Modify(order, reply) => respond(&reply, self.modify_order(order)),
            Command::ModifyChecked(command, reply) => {
                respond(&reply, self.modify_order_checked_internal(command));
            }
            Command::ModifyFromMarket(order, reply) => {
                respond(&reply, self.modify_order_from_market(order));
            }
            Command::ModifyFromMarketChecked(command, reply) => {
                respond(
                    &reply,
                    self.modify_order_from_market_checked_internal(command),
                );
            }
            Command::MovePlannedLeg(command, reply) => {
                respond(&reply, self.move_planned_bracket_leg(&command));
            }
            Command::Cancel(client_order_id, reply) => {
                respond(&reply, self.cancel_order(&client_order_id));
            }
            Command::CancelChecked(command, reply) => {
                respond(&reply, self.cancel_order_checked_internal(&command));
            }
            Command::CancelAll(account_id, reply) => {
                respond(&reply, self.cancel_all(account_id.as_ref()));
            }
            Command::CloseBrokerPosition(account_id, position_id, reply) => respond(
                &reply,
                self.require_broker_route(&account_id)
                    .and_then(|()| self.close_broker_position(&account_id, &position_id)),
            ),
            Command::AmendBrokerPositionSltp(
                account_id,
                position_id,
                stop_loss,
                take_profit,
                reply,
            ) => respond(
                &reply,
                self.require_broker_route(&account_id).and_then(|()| {
                    self.amend_broker_position_protection(
                        &account_id,
                        &position_id,
                        stop_loss,
                        take_profit,
                    )
                }),
            ),
            _ => {}
        }
    }

    fn market_observation(
        &self,
        instrument: &InstrumentId,
    ) -> Result<SimulatedMarketObservation, String> {
        if let Some(terms) = self.state.instruments.get(instrument) {
            self.check_practice_session(terms)
                .map_err(|reason| reason.to_string())?;
        }
        self.quote_book
            .observation(instrument)
            .ok_or_else(|| "a current live market quote is unavailable".to_string())
    }

    fn market_order(&self, mut order: PlaceOrder, at_touch: bool) -> Result<PlaceOrder, String> {
        let quote = self.market_observation(&order.instrument_id)?;
        if at_touch {
            let price = match order.side {
                OrderSide::Buy => quote.ask,
                OrderSide::Sell => quote.bid,
            };
            match order.order_type {
                OrderType::Market => {}
                OrderType::Limit | OrderType::RestingAt => order.limit_price = Some(price),
                OrderType::Stop => order.stop_price = Some(price),
                OrderType::StopLimit => {
                    order.limit_price = Some(price);
                    order.stop_price = Some(price);
                }
            }
        }
        order.provenance = quote.provenance;
        Ok(order)
    }

    fn market_resting_bracket_entry(&self, mut entry: PlaceOrder) -> Result<PlaceOrder, String> {
        let quote = self.market_observation(&entry.instrument_id)?;
        if entry.limit_price.is_none() {
            return Err("bracket entry price is unavailable".to_string());
        }
        entry.order_type = OrderType::RestingAt;
        entry.provenance = quote.provenance;
        self.resolve_order_instruction(entry)
            .map_err(|reason| reason.to_string())
    }

    fn status(&self) -> TradingServiceStatus {
        TradingServiceStatus {
            schema_version: SCHEMA_VERSION,
            revision: self.state.revision,
            account_count: self.state.accounts.len(),
            working_order_count: self.state.orders.open_count(),
            position_count: self.state.positions.len(),
            retention: self.retention,
            venue_error: self.venue_error.clone(),
        }
    }

    fn register_trade_copier(&mut self, config: TradeCopierConfig) -> Result<(), String> {
        if self.route(&config.source_account_id)? != VenueRoute::Simulated {
            return Err("trade copier is unavailable for broker accounts".into());
        }
        for target in &config.targets {
            if self.route(&target.account_id)? != VenueRoute::Simulated {
                return Err("trade copier is unavailable for broker accounts".into());
            }
        }
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

    fn bump_revision(&mut self) -> Result<(), String> {
        if self.quote_planning {
            self.state.revision = self.state.revision.saturating_add(1).min(i64::MAX as u64);
        } else {
            self.state.revision = self.store.revision()?;
        }
        Ok(())
    }

    fn commit_order_step(
        &mut self,
        step: OrderStep,
        dispatches: Vec<TradeCopyDispatch>,
    ) -> Result<(), String> {
        if step.inserted.is_empty()
            && step.updated.is_empty()
            && step.brackets.is_empty()
            && step.deleted_brackets.is_empty()
        {
            return Ok(());
        }
        let revision_increment =
            u64::try_from(step.updated.len().max(1)).map_err(|error| error.to_string())?;
        let committed_revision = self
            .state
            .revision
            .saturating_add(revision_increment)
            .min(i64::MAX as u64);
        if !self.quote_planning {
            self.store.commit_order_step(&step)?;
        }
        self.state.next_sequence = step.next_sequence;
        for id in step.deleted_brackets {
            self.state.managed_brackets.remove(&id);
        }
        for bracket in step.brackets {
            self.state
                .managed_brackets
                .insert(bracket.bracket_id.clone(), bracket);
        }
        for protective in step.protective {
            self.state
                .protective_orders
                .insert(protective.client_order_id.clone(), protective);
        }
        for (order, event) in step.inserted.into_iter().chain(step.updated) {
            self.state.orders.insert(order.id.clone(), order);
            self.state.order_events.push_back(event);
        }
        for dispatch in dispatches {
            self.copy_dispatches.push_back(dispatch);
            while self.copy_dispatches.len() > MAXIMUM_COPY_DISPATCHES {
                self.copy_dispatches.pop_front();
            }
        }
        self.state.revision = committed_revision;
        Ok(())
    }

    fn enforce_memory_retention(&mut self) {
        while self.state.order_events.len() > self.retention.maximum_order_events {
            self.state.order_events.pop_front();
        }
        while self.state.fills.len() > self.retention.maximum_fills {
            if let Some(fill) = self.state.fills.pop_front() {
                self.state.fill_accounting.remove(&fill.id);
                self.state.broker_fill_positions.remove(&fill.id);
            }
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

pub(crate) fn fill_policy_persistence(update: &FillPolicyUpdate) -> FillPolicyPersistence<'_> {
    FillPolicyPersistence {
        rule_state: update.rule_state.as_ref(),
        trade_cycle: &update.trade_cycle,
        discipline_state: &update.discipline_state,
        accounting: update.accounting,
    }
}

pub(crate) fn order_acceptance_detail(warnings: &[RiskWarning]) -> String {
    if warnings.is_empty() {
        return "accepted by local simulated venue".to_string();
    }
    warnings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" · ")
        .chars()
        .take(256)
        .collect()
}

pub(crate) fn flatten_acceptance_event(order: &Order, sequence: u64) -> Result<OrderEvent, String> {
    let event = OrderEvent {
        id: OrderEventId::try_new(format!("sim-flatten-event-{sequence}"))
            .map_err(|error| error.to_string())?,
        order_id: order.id.clone(),
        sequence,
        kind: OrderEventKind::Accepted,
        reason: None,
        event_unix_nanos: order.submitted_unix_nanos,
        detail: Some("accepted by local flatten command".to_string()),
        provenance: order.provenance.clone(),
    };
    event.validate().map_err(|error| error.to_string())?;
    Ok(event)
}

pub(crate) fn flatten_fill_event(
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
        reason: None,
        event_unix_nanos: fill.execution_unix_nanos,
        detail: Some("filled by local flatten command".to_string()),
        provenance,
    };
    event.validate().map_err(|error| error.to_string())?;
    Ok(event)
}

pub(crate) fn touch_price(
    order: &Order,
    observation: &SimulatedMarketObservation,
) -> Option<FixedPoint> {
    match (order.side, order.order_type) {
        (OrderSide::Buy, OrderType::Market) => Some(observation.ask),
        (OrderSide::Sell, OrderType::Market) => Some(observation.bid),
        (OrderSide::Buy, OrderType::Limit)
            if observation.ask.units() <= order.limit_price?.units() =>
        {
            order.limit_price
        }
        (OrderSide::Sell, OrderType::Limit)
            if observation.bid.units() >= order.limit_price?.units() =>
        {
            order.limit_price
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
            if (order.triggered_unix_nanos.is_some()
                || observation.ask.units() >= order.stop_price?.units())
                && observation.ask.units() <= order.limit_price?.units() =>
        {
            if order.triggered_unix_nanos.is_some() {
                order.limit_price
            } else {
                Some(observation.ask)
            }
        }
        (OrderSide::Sell, OrderType::StopLimit)
            if (order.triggered_unix_nanos.is_some()
                || observation.bid.units() <= order.stop_price?.units())
                && observation.bid.units() >= order.limit_price?.units() =>
        {
            if order.triggered_unix_nanos.is_some() {
                order.limit_price
            } else {
                Some(observation.bid)
            }
        }
        _ => None,
    }
}

pub(crate) fn stop_limit_crossed(order: &Order, observation: &SimulatedMarketObservation) -> bool {
    order.order_type == OrderType::StopLimit
        && order.triggered_unix_nanos.is_none()
        && match order.side {
            OrderSide::Buy => order
                .stop_price
                .is_some_and(|stop| observation.ask.units() >= stop.units()),
            OrderSide::Sell => order
                .stop_price
                .is_some_and(|stop| observation.bid.units() <= stop.units()),
        }
}

pub(crate) fn is_simulated(
    accounts: &BTreeMap<TradingAccountId, TradingAccount>,
    account_id: &TradingAccountId,
) -> bool {
    accounts
        .get(account_id)
        .is_some_and(|account| account.environment == AccountEnvironment::Simulated)
}

/// Orders the touch simulator may fill: executable simulated-account orders on the
/// observed instrument. Broker orders are filled only by their broker.
pub(crate) fn market_fill_candidates(
    orders: &OrderBook,
    accounts: &BTreeMap<TradingAccountId, TradingAccount>,
    observation: &SimulatedMarketObservation,
) -> Vec<(OrderId, FixedPoint)> {
    let mut candidates = orders
        .crossing_for(
            &observation.instrument_id,
            observation.bid.units(),
            observation.ask.units(),
        )
        .into_iter()
        .filter(|order| is_simulated(accounts, &order.account_id))
        .filter_map(|order| touch_price(order, observation).map(|price| (order.id.clone(), price)))
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_id, _), (right_id, _)| {
        let left = &orders[left_id];
        let right = &orders[right_id];
        let priority = |order: &Order| match order.order_type {
            OrderType::Market => (0, 0),
            OrderType::Stop | OrderType::StopLimit if order.triggered_unix_nanos.is_none() => {
                let price = order.stop_price.map_or(0, FixedPoint::units);
                (
                    1,
                    if order.side == OrderSide::Buy {
                        price
                    } else {
                        -price
                    },
                )
            }
            _ => {
                let price = order.limit_price.map_or(0, FixedPoint::units);
                (
                    2,
                    if order.side == OrderSide::Buy {
                        -price
                    } else {
                        price
                    },
                )
            }
        };
        priority(left)
            .cmp(&priority(right))
            .then_with(|| left.accepted_unix_nanos.cmp(&right.accepted_unix_nanos))
            .then_with(|| {
                let sequence = |order: &Order| {
                    order
                        .id
                        .as_str()
                        .strip_prefix("sim-order-")
                        .and_then(|number| number.parse::<u64>().ok())
                };
                sequence(left).cmp(&sequence(right))
            })
            .then_with(|| left_id.cmp(right_id))
    });
    candidates
}

pub(crate) fn next_position(
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
            Some(previous_average) => Some(weighted_average_entry_price(
                previous_average,
                previous_quantity.unsigned_abs(),
                fill.price,
                signed_fill.unsigned_abs(),
            )?),
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
        let closed_quantity = FixedPoint::try_new(
            i64::try_from(closed_quantity)
                .map_err(|_| "realized PnL quantity overflowed".to_string())?,
            fill.quantity.scale(),
        )
        .map_err(|error| error.to_string())?;
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

pub(crate) fn weighted_average_entry_price(
    previous_average: FixedPoint,
    previous_quantity_units: u64,
    fill_price: FixedPoint,
    fill_quantity_units: u64,
) -> Result<FixedPoint, String> {
    let previous_average = previous_average
        .exact_rescale(fill_price.scale())
        .map_err(|error| format!("position average scale is invalid: {error}"))?;
    let previous_weight = i128::from(previous_average.units())
        .checked_mul(i128::from(previous_quantity_units))
        .ok_or_else(|| "position cost overflowed".to_string())?;
    let fill_weight = i128::from(fill_price.units())
        .checked_mul(i128::from(fill_quantity_units))
        .ok_or_else(|| "position cost overflowed".to_string())?;
    let numerator = previous_weight
        .checked_add(fill_weight)
        .ok_or_else(|| "position cost overflowed".to_string())?;
    let denominator = i128::from(
        previous_quantity_units
            .checked_add(fill_quantity_units)
            .ok_or_else(|| "position quantity overflowed".to_string())?,
    );
    if denominator <= 0 {
        return Err("position average denominator is invalid".to_string());
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded = if remainder == 0 {
        quotient
    } else {
        let doubled = remainder
            .checked_abs()
            .and_then(|value| value.checked_mul(2))
            .ok_or_else(|| "position average overflowed".to_string())?;
        if doubled >= denominator {
            quotient
                .checked_add(numerator.signum())
                .ok_or_else(|| "position average overflowed".to_string())?
        } else {
            quotient
        }
    };
    let units = i64::try_from(rounded).map_err(|_| "position average overflowed".to_string())?;
    FixedPoint::try_new(units, fill_price.scale()).map_err(|error| error.to_string())
}

pub(crate) fn realized_pnl(
    entry: FixedPoint,
    exit: FixedPoint,
    position_sign: i64,
    quantity: FixedPoint,
    currency_scale: u8,
    instrument: &TradingInstrument,
) -> Result<FixedPoint, String> {
    let point_value = instrument
        .contract
        .point_value
        .ok_or_else(|| "point value is unavailable for realized PnL".to_string())?;
    let signed_quantity = FixedPoint::try_new(
        quantity
            .units()
            .checked_mul(position_sign)
            .ok_or_else(|| "realized PnL quantity overflowed".to_string())?,
        quantity.scale(),
    )
    .map_err(|error| error.to_string())?;
    project_unrealized_pnl(
        entry,
        exit,
        signed_quantity,
        FixedPoint::try_new(point_value.units(), point_value.scale())
            .map_err(|error| error.to_string())?,
        currency_scale,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn negative_loss(value: FixedPoint) -> Result<FixedPoint, String> {
    let units = if value.units() < 0 {
        i64::try_from(value.units().unsigned_abs())
            .map_err(|_| "risk loss projection overflowed".to_string())?
    } else {
        0
    };
    FixedPoint::try_new(units, value.scale()).map_err(|error| error.to_string())
}

pub(crate) fn remaining_limit(limit: FixedPoint, used: FixedPoint) -> Result<FixedPoint, String> {
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

pub(crate) fn subtract_fixed(left: FixedPoint, right: FixedPoint) -> Result<FixedPoint, String> {
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

pub(crate) fn nonnegative_difference(
    left: FixedPoint,
    right: FixedPoint,
) -> Result<FixedPoint, String> {
    let difference = subtract_fixed(left, right)?;
    if difference.units() < 0 {
        FixedPoint::try_new(0, difference.scale()).map_err(|error| error.to_string())
    } else {
        Ok(difference)
    }
}

pub(crate) fn consistency_projection(
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

pub(crate) fn tick_value(
    instrument: &TradingInstrument,
    currency_scale: u8,
) -> Result<FixedPoint, String> {
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

pub(crate) fn pnl_ticks(pnl: FixedPoint, tick_value: FixedPoint) -> Option<FixedPoint> {
    let pnl = pnl.exact_rescale(tick_value.scale()).ok()?;
    let divisor = tick_value.units();
    (divisor != 0 && pnl.units() % divisor == 0)
        .then(|| FixedPoint::try_new(pnl.units() / divisor, 0).ok())
        .flatten()
}

pub(crate) fn scale_currency_by_quantity_and_ticks(
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

pub(crate) fn multiply_quantity(
    quantity: FixedPoint,
    multiplier: FixedPoint,
) -> Result<FixedPoint, String> {
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

/// A bracket whose entry filled, with its child orders prepared but not yet stored.
pub(crate) struct ArmedBracket {
    bracket: ManagedBracket,
    children: Vec<(Order, OrderEvent)>,
    next_sequence: u64,
}

/// The smallest orderable quantity step of an instrument, in quantity-scale units.
pub(crate) fn order_increment_units(instrument: &TradingInstrument) -> i64 {
    instrument
        .contract
        .order_quantity_increment
        .map_or(1, aeris_instruments::InstrumentDecimal::units)
}

/// Splits a bracket quantity across its targets in whole order increments, paired with each
/// target's template index. Each target receives its percentage rounded down; the remainder goes
/// to the furthest target, and a target whose share rounds to nothing is omitted.
pub(crate) fn allocated_target_quantities(
    quantity: FixedPoint,
    targets: &[BracketTarget],
    increment_units: i64,
) -> Result<Vec<(usize, FixedPoint)>, String> {
    if increment_units <= 0 || quantity.units() <= 0 || quantity.units() % increment_units != 0 {
        return Err("bracket quantity must be a positive whole number of order units".to_string());
    }
    let last = targets
        .len()
        .checked_sub(1)
        .ok_or_else(|| "bracket requires at least one target".to_string())?;
    let increments = i128::from(quantity.units() / increment_units);
    let mut allocated = 0_i128;
    let mut quantities = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let share = if index == last {
            increments - allocated
        } else {
            increments * i128::from(target.quantity_percent) / 100
        };
        allocated += share;
        if share == 0 {
            continue;
        }
        let units = i64::try_from(share * i128::from(increment_units))
            .map_err(|_| "bracket target quantity overflowed".to_string())?;
        quantities.push((
            index,
            FixedPoint::try_new(units, quantity.scale()).map_err(|error| error.to_string())?,
        ));
    }
    Ok(quantities)
}

pub(crate) fn instrument_tick_units(
    instrument: &TradingInstrument,
    at: FixedPoint,
) -> Result<i64, String> {
    if at.scale() != instrument.price_scale || at.units() <= 0 {
        return Err("bracket price must be positive at the instrument scale".to_string());
    }
    if let aeris_instruments::PriceGrid::SignificantFigures {
        figures,
        max_decimals,
    } = instrument.contract.price_grid
    {
        // The number of significant figures defines a local increment, not a
        // constant tick. Calculate it at the entry price without floating point.
        let digits = i32::try_from(at.units().ilog10() + 1)
            .map_err(|_| "bracket price has too many digits")?;
        let power = (digits - i32::from(figures))
            .max(i32::from(at.scale()) - i32::from(max_decimals))
            .max(0);
        return 10_i64
            .checked_pow(u32::try_from(power).map_err(|_| "bracket increment overflowed")?)
            .ok_or_else(|| "bracket increment overflowed".to_string());
    }
    let tick = instrument
        .contract
        .tick_size
        .or(match instrument.contract.price_grid {
            aeris_instruments::PriceGrid::Fixed(tick) => Some(tick),
            _ => None,
        })
        .ok_or_else(|| "tick size is required for a bracket strategy".to_string())?;
    FixedPoint::try_new(tick.units(), tick.scale())
        .map_err(|error| error.to_string())?
        .exact_rescale(instrument.price_scale)
        .map(FixedPoint::units)
        .map_err(|error| format!("instrument tick size is not exact at its price scale: {error}"))
}

pub(crate) fn shifted_price(
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

pub(crate) fn favorable_tick_distance(
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

pub(crate) fn tighter_stop(
    current: FixedPoint,
    candidate: FixedPoint,
    entry_side: OrderSide,
) -> FixedPoint {
    if (entry_side == OrderSide::Buy && current.units() >= candidate.units())
        || (entry_side == OrderSide::Sell && current.units() <= candidate.units())
    {
        current
    } else {
        candidate
    }
}

pub(crate) const fn opposite_side(side: OrderSide) -> OrderSide {
    match side {
        OrderSide::Buy => OrderSide::Sell,
        OrderSide::Sell => OrderSide::Buy,
    }
}

pub(crate) fn managed_child_client_order_id(
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

pub(crate) fn mirrored_client_order_id(
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
