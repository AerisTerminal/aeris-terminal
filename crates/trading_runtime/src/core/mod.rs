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
pub use groups::model::{
    GroupKind, GroupLeg, GroupStatus, GroupValidationError, LegRole, OrderGroup, StopManagement,
};
#[path = "../ledger/mod.rs"]
pub(crate) mod ledger;
#[path = "../market/mod.rs"]
pub(crate) mod market;
#[path = "../publication.rs"]
pub mod publication;
#[path = "../risk/owner.rs"]
mod risk_owner;
#[cfg(test)]
pub(crate) use groups::chart_bracket_template;
#[path = "../venue/broker.rs"]
mod broker;
#[path = "../venue/scales.rs"]
mod scales;

use self::ledger::{CostBasis, FillAccounting, TradeCycle};
use self::market::feed::{MarketFeedInbox, QuoteBook};
pub use self::market::gating::ExecutionPolicy;
use crate::book::OrderBook;
pub use crate::copier::{
    MAXIMUM_COPIER_TARGETS, TradeCopierConfig, TradeCopierTarget, TradeCopyDispatch,
};
pub use crate::discipline::{DisciplineState, FAST_STOP_REENTRY_NANOS};
pub use crate::plan::{
    MAXIMUM_ALLOWED_SETUPS, MAXIMUM_CHECKLIST_ITEMS, MAXIMUM_PLAN_LEVELS, SessionAdherenceReview,
    SessionBias, SessionChecklistItem, SessionPlan, SessionPlanLevel,
};
pub use crate::risk::session::{RiskSession, RiskSessionBoundary};
pub use crate::risk::{
    EconomicEventRiskAction, EconomicEventRiskImportance, EconomicEventRiskOutcome,
    EconomicEventRiskRule, EconomicEventRiskTrigger, RiskEvaluation, RiskLock, RiskLockKind,
    RiskMeter, RiskProfile, RiskRuleState, TrailingDrawdownMode,
};
pub use crate::round_trip::{TradeLeg, TradeRoundTrip};
use crate::service::Command;
use crate::store::{OrderStep, StoredState, TradingStore};
pub use crate::store::{SCHEMA_VERSION, StoreError};
pub use crate::strategy::{
    BracketStrategyTemplate, BracketTarget, BracketView, BracketViewStatus, BreakEvenRule,
    MAXIMUM_BRACKET_TARGETS, MAXIMUM_MANAGED_BRACKETS, MAXIMUM_STRATEGY_TEMPLATES, ProtectionRole,
    ProtectionView, TrailingStopRule,
};
use crate::venue::VenueInbox;
use aeris_instruments::{ContractMetadata, InstrumentId};
use aeris_trading::market::{MarketRoute, QuoteInterestSink};
use aeris_trading::venue::{BROKER_VENUE_ID, BracketSide, VenueCapabilities};
use aeris_trading::venue_outbox::{VenueRequestReceiver, VenueRequestSender};
use aeris_trading::{
    AccountEnvironment, AccountPnl, ClientOrderId, EquitySample, FeeSchedule, Fill, FillId,
    FixedPoint, LedgerEntry, LedgerEntryKind, Liquidity, Order, OrderEvent, OrderEventId,
    OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position, RiskWarning, TimeInForce,
    TradingAccount, TradingAccountId, TradingProvenance, project_unrealized_pnl,
};
pub use groups::model::{MovePlannedLeg, PlannedLeg};
pub use publication::{
    MARK_PUBLICATION_INTERVAL, MAXIMUM_SUBSCRIPTIONS, OrderFeedStatus, SUBSCRIPTION_QUEUE_CAPACITY,
    SubscriptionUpdate, TradingEvent, TradingEventCause, TradingSubscription,
    WORKING_SET_FILL_LIMIT, WorkingSet,
};
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
    "broker live accounts are data-only; trading is disabled";
pub(crate) const DEMO_VENUE_UNAVAILABLE: &str = "broker demo venue is not connected";
pub(crate) const VENUE_UNSUPPORTED: &str = "trading account venue is not supported";
pub(crate) const BROKER_PNL_UNAVAILABLE: &str =
    "the broker account is unreachable, so its open P&L for the loss rules is unknown";
/// The equity timer cadence; the owner wakes on it alongside the other timer deadlines.
pub(crate) const EQUITY_SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const RETENTION_INTERVAL: Duration = Duration::from_secs(3_600);
pub(crate) type Reply<T> = SyncSender<Result<T, String>>;

/// The owner derives the venue from the registered account, never from order provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VenueRoute {
    Simulated,
    BrokerDemo,
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
            (BROKER_VENUE_ID, AccountEnvironment::Demo) => Ok(Self::BrokerDemo),
            (BROKER_VENUE_ID, AccountEnvironment::Live) => Err(LIVE_TRADING_DISABLED.into()),
            // Accounts recorded by a venue this build no longer routes stay readable but
            // can never trade.
            (_, AccountEnvironment::Demo | AccountEnvironment::Live) => {
                Err(VENUE_UNSUPPORTED.into())
            }
            _ => Err("trading account route is invalid".into()),
        }
    }
}

/// How a bracket was placed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlacementOutcome {
    /// Simulated accounts: the owner manages the stop and target child orders locally.
    Managed(BracketView),
    /// Broker accounts: the entry order carries a server-held bracket, whose target and stop
    /// levels the venue releases and enforces as the entry fills.
    BrokerProtected(Order),
}

impl PlacementOutcome {
    #[must_use]
    pub const fn entry_client_order_id(&self) -> &ClientOrderId {
        match self {
            Self::Managed(bracket) => &bracket.entry_client_order_id,
            Self::BrokerProtected(order) => &order.client_order_id,
        }
    }
}

/// Simulated closes fill immediately; broker exits are asynchronous requests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FlattenOutcome {
    pub fills: Vec<Fill>,
    /// Broker positions whose exit was requested; each stays open until the venue reports
    /// the fills.
    pub pending_exits: Vec<PendingExit>,
    /// What could not be requested (for example with a broker venue disconnected); every
    /// other order and position in scope was still handled.
    pub incomplete: Option<String>,
    /// Positions kept because the owner holds no current verified quote for their
    /// instrument. They are reported, never silently skipped, and keep their protection.
    pub unpriced: Vec<UnpricedPosition>,
}

/// Which accounts and instruments a flatten command touches: `None` selects every account
/// or every instrument with an open position.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FlattenScope {
    pub account: Option<TradingAccountId>,
    pub instrument: Option<InstrumentId>,
}

/// A broker position whose exit a flatten, reverse or the kill switch requested from the venue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingExit {
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub net_quantity: FixedPoint,
}

/// A position a flatten or the kill switch kept because no current verified quote could
/// price it; its protection stays in place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnpricedPosition {
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub net_quantity: FixedPoint,
}

/// What one kill-switch invocation did: the accounts it locked, the non-protective
/// orders it cancelled, the closing fills it committed and what it could not finish.
/// Locks, cancellations and simulated fills commit in one durable step.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmergencyOutcome {
    /// Accounts locked by this invocation.
    pub accounts_locked: usize,
    /// Non-protective working orders cancelled with the kill-switch cause. Protection
    /// legs are never in this list: they retire with a flatten fill or keep working.
    pub cancelled: Vec<Order>,
    /// Simulated closing fills committed in the same step.
    pub fills: Vec<Fill>,
    /// Broker positions whose exit was requested; each stays open until the venue reports
    /// the fills.
    pub pending_exits: Vec<PendingExit>,
    /// Positions kept because the owner holds no current verified quote for their
    /// instrument; they are reported, never silently skipped, and keep their protection.
    pub unpriced: Vec<UnpricedPosition>,
    /// What could not be requested (for example with a broker venue disconnected); the
    /// locks and every other order and position in scope were still handled.
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

/// Entry command for one explicit one-cancels-the-other pair of simulated orders; both
/// members commit with their group in a single order step, and a member's full fill cancels
/// its siblings inside the quote step that filled it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceOco {
    pub first: PlaceOrder,
    pub second: PlaceOrder,
}

/// One standalone reduce-only protection command from a chart or order surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceProtection {
    pub order: PlaceOrder,
    pub role: ProtectionRole,
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

/// Moves one level of a bracket the broker venue holds for a broker entry, from its current
/// distance in ticks from the entry fill to a new one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveBracketLevel {
    /// The bracketed entry order.
    pub client_order_id: ClientOrderId,
    pub side: BracketSide,
    pub current_ticks: i32,
    pub new_ticks: i32,
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

/// A deterministic startup repair the user must see. It is raised once by the start that
/// performs the repair; M5.11's typed event stream will carry it to subscribers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryNotice {
    pub kind: RecoveryNoticeKind,
    /// The legacy bracket or group the notice is about.
    pub subject: String,
    pub detail: String,
}

/// The repair a recovery notice reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryNoticeKind {
    /// A pre-group awaiting bracket could not be priced from its entry after the upgrade; its
    /// group is cancelled and its entry keeps working unmanaged.
    UnpriceableBracket,
    /// A recovered group violated its invariants at startup; it was cancelled with its
    /// remaining open legs.
    BrokenGroup,
    /// A group member filled before the last shutdown; the group settled at startup.
    InterruptedGroupFill,
    /// Recovered protection no longer matched its position; startup alignment realigned it.
    CoverageRepair,
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
    /// partial or final, plus each open trade, of simulated and broker accounts alike.
    pub round_trips: Vec<TradeRoundTrip>,
    pub positions: Vec<Position>,
    /// The last balance each broker account reported, in its currency.
    pub broker_balances: BTreeMap<TradingAccountId, FixedPoint>,
    /// Broker accounts the attached venue can reach now.
    pub connected_broker_accounts: BTreeSet<TradingAccountId>,
    pub position_pnl: Vec<PositionPnl>,
    pub account_pnl: Vec<AccountPnl>,
    /// The durable cash-ledger entries of every simulated account, in commit order.
    pub ledger_entries: Vec<LedgerEntry>,
    /// The recent equity samples of each simulated account, oldest first per account.
    pub equity_samples: Vec<EquitySample>,
    /// The simulated fee schedule of each account.
    pub fee_schedules: Vec<(TradingAccountId, FeeSchedule)>,
    pub risk_profiles: Vec<RiskProfile>,
    pub risk_meters: Vec<RiskMeter>,
    pub risk_locks: Vec<RiskLock>,
    pub risk_rule_states: Vec<RiskRuleState>,
    /// The durable risk session of each profiled account, rolled at its session boundary.
    pub risk_sessions: Vec<RiskSession>,
    pub discipline_states: Vec<DisciplineState>,
    pub session_plans: Vec<SessionPlan>,
    pub session_adherence_reviews: Vec<SessionAdherenceReview>,
    pub trade_copiers: Vec<TradeCopierConfig>,
    pub copy_dispatches: Vec<TradeCopyDispatch>,
    pub strategy_templates: Vec<BracketStrategyTemplate>,
    pub managed_brackets: Vec<BracketView>,
    /// The stop and targets each awaiting bracket places once its working entry fills.
    pub planned_legs: Vec<PlannedLeg>,
    pub protective_orders: Vec<ProtectionView>,
    /// Startup repairs this process performed, raised once at the start that repaired them.
    pub recovery_notices: Vec<RecoveryNotice>,
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
    pub fn protective_role(&self, client_order_id: &ClientOrderId) -> Option<ProtectionRole> {
        self.managed_brackets
            .iter()
            .find_map(|bracket| groups::managed_bracket_role(bracket, client_order_id))
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
            let Some((key, price)) = groups::protection_trigger(order, role) else {
                continue;
            };
            let slot = match role {
                ProtectionRole::StopLoss => &mut stop_loss,
                ProtectionRole::TakeProfit => &mut take_profit,
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
    /// The paged order-history reader on its own read-only connection; `None` for the
    /// in-memory store of tests, which has no second connection.
    history_reader: Mutex<Option<crate::history::HistoryReader>>,
}

pub(crate) struct TradingCore {
    pub(crate) store: TradingStore,
    state: StoredState,
    retention: TradingRetention,
    pub(crate) clock: Arc<dyn clock::Clock>,
    command_now: i64,
    next_retention_at: i64,
    next_equity_sample_at: i64,
    /// Fill samples waiting for their committing transaction; cleared on every plan outcome.
    pending_equity_samples: Vec<EquitySample>,
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
    /// What the attached venue declared it supports.
    venue_capabilities: Option<VenueCapabilities>,
    venue_inbox: Weak<VenueInbox>,
    venue_ingestion: u64,
    /// Broker accounts the current venue generation has announced; the rest are not
    /// reachable now.
    connected_broker_accounts: BTreeSet<TradingAccountId>,
    /// Startup repairs (legacy awaiting-bracket conversion, group verification) published to
    /// every subscriber's initial working set until M6.10 consumes them as typed events.
    recovery_notices: Vec<RecoveryNotice>,
    /// Bounded change-driven subscribers of the working set (M5.11).
    subscribers: BTreeMap<u64, Arc<publication::SubscriberSlot>>,
    next_subscriber_id: u64,
    /// In-memory stamp bumped by quote-only mark changes; revision-changing steps subsume it.
    mark_revision: u64,
    publication: publication::PublicationState,
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
    cost_basis: Option<crate::core::ledger::CostBasis>,
    position_key: (TradingAccountId, InstrumentId),
    policy_update: FillPolicyUpdate,
    next_sequence: u64,
}

pub(crate) struct FillPolicyUpdate {
    rule_state: Option<RiskRuleState>,
    trade_cycle: TradeCycle,
    discipline_state: DisciplineState,
    accounting: FillAccounting,
    /// The cash-ledger entries this fill commits, realized P&L before commission.
    ledger_entries: Vec<LedgerEntry>,
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
        let history_path = crate::history::history_database_path(&database_path);
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
        // The history reader serves paged queries on its own read-only connection, off the
        // owner thread. It opens after the owner has created/migrated the database file; a
        // failure fails the service start (the owner handle drops and stops).
        let history_reader = history_path
            .map(|path| crate::history::HistoryReader::start(&path))
            .transpose()
            .map_err(StoreError::Other)?;
        Ok(Self {
            commands,
            runtime: Arc::new(TradingRuntime {
                stopping,
                worker: Mutex::new(Some(worker)),
                market_feed,
                venue_inbox,
                history_reader: Mutex::new(history_reader),
            }),
        })
    }

    /// Subscribes to change-driven working-set publications; at most
    /// [`MAXIMUM_SUBSCRIPTIONS`] are open at once. The first received event is the complete
    /// current working set.
    ///
    /// # Errors
    /// Returns an error when the subscription limit is reached or the owner is unavailable.
    pub fn subscribe(&self) -> Result<TradingSubscription, String> {
        let (id, slot) = self.request(Command::Subscribe)?;
        Ok(TradingSubscription {
            id,
            slot,
            commands: self.commands.clone(),
            stopping: Arc::clone(&self.runtime.stopping),
        })
    }

    /// Reads one page of durable order history through the read-only reader thread. Pages hold
    /// at most [`crate::MAXIMUM_HISTORY_PAGE`] rows.
    ///
    /// # Errors
    /// Returns an error when the store is in-memory (no reader), the reader queue is full, or
    /// the read fails.
    pub fn history(&self, query: crate::HistoryQuery) -> Result<crate::HistoryPage, String> {
        let reader = self
            .runtime
            .history_reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reader
            .as_ref()
            .ok_or_else(|| "order history requires a file-backed store".to_string())?
            .query(query)
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

    /// Installs the simulated fee schedule of one practice account.
    ///
    /// # Errors
    /// Returns an error for an unknown or broker account, an invalid schedule, an
    /// unregistered per-instrument override, owner overload, or a storage failure.
    pub fn set_fee_schedule(
        &self,
        account_id: TradingAccountId,
        schedule: FeeSchedule,
    ) -> Result<(), String> {
        self.request(|reply| Command::SetFeeSchedule(account_id, schedule, reply))
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
    ) -> Result<PlacementOutcome, String> {
        self.request(|reply| Command::PlaceBracketFromMarket(bracket, reply))
    }

    /// Submits an inline bracket stamped with the owner's quote. The entry keeps its own order
    /// type and explicit prices; a market entry carries none.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or rejected bracket.
    pub fn place_inline_bracket_from_market(
        &self,
        bracket: PlaceInlineBracket,
    ) -> Result<PlacementOutcome, String> {
        self.request(|reply| Command::PlaceInlineBracketFromMarket(bracket, reply))
    }
    /// Places chart-authored bracket levels using verified instrument terms and the owner quote.
    ///
    /// # Errors
    /// Returns an error for unavailable terms or quote, off-grid levels, or rejected admission.
    pub fn place_chart_bracket_from_market(
        &self,
        bracket: PlaceChartBracket,
    ) -> Result<PlacementOutcome, String> {
        self.request(|reply| Command::PlaceChartBracketFromMarket(bracket, reply))
    }

    /// Submits a protective order using the owner's quote.
    ///
    /// # Errors
    /// Returns an error for an unavailable quote or invalid protection.
    pub fn place_protective_order_from_market(
        &self,
        order: PlaceProtection,
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
    /// `capabilities` is what the relay's venue supports.
    ///
    /// # Errors
    /// Returns an error for invalid capabilities, when the owner is unavailable or when the
    /// generation cannot be stored.
    pub fn attach_demo_venue(
        &self,
        capabilities: VenueCapabilities,
    ) -> Result<(u64, VenueRequestReceiver), String> {
        self.request(|reply| Command::AttachVenue(capabilities, reply))
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
    pub fn place_bracket(&self, bracket: PlaceBracket) -> Result<PlacementOutcome, String> {
        self.request(|reply| Command::PlaceBracket(bracket, reply))
    }

    /// Submits an explicit one-cancels-the-other pair on the simulated venue, both members
    /// resting until one fills and cancels the other.
    ///
    /// # Errors
    /// Returns an error when the members differ in scope, admission rejects either member,
    /// capacity is exhausted, or persistence fails.
    pub fn place_oco(&self, pair: PlaceOco) -> Result<[Order; 2], String> {
        self.request(|reply| Command::PlaceOco(pair, reply))
    }

    /// Submits a chart-authored bracket with exact, inline tick offsets.
    ///
    /// # Errors
    /// Returns an error when the entry, template, risk, or persistence is invalid.
    pub fn place_inline_bracket(
        &self,
        bracket: PlaceInlineBracket,
    ) -> Result<PlacementOutcome, String> {
        self.request(|reply| Command::PlaceInlineBracket(bracket, reply))
    }

    /// Submits one reduce-only protective stop or target for an existing position.
    ///
    /// # Errors
    /// Returns an error unless the order strictly reduces a canonical open position.
    pub fn place_protective_order(&self, order: PlaceProtection) -> Result<Order, String> {
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
    pub fn move_planned_leg(&self, command: MovePlannedLeg) -> Result<PlannedLeg, String> {
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

    /// Asks the broker venue to move one level of a bracket it holds for a broker entry. The
    /// level keeps its distance until the venue confirms the move. Widening a stop is
    /// risk-checked like a new bracket.
    ///
    /// # Errors
    /// Returns an error for an unknown entry, a non-broker or live account, a bracket that does
    /// not hold the level, an unacknowledged entry, a risk refusal, or an unattached demo venue.
    pub fn move_bracket_level(&self, command: MoveBracketLevel) -> Result<(), String> {
        self.request(|reply| Command::MoveBracketLevel(command, reply))
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

    /// Configures the boundary at which the account's risk session rolls over. The new
    /// boundary applies from the next rollover; the running session keeps its baseline.
    ///
    /// # Errors
    /// Returns an error when the account has no risk session or storage fails.
    pub fn set_risk_session_boundary(
        &self,
        account_id: TradingAccountId,
        boundary: RiskSessionBoundary,
    ) -> Result<(), String> {
        self.request(|reply| Command::SetRiskSessionBoundary(account_id, boundary, reply))
    }

    /// Locks one account or every tradable account, cancels its non-protective working
    /// orders and flattens every priced position at the owner's verified quotes in one
    /// durable step per invocation. Protection of flattened positions retires with the
    /// fill; positions without a verified quote keep their protection and are reported
    /// in `EmergencyOutcome::unpriced`. Broker cancels and closes are venue requests
    /// reported through the outcome.
    ///
    /// # Errors
    /// Returns an error when no target account exists, a targeted account is data-only,
    /// or the durable step cannot be committed.
    pub fn kill_switch(
        &self,
        account_id: Option<TradingAccountId>,
        reason: String,
        locked_at_unix_nanos: i64,
    ) -> Result<EmergencyOutcome, String> {
        self.request(|reply| Command::KillSwitch(account_id, reason, locked_at_unix_nanos, reply))
    }

    /// Flattens every in-scope simulated position at the owner's own verified quotes and
    /// requests broker closes for every in-scope instrument. Positions without a current
    /// verified quote are reported in `unpriced` and keep their protection; cancelling the
    /// scope's working orders and closing the priced positions commits as one durable step.
    ///
    /// # Errors
    /// Returns an error when a selected account is unknown, no account is registered, or
    /// persistence fails. This administrative action remains available while risk-locked.
    pub fn flatten(&self, scope: FlattenScope) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::Flatten(scope, reply))
    }

    /// Reverses the selected instrument using only the owner's validated quote. The closing
    /// and opening fills commit in one step; a risk rejection leaves the position untouched.
    ///
    /// # Errors
    /// Returns an error when the position is flat, the quote is unavailable, the replacement
    /// side violates account risk, the account is locked, or persistence fails.
    pub fn reverse_position_at(
        &self,
        account: TradingAccountId,
        instrument: InstrumentId,
    ) -> Result<FlattenOutcome, String> {
        self.request(|reply| Command::ReverseAt(account, instrument, reply))
    }

    /// Applies every enabled account event rule once, durably fenced by event/profile
    /// identity. Flatten rules price positions from the owner's own verified quotes and need
    /// no caller-supplied observation.
    ///
    /// # Errors
    /// Returns an error when the trigger is invalid, the runtime is overloaded, or durable
    /// rule state cannot be written.
    pub fn apply_economic_event_risk(
        &self,
        event: EconomicEventRiskTrigger,
    ) -> Result<EconomicEventRiskOutcome, String> {
        self.request(|reply| Command::ApplyEconomicEventRisk(event, reply))
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
        self.shutdown_with_deadline(Some(timeout))
    }

    /// Fixture shutdown waits for the owner to finish its queued checkpoint, independent of
    /// how long the parallel test scheduler leaves the owner thread unscheduled.
    #[cfg(test)]
    pub(crate) fn shutdown_after_owner_exit_for_test(&self) -> Result<(), String> {
        self.shutdown_with_deadline(None)
    }

    #[cfg(test)]
    pub(crate) fn join_expired_shutdown_for_test(&self) {
        self.runtime
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("owner thread")
            .join()
            .expect("owner checkpoints after release");
    }

    fn shutdown_with_deadline(&self, timeout: Option<Duration>) -> Result<(), String> {
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
            if finished || timeout.is_none() {
                let worker = self
                    .runtime
                    .worker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if worker.is_some_and(|worker| worker.join().is_err()) {
                    return Err("trading owner panicked during shutdown".to_string());
                }
                let result = reply_rx
                    .recv()
                    .map_err(|_| "trading owner stopped before checkpointing".to_string())?;
                // The owner has stopped; the history reader's thread stops with it.
                self.runtime
                    .history_reader
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                return result;
            }
            if let Some(timeout) = timeout {
                let elapsed = clock::Clock::monotonic(&shutdown_clock);
                if elapsed >= timeout {
                    return Err("trading owner shutdown deadline expired".to_string());
                }
                thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(elapsed)));
            }
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
        let store = TradingStore::open(database_path, retention)?;
        Self::open_with_store(store, retention, clock, execution, venue_inbox, market_feed)
    }

    /// Opens a core on an already migrated store. Tests arm commit failure between the store
    /// open and this load to prove the startup conversion stays atomic and retryable.
    pub(crate) fn open_with_store(
        mut store: TradingStore,
        retention: TradingRetention,
        clock: Arc<dyn clock::Clock>,
        execution: ExecutionPolicy,
        venue_inbox: Weak<VenueInbox>,
        market_feed: Arc<MarketFeedInbox>,
    ) -> Result<Self, StoreError> {
        // Awaiting legacy brackets load before the state so their conversion can run at this
        // start; the legacy tables drop once every row converted.
        let legacy_awaiting = store
            .load_legacy_awaiting_brackets()
            .map_err(StoreError::Other)?;
        let state = store.load_state().map_err(StoreError::Other)?;
        let interest_revision = store.interest_revision().map_err(StoreError::Other)?;
        let recovery_quote_floor = clock.wall_unix_nanos();
        let mut core = Self {
            store,
            state,
            retention,
            command_now: clock.wall_unix_nanos(),
            // Retention runs at startup and then hourly; the owner timer drives both.
            next_retention_at: clock.wall_unix_nanos(),
            // The first equity tick fires one interval after open so a restart replays the
            // durable history unchanged before sampling resumes.
            next_equity_sample_at: clock
                .wall_unix_nanos()
                .saturating_add(ledger::EQUITY_SAMPLE_INTERVAL_NANOS),
            pending_equity_samples: Vec::new(),
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
            interest_revision,
            observation_cursors: BTreeMap::new(),
            recovery_quote_floor,
            execution,
            venue_outbound: None,
            venue_capabilities: None,
            venue_inbox,
            venue_ingestion: 0,
            connected_broker_accounts: BTreeSet::new(),
            recovery_notices: Vec::new(),
            subscribers: BTreeMap::new(),
            next_subscriber_id: 0,
            mark_revision: 0,
            publication: publication::PublicationState::default(),
            #[cfg(test)]
            timer_calls: 0,
        };
        core.repair_loaded_position_projections()
            .map_err(StoreError::Other)?;
        // Recovered groups are verified and their protection coverage repaired before any
        // quote or command arrives.
        core.verify_recovered_groups().map_err(StoreError::Other)?;
        let legacy_ids = legacy_awaiting
            .iter()
            .map(|bracket| bracket.bracket_id.clone())
            .collect::<Vec<_>>();
        core.convert_awaiting_brackets(legacy_awaiting)
            .map_err(StoreError::Other)?;
        let converted = legacy_ids
            .into_iter()
            .filter(|bracket_id| core.state.order_groups.contains_key(bracket_id))
            .collect::<Vec<_>>();
        core.store
            .delete_legacy_brackets(&converted)
            .map_err(StoreError::Other)?;
        core.store
            .complete_post_group_migrations()
            .map_err(StoreError::Other)?;
        core.recover_day_deadlines().map_err(StoreError::Other)?;
        // Expire durable DAY deadlines before the service acknowledges startup
        // or accepts a quote. A failed transaction leaves recovery retryable.
        core.expire_due_day_orders().map_err(StoreError::Other)?;
        Ok(core)
    }

    /// Runs the bounded retention pass, folding pruned fills and ledger entries into their
    /// checkpoints through the position math this core owns. The store keeps the checkpoint
    /// writes and deletions in one transaction.
    fn enforce_retention(&mut self) -> Result<(), String> {
        let state = &self.state;
        self.store
            .enforce_retention(self.command_now, &mut |key, base, fills| {
                fold_position_checkpoint(state, key, base, fills)
            })
    }

    fn repair_loaded_position_projections(&mut self) -> Result<(), String> {
        // A broker position is settled against the venue at every attach, and a rebase moves
        // it away from its fills, so only simulated positions are rebuilt from their fills.
        let keys = self
            .state
            .positions
            .keys()
            .filter(|(account_id, _)| is_simulated(&self.state.accounts, account_id))
            .cloned()
            .collect::<Vec<_>>();
        let mut repaired = Vec::new();
        for key in keys {
            let Some(stored) = self.state.positions.get(&key).cloned() else {
                continue;
            };
            let checkpointed = self.state.position_checkpoints.contains_key(&key);
            if !checkpointed && !self.position_projection_needs_repair(&stored) {
                continue;
            }
            let Some((rebuilt, rebuilt_basis)) = self.rebuild_position_from_checkpoint(&key) else {
                continue;
            };
            // Fills are the durable ledger and positions are a projection. A checkpointed
            // position replays exactly from checkpoint plus the retained suffix, so the
            // rebuild is authoritative. Without a checkpoint, only replace a loaded
            // projection when the retained ledger reproduces both the current net quantity
            // and the last-fill boundary; this prevents retention-truncated history from
            // rewriting a still-valid open position while repairing projections produced by
            // older buggy math. The unrealized mark is not part of the projection identity:
            // it re-derives from the next quote.
            let basis_changed = self.state.cost_basis.get(&key).copied() != rebuilt_basis;
            let projection_matches = rebuilt.net_quantity == stored.net_quantity
                && rebuilt.average_entry_price == stored.average_entry_price
                && rebuilt.realized_pnl == stored.realized_pnl
                && rebuilt.last_fill_unix_nanos == stored.last_fill_unix_nanos;
            let replace = if checkpointed {
                !projection_matches || basis_changed
            } else {
                rebuilt.net_quantity == stored.net_quantity
                    && rebuilt.last_fill_unix_nanos == stored.last_fill_unix_nanos
                    && (rebuilt != stored || basis_changed)
            };
            if replace {
                // The unrealized mark is not part of the projection identity: it re-derives
                // from the next quote, so the rebuilt zero mark replaces any stored value.
                repaired.push((key, rebuilt, rebuilt_basis));
            }
        }
        if repaired.is_empty() {
            return Ok(());
        }
        for (key, position, basis) in repaired {
            self.store.update_position(&position, basis)?;
            match basis {
                Some(basis) => self.state.cost_basis.insert(key.clone(), basis),
                None => self.state.cost_basis.remove(&key),
            };
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

    /// Rebuilds one position projection from its checkpoint plus the retained fill suffix.
    /// Before the first retention run there is no checkpoint and every fill is retained, so
    /// this is a full replay; afterwards it starts from the exact folded state.
    fn rebuild_position_from_checkpoint(
        &self,
        key: &(TradingAccountId, InstrumentId),
    ) -> Option<(Position, Option<CostBasis>)> {
        let account = self.state.accounts.get(&key.0)?;
        let instrument = self.state.instruments.get(&key.1)?;
        let checkpoint = self.state.position_checkpoints.get(key);
        let mut rebuilt: Option<Position> = match checkpoint {
            Some(checkpoint) => Some(position_from_checkpoint(checkpoint).ok()?),
            None => None,
        };
        let mut rebuilt_basis: Option<CostBasis> = checkpoint.and_then(|c| c.cost_basis);
        let mut saw_fill = checkpoint.is_some();
        for fill in self
            .state
            .fills
            .iter()
            .filter(|fill| fill.account_id == key.0 && fill.instrument_id == key.1)
        {
            saw_fill = true;
            let (position, basis) =
                next_position(rebuilt.as_ref(), rebuilt_basis, fill, account, instrument).ok()?;
            rebuilt = Some(position);
            rebuilt_basis = basis;
        }
        if !saw_fill {
            return None;
        }
        rebuilt.map(|position| (position, rebuilt_basis))
    }

    /// Applies one command on the single owner. Returns false after a shutdown checkpoint.
    pub(crate) fn handle(&mut self, command: Command, now: i64) -> bool {
        self.command_now = self.command_now.max(now);
        match command {
            Command::Status(reply) => respond(&reply, Ok(self.status())),
            Command::Subscribe(reply) => respond(&reply, self.subscribe()),
            Command::Unsubscribe(id) => self.unsubscribe(id),
            Command::WorkingSet(reply) => respond(&reply, self.current_working_set()),
            Command::RegisterAccount(account, reply) => {
                respond(&reply, self.register_account(account));
            }
            Command::SetFeeSchedule(account_id, schedule, reply) => {
                respond(&reply, self.set_fee_schedule(&account_id, schedule));
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
            | Command::PlaceOco(..)
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
            | Command::MoveBracketLevel(..) => {
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
            | Command::SetRiskSessionBoundary(..)
            | Command::UnlockAccount(..)
            | Command::KillSwitch(..)
            | Command::Flatten(..)
            | Command::ReverseAt(..)
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
            Command::AttachVenue(capabilities, reply) => {
                respond(&reply, self.attach_venue(capabilities));
            }
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

    /// Runs due timer work only at a due deadline, never on an idle command wake.
    pub(crate) fn on_timer(&mut self, now: i64) {
        self.command_now = self.command_now.max(now);
        #[cfg(test)]
        {
            self.timer_calls += 1;
        }
        if let Err(error) = self.expire_due_day_orders() {
            aeris_observability::diagnostic!("DAY order expiry persistence failed: {error}");
        }
        if let Err(error) = self.cancel_due_markets() {
            aeris_observability::diagnostic!("market order timeout persistence failed: {error}");
        }
        if let Err(error) = self.roll_risk_sessions(self.command_now) {
            aeris_observability::diagnostic!("risk session rollover persistence failed: {error}");
        }
        if self.command_now >= self.next_retention_at {
            match self.enforce_retention() {
                Ok(()) => {
                    self.next_retention_at = self.command_now.saturating_add(3_600_000_000_000);
                    self.enforce_memory_retention();
                }
                Err(error) => aeris_observability::diagnostic!("trading retention failed: {error}"),
            }
        }
        if self.command_now >= self.next_equity_sample_at {
            self.next_equity_sample_at = self
                .command_now
                .saturating_add(ledger::EQUITY_SAMPLE_INTERVAL_NANOS);
            self.run_equity_timer(self.command_now);
        }
        if let Err(error) = self.recompute_quote_interest() {
            self.market_observation_error = Some(error.to_string());
        }
    }

    fn dispatch_account_command(&mut self, command: Command) {
        match command {
            Command::LockAccount(account_id, reason, locked_at, reply) => {
                respond(
                    &reply,
                    self.lock_account(account_id, RiskLockKind::Manual, reason, locked_at),
                );
            }
            Command::UnlockAccount(account_id, reply) => {
                respond(&reply, self.unlock_account(&account_id));
            }
            Command::SetRiskSessionBoundary(account_id, boundary, reply) => {
                respond(
                    &reply,
                    self.set_risk_session_boundary(&account_id, boundary),
                );
            }
            Command::KillSwitch(account_id, reason, locked_at, reply) => {
                respond(
                    &reply,
                    self.kill_switch(account_id.as_ref(), &reason, locked_at),
                );
            }
            Command::Flatten(scope, reply) => {
                respond(&reply, self.flatten_scope(&scope));
            }
            Command::ReverseAt(account, instrument, reply) => {
                respond(&reply, self.reverse_position(&account, &instrument));
            }
            Command::ApplyEconomicEventRisk(event, reply) => {
                respond(&reply, self.apply_economic_event_risk(&event));
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

    fn place_protective_from_market(&mut self, order: PlaceProtection) -> Result<Order, String> {
        self.market_order(order.order, false)
            .and_then(|order_command| {
                self.place_protective_order(PlaceProtection {
                    order: order_command,
                    role: order.role,
                })
            })
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
            Command::PlaceOco(pair, reply) => respond(&reply, self.place_owned_oco(&pair)),
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
                self.place_bracket_with_template(&bracket.entry, &bracket.template),
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
                respond(&reply, self.place_protective_from_market(order));
            }
            Command::EvaluateRisk(order, reply) => {
                let attached_stop = order.stop_price.is_some();
                respond(&reply, self.evaluate_order_risk(&order, attached_stop));
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
                respond(&reply, self.move_planned_leg(&command));
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
            Command::MoveBracketLevel(command, reply) => {
                respond(&reply, self.move_bracket_level(&command));
            }
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
            && step.groups.is_empty()
            && step.deleted_groups.is_empty()
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
        for group_id in step.deleted_groups {
            self.state.order_groups.remove(&group_id);
        }
        for group in step.groups {
            self.state
                .order_groups
                .insert(group.group_id.clone(), group);
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
            }
        }
        while self.state.orders.len() > self.retention.maximum_orders {
            let Some(retired) = self
                .state
                .orders
                .values()
                .filter(|order| {
                    !order.status.is_open() && !self.state.broker_brackets.contains_key(&order.id)
                })
                .min_by_key(|order| order.submitted_unix_nanos)
                .map(|order| order.id.clone())
            else {
                break;
            };
            self.state.orders.remove(&retired);
        }
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

/// Rebuilds the in-memory projection base from one position checkpoint. The unrealized mark
/// is not checkpointed: it re-derives from the next quote.
fn position_from_checkpoint(
    checkpoint: &crate::core::ledger::PositionCheckpoint,
) -> Result<Position, String> {
    let unrealized = FixedPoint::try_new(0, checkpoint.realized_pnl.scale())
        .map_err(|error| error.to_string())?;
    Ok(Position {
        account_id: checkpoint.account_id.clone(),
        instrument_id: checkpoint.instrument_id.clone(),
        net_quantity: checkpoint.net_quantity,
        average_entry_price: checkpoint.average_entry_price,
        realized_pnl: checkpoint.realized_pnl,
        unrealized_pnl: unrealized,
        last_fill_unix_nanos: checkpoint.last_fill_unix_nanos,
    })
}

/// The retention fold: replays one instrument's pruned fill prefix onto its previous
/// checkpoint through the same position math live fills use, so recovery from checkpoint
/// plus the retained suffix lands on the pre-retention projection exactly. Returns `None`
/// when the account or instrument is gone; retention keeps those fills.
fn fold_position_checkpoint(
    state: &StoredState,
    key: &(TradingAccountId, InstrumentId),
    base: Option<&crate::core::ledger::PositionCheckpoint>,
    fills: &[Fill],
) -> Result<Option<crate::core::ledger::PositionCheckpoint>, String> {
    let Some(account) = state.accounts.get(&key.0) else {
        return Ok(None);
    };
    let Some(instrument) = state.instruments.get(&key.1) else {
        return Ok(None);
    };
    let mut position = base.map(position_from_checkpoint).transpose()?;
    let mut basis = base.and_then(|checkpoint| checkpoint.cost_basis);
    for fill in fills {
        let (next, next_basis) =
            next_position(position.as_ref(), basis, fill, account, instrument)?;
        position = Some(next);
        basis = next_basis;
    }
    let fill_count = base.map_or(0, |checkpoint| checkpoint.fill_count);
    let Some(position) = position else {
        return Ok(None);
    };
    Ok(Some(crate::core::ledger::PositionCheckpoint {
        account_id: key.0.clone(),
        instrument_id: key.1.clone(),
        net_quantity: position.net_quantity,
        average_entry_price: position.average_entry_price,
        realized_pnl: position.realized_pnl,
        cost_basis: basis,
        last_fill_unix_nanos: position.last_fill_unix_nanos,
        fill_count: fill_count + fills.len() as u64,
    }))
}

pub(crate) fn next_position(
    current: Option<&Position>,
    previous_basis: Option<CostBasis>,
    fill: &Fill,
    account: &TradingAccount,
    instrument: &TradingInstrument,
) -> Result<(Position, Option<CostBasis>), String> {
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
    let combined_scale = fill
        .price
        .scale()
        .checked_add(fill.quantity.scale())
        .ok_or_else(|| "position cost basis scale overflowed".to_string())?;
    let previous_basis = match (previous_basis, previous_quantity) {
        (Some(basis), quantity) if quantity != 0 => Some(basis.rescaled(combined_scale)?),
        _ => None,
    };
    let closed_quantity = if same_direction {
        0
    } else {
        previous_quantity
            .unsigned_abs()
            .min(signed_fill.unsigned_abs())
    };
    // The exact basis carries the money math; the display average derives from it. A
    // position without a basis (a defensive legacy row) keeps the rounded-average math.
    let mut exact_share: Option<i128> = None;
    let next_basis: Option<CostBasis> = if next_quantity == 0 {
        exact_share = previous_basis.map(|basis| basis.units);
        None
    } else if same_direction {
        Some(match previous_basis {
            Some(basis) => basis.added(fill)?,
            None => CostBasis::from_fill(fill)?,
        })
    } else if next_quantity.signum() == previous_quantity.signum() {
        match previous_basis {
            Some(basis) => {
                let (remaining, share) =
                    basis.close(previous_quantity.unsigned_abs(), closed_quantity)?;
                exact_share = Some(share);
                remaining
            }
            None => None,
        }
    } else {
        exact_share = previous_basis.map(|basis| basis.units);
        Some(CostBasis {
            units: i128::from(fill.price.units())
                .checked_mul(i128::from(next_quantity.unsigned_abs()))
                .ok_or_else(|| "position cost basis overflowed".to_string())?,
            scale: combined_scale,
        })
    };
    let average_entry_price = next_average_entry(
        current,
        next_basis,
        same_direction,
        previous_quantity,
        next_quantity,
        fill,
    )?;
    let prior_realized = current.map_or(
        FixedPoint::try_new(0, account.currency_scale).map_err(|error| error.to_string())?,
        |position| position.realized_pnl,
    );
    let realized_change = realized_change(
        current,
        exact_share,
        closed_quantity,
        previous_quantity,
        fill,
        account,
        instrument,
    )?;
    let realized_pnl = prior_realized
        .checked_add(realized_change)
        .map_err(|error| error.to_string())?;
    Ok((
        Position {
            account_id: fill.account_id.clone(),
            instrument_id: fill.instrument_id.clone(),
            net_quantity: FixedPoint::try_new(next_quantity, fill.quantity.scale())
                .map_err(|error| error.to_string())?,
            average_entry_price,
            realized_pnl,
            unrealized_pnl: FixedPoint::try_new(0, account.currency_scale)
                .map_err(|error| error.to_string())?,
            last_fill_unix_nanos: fill.execution_unix_nanos,
        },
        next_basis,
    ))
}

/// The display average after a fill: derived from the exact basis when one exists, else the
/// legacy rounded-average math a defensive pre-basis row keeps.
fn next_average_entry(
    current: Option<&Position>,
    next_basis: Option<CostBasis>,
    same_direction: bool,
    previous_quantity: i64,
    next_quantity: i64,
    fill: &Fill,
) -> Result<Option<FixedPoint>, String> {
    if next_quantity == 0 {
        return Ok(None);
    }
    if let Some(basis) = next_basis {
        let average =
            aeris_trading::round_divide_i128(basis.units, i128::from(next_quantity.unsigned_abs()))
                .map_err(|error| error.to_string())?;
        return FixedPoint::try_new(
            i64::try_from(average).map_err(|_| "position average overflowed".to_string())?,
            fill.price.scale(),
        )
        .map(Some)
        .map_err(|error| error.to_string());
    }
    if same_direction {
        return match current.and_then(|position| position.average_entry_price) {
            None => Ok(Some(fill.price)),
            Some(previous_average) => weighted_average_entry_price(
                previous_average,
                previous_quantity.unsigned_abs(),
                fill.price,
                fill.quantity.units().unsigned_abs(),
            )
            .map(Some),
        };
    }
    if next_quantity.signum() == previous_quantity.signum() {
        return Ok(current.and_then(|position| position.average_entry_price));
    }
    Ok(Some(fill.price))
}

/// The realized P&L change of one fill: the exact basis-share value when a basis exists,
/// else the legacy rounded-average computation a defensive pre-basis row keeps.
fn realized_change(
    current: Option<&Position>,
    exact_share: Option<i128>,
    closed_quantity: u64,
    previous_quantity: i64,
    fill: &Fill,
    account: &TradingAccount,
    instrument: &TradingInstrument,
) -> Result<FixedPoint, String> {
    let zero = FixedPoint::try_new(0, account.currency_scale).map_err(|error| error.to_string())?;
    if closed_quantity == 0 {
        return Ok(zero);
    }
    if let Some(share) = exact_share {
        let point_value = instrument
            .contract
            .point_value
            .ok_or_else(|| "instrument point value is unavailable".to_string())?;
        let exit_value = i128::from(fill.price.units())
            .checked_mul(i128::from(closed_quantity))
            .ok_or_else(|| "realized PnL overflowed".to_string())?;
        let gross = exit_value
            .checked_sub(share)
            .and_then(|value| value.checked_mul(i128::from(previous_quantity.signum())))
            .and_then(|value| value.checked_mul(i128::from(point_value.units())))
            .ok_or_else(|| "realized PnL overflowed".to_string())?;
        let combined_scale = fill
            .price
            .scale()
            .checked_add(fill.quantity.scale())
            .ok_or_else(|| "position cost basis scale overflowed".to_string())?;
        let raw_scale = combined_scale
            .checked_add(point_value.scale())
            .ok_or_else(|| "realized PnL scale overflowed".to_string())?;
        return aeris_trading::fixed_point_from_i128_rounded(
            gross,
            raw_scale,
            account.currency_scale,
        )
        .map_err(|error| error.to_string());
    }
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
    )
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
