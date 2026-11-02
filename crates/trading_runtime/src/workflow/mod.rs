//! User-accepted trading workflows: a trigger and a small acyclic graph of If and action
//! nodes, with limits, built from a closed set of blocks. The trading owner alone evaluates
//! an armed workflow, on its own verified quotes and clock, and every order it places goes
//! through the same admission and risk checks as a manual order. Where each node sits on
//! the board is a separate layout, so moving a node never changes the accepted revision.

use crate::risk::session::{RiskSessionBoundary, chicago_local_to_unix_seconds};
use aeris_instruments::InstrumentId;
use aeris_instruments::calendar::chicago_utc_offset_seconds;
use aeris_trading::{ClientOrderId, FixedPoint, OrderSide, TradingAccountId};
use std::collections::BTreeSet;
use std::fmt;

/// The owner holds at most this many workflows across every account.
pub const MAXIMUM_WORKFLOWS: usize = 64;
/// If and action nodes in one workflow, besides its trigger.
pub const MAXIMUM_WORKFLOW_NODES: usize = 32;
/// Triggers one workflow may have; any of them fires it.
pub const MAXIMUM_WORKFLOW_TRIGGERS: usize = 4;
/// Checks one If may join.
pub const MAXIMUM_WORKFLOW_CHECKS: usize = 4;
/// The most firings one workflow may make in one risk session.
pub const MAXIMUM_WORKFLOW_SESSION_FIRINGS: u32 = 50;
pub const MAXIMUM_WORKFLOW_COOLDOWN_SECONDS: u32 = 86_400;
pub const MAXIMUM_WORKFLOW_PROTECTION_TICKS: u32 = 10_000;
/// The longest time in trade a check can name: one week.
pub const MAXIMUM_WORKFLOW_TRADE_MINUTES: u32 = 10_080;
/// The newest firings kept, durably and in the working set, per workflow.
pub const WORKFLOW_FIRING_HISTORY: usize = 20;
pub const MAXIMUM_WORKFLOW_ID_BYTES: usize = 64;
pub const MAXIMUM_WORKFLOW_NAME_CHARS: usize = 80;
/// Board coordinates stay within this distance of the origin on each axis.
pub const MAXIMUM_WORKFLOW_LAYOUT_COORDINATE: i32 = 100_000;
pub const MINUTES_PER_DAY: u16 = 1_440;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const DAY_SECONDS: i64 = 86_400;

/// One workflow revision exactly as the user reviewed and accepted it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDefinition {
    pub workflow_id: String,
    pub revision: u32,
    pub name: String,
    pub account_id: TradingAccountId,
    pub scope: WorkflowScope,
    /// One to [`MAXIMUM_WORKFLOW_TRIGGERS`] triggers; any of them fires the workflow, and
    /// triggers due in the same owner step fire it once.
    pub triggers: Vec<WorkflowTrigger>,
    /// The node the triggers lead to.
    pub start: WorkflowNodeId,
    /// Every If and action node; each is reachable from `start` and none is reached twice
    /// on one run because the graph has no cycle.
    pub nodes: Vec<WorkflowNode>,
    pub limits: WorkflowLimits,
}

/// The symbols a workflow watches and acts on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowScope {
    /// One instrument: every trigger, check and action reads or trades it.
    Instrument(InstrumentId),
    /// Every symbol the account holds. A run starts on every position (or, fired by a
    /// position trigger, on the symbols whose position opened or closed); an If whose
    /// checks read the position holds when any of those positions passes, and its `then`
    /// branch goes on with only the positions that passed; a condition trigger starts a run
    /// on the positions whose condition became true. Price triggers, price checks and
    /// entries need one symbol and are refused.
    AnySymbol,
}

impl WorkflowScope {
    #[must_use]
    pub const fn instrument(&self) -> Option<&InstrumentId> {
        match self {
            Self::Instrument(instrument_id) => Some(instrument_id),
            Self::AnySymbol => None,
        }
    }
}

/// The symbols one run acts on, narrowed by the position checks it passes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkflowTargets {
    /// Every symbol of the account.
    Every,
    Only(BTreeSet<InstrumentId>),
}

impl WorkflowTargets {
    pub(crate) fn includes(&self, instrument_id: &InstrumentId) -> bool {
        match self {
            Self::Every => true,
            Self::Only(instruments) => instruments.contains(instrument_id),
        }
    }

    /// The symbols of either.
    pub(crate) fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Only(mut instruments), Self::Only(more)) => {
                instruments.extend(more);
                Self::Only(instruments)
            }
            (Self::Every, _) | (_, Self::Every) => Self::Every,
        }
    }
}

/// A node's identity within its workflow; the trigger is always [`WorkflowNodeId::TRIGGER`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkflowNodeId(pub u16);

impl WorkflowNodeId {
    pub const TRIGGER: Self = Self(0);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowNode {
    pub id: WorkflowNodeId,
    pub step: WorkflowStep,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowStep {
    /// Continues at `then` when the condition holds and at `otherwise` when it does not; a
    /// branch without a node ends the run.
    If {
        condition: WorkflowCondition,
        then: Option<WorkflowNodeId>,
        otherwise: Option<WorkflowNodeId>,
    },
    /// Runs the action, then continues at `next`. A refused action never stops the run.
    Action {
        action: WorkflowAction,
        next: Option<WorkflowNodeId>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowTrigger {
    /// Fires when the market moves from one side of `level` to the other: `Above` when the
    /// bid reaches the level or higher, `Below` when the ask reaches the level or lower. The
    /// first quote after arming or a restart only records which side the market is on.
    PriceCrosses {
        direction: PriceDirection,
        level: FixedPoint,
    },
    /// Fires once a day at a local time. A time that passes while Aeris is closed does not
    /// fire later.
    TimeOfDay(WorkflowClockTime),
    /// Fires at UTC midnight and every `minutes` after it, so every 15 minutes fires at
    /// :00, :15, :30 and :45. A moment that passes while Aeris is closed does not fire later.
    Every { minutes: u16 },
    /// Fires when the account's position in a workflow symbol opens from flat or reverses
    /// into its other side. Arming or a restart only records the current positions.
    PositionOpened,
    /// Fires when the account's position in a workflow symbol closes to flat or reverses.
    /// Arming or a restart only records the current positions.
    PositionClosed,
    /// Fires when the condition becomes true: it was read false at the previous owner step
    /// and reads true now. The owner reads it after every command, market batch and timer
    /// step, and at each whole minute while a check reads the clock; arming or a restart
    /// only records whether it holds. On any symbol a condition with a position check is
    /// read for each open position, and a run starts on the symbols that became true.
    ConditionMet(WorkflowCondition),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PriceDirection {
    Above,
    Below,
}

/// A minute of the day in one of the zones a workflow can name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowClockTime {
    pub zone: WorkflowTimeZone,
    pub minute_of_day: u16,
}

/// Chicago is the CME exchange time; its daylight saving rule is the futures calendar's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowTimeZone {
    Utc,
    Chicago,
}

/// An If's test: one to [`MAXIMUM_WORKFLOW_CHECKS`] checks that must all hold or of which
/// any must hold. Every check is read from the owner's state at the moment the trigger
/// fires, before any action of that run, so an order a run places never changes which
/// branch it took.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowCondition {
    join: WorkflowJoin,
    /// Packed from the front; the first slot is always filled.
    checks: [Option<WorkflowCheck>; MAXIMUM_WORKFLOW_CHECKS],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowJoin {
    All,
    Any,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowCheck {
    /// The account's position in the symbol it is read for.
    Position(PositionRequirement),
    /// The local time is at or after `start` and before `end`; a window whose end is
    /// earlier than its start spans midnight.
    Between {
        zone: WorkflowTimeZone,
        start_minute: u16,
        end_minute: u16,
    },
    /// `Above` holds while the bid is at or above `level`, `Below` while the ask is at or
    /// below it.
    Price {
        direction: PriceDirection,
        level: FixedPoint,
    },
    /// Holds while the bid is at or above `low` and the ask at or below `high`.
    PriceRange { low: FixedPoint, high: FixedPoint },
    /// Holds on the named local days of the week.
    Weekday {
        zone: WorkflowTimeZone,
        days: WorkflowWeekdays,
    },
    /// How long the position has been open. Never holds while flat.
    TimeInTrade {
        compare: WorkflowCompare,
        minutes: u32,
    },
    /// The open position's distance from its average entry to the price it would exit at
    /// (the bid when long, the ask when short) in the instrument's ticks; a loss is
    /// negative. Never holds while flat.
    OpenProfitTicks {
        compare: WorkflowCompare,
        ticks: i32,
    },
    /// The open position's unrealized profit in the account currency; a loss is negative.
    /// Never holds while flat.
    OpenProfit {
        compare: WorkflowCompare,
        amount: FixedPoint,
    },
    /// The exit price has come back at least `ticks` from the best exit price since the
    /// position opened. Never holds while flat.
    Pullback { ticks: u32 },
    /// The account's P&L today in the account currency, a loss negative: the realized P&L
    /// of the fills executed since the current risk session started (the account's risk
    /// session boundary, or UTC midnight when it has none) plus the open P&L of every
    /// position. Read for the whole account, not one symbol.
    DayProfit {
        compare: WorkflowCompare,
        amount: FixedPoint,
    },
}

/// `AtLeast` holds at or above the value it is given, `AtMost` at or below it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowCompare {
    AtLeast,
    AtMost,
}

impl WorkflowCompare {
    #[must_use]
    pub fn holds<T: Ord + Copy>(self, value: T, threshold: T) -> bool {
        match self {
            Self::AtLeast => value >= threshold,
            Self::AtMost => value <= threshold,
        }
    }
}

/// A non-empty set of days of the week; bit 0 is Monday and bit 6 Sunday.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowWeekdays(u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionRequirement {
    Flat,
    Long,
    Short,
    Open,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowAction {
    /// A market order, with an owner-managed stop and target when `protection` is set.
    Enter {
        side: OrderSide,
        quantity: FixedPoint,
        protection: Option<WorkflowProtection>,
    },
    /// Closes the account's position in each symbol the run acts on.
    Flatten,
    /// Cancels the account's working orders in each symbol the run acts on.
    CancelOrders,
    /// Locks the account against new orders until the user unlocks it.
    LockAccount,
    /// Records that the workflow reached this block, which the desktop announces; it sends
    /// nothing to a venue.
    Notify,
}

impl WorkflowAction {
    /// Whether the action sends anything to the account's venue; a lock and a notice stay
    /// in the owner.
    #[must_use]
    pub const fn needs_venue(self) -> bool {
        matches!(
            self,
            Self::Enter { .. } | Self::Flatten | Self::CancelOrders
        )
    }
}

/// Stop and target distances from the entry fill, in the instrument's ticks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowProtection {
    pub stop_ticks: u32,
    pub target_ticks: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowRepeat {
    /// The first firing completes the workflow.
    Once,
    /// At most `maximum` firings in each risk session of the account.
    PerSession { maximum: u32 },
    /// Every trigger fires it, kept apart only by the cooldown.
    Always,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowLimits {
    pub repeat: WorkflowRepeat,
    /// The least time between two firings.
    pub cooldown_seconds: u32,
    pub expires_unix_nanos: Option<i64>,
}

/// Where the board draws each node. A node without a position is placed by the board.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkflowLayout {
    pub positions: Vec<WorkflowNodePosition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowNodePosition {
    pub node: WorkflowNodeId,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowState {
    /// Saved but not running; every new revision starts here.
    Paused,
    Armed,
    /// A `Once` workflow fired.
    Completed,
    Expired,
}

impl WorkflowState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::Armed => "armed",
            Self::Completed => "completed",
            Self::Expired => "expired",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "paused" => Some(Self::Paused),
            "armed" => Some(Self::Armed),
            "completed" => Some(Self::Completed),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// Why a workflow cannot act right now. Checked when it is armed, published while it is
/// armed and checked again, for the branch taken, when it fires.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WorkflowBlocker {
    AccountMissing,
    InstrumentTermsMissing,
    AccountLocked,
    BrokerUnreachable,
    QuoteUnavailable,
    /// A check reads the position's open time or entry price, and fills Aeris never saw
    /// opened it (before this version, or at the broker while Aeris was away), or reads a
    /// P&L the owner cannot sum exactly.
    PositionUnknown,
}

impl WorkflowBlocker {
    /// Blockers that cannot clear while the workflow is armed; arming refuses them.
    #[must_use]
    pub const fn prevents_arming(self) -> bool {
        matches!(self, Self::AccountMissing | Self::InstrumentTermsMissing)
    }
}

/// One firing of an accepted revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowFiring {
    pub sequence: u64,
    pub revision: u32,
    pub fired_unix_nanos: i64,
    pub outcome: WorkflowFiringOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowFiringOutcome {
    /// Every node the run passed, in order: each If with the branch it took and each
    /// action with its result.
    Executed(Vec<WorkflowStepResult>),
    /// The trigger fired while the branch it would take could not act; nothing was sent.
    Blocked(Vec<WorkflowBlocker>),
    /// The firing was recorded but Aeris stopped before its actions reported back.
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowStepResult {
    pub node: WorkflowNodeId,
    pub outcome: WorkflowStepOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowStepOutcome {
    /// The If's condition held (`true`) or did not.
    Branch(bool),
    Action(WorkflowActionResult),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowActionResult {
    Placed { client_order_id: ClientOrderId },
    Flattened,
    Cancelled { orders: usize },
    Locked,
    Notified,
    Refused { detail: String },
}

/// The published view of one workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowView {
    pub definition: WorkflowDefinition,
    pub layout: WorkflowLayout,
    pub state: WorkflowState,
    pub session_firings: u32,
    pub total_firings: u64,
    pub last_attempt_unix_nanos: Option<i64>,
    /// The next time a time-of-day workflow fires while armed.
    pub next_due_unix_nanos: Option<i64>,
    /// Empty when an armed workflow can act on its next trigger.
    pub blockers: Vec<WorkflowBlocker>,
    /// The newest firings, newest first.
    pub recent_firings: Vec<WorkflowFiring>,
}

/// The durable record of one workflow beside its accepted definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkflowRecord {
    pub definition: WorkflowDefinition,
    pub layout: WorkflowLayout,
    pub state: WorkflowState,
    /// The start of the risk session `session_firings` counts in.
    pub session_start_unix_nanos: i64,
    pub session_firings: u32,
    pub total_firings: u64,
    pub last_sequence: u64,
    pub last_attempt_unix_nanos: Option<i64>,
}

/// A refused workflow command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowError {
    Invalid(WorkflowInvalid),
    TooManyWorkflows,
    UnknownWorkflow,
    UnknownAccount,
    UnknownInstrument,
    RevisionMustAdvance {
        current: u32,
    },
    /// The command names a revision other than the stored one.
    StaleRevision {
        current: u32,
    },
    /// A `Once` workflow fired; a new revision can run it again.
    Completed,
    Expired,
    NotReady(Vec<WorkflowBlocker>),
    /// The trading owner, its queue or its store failed.
    Owner(String),
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(invalid) => write!(formatter, "workflow is invalid: {invalid:?}"),
            Self::TooManyWorkflows => {
                write!(
                    formatter,
                    "at most {MAXIMUM_WORKFLOWS} workflows are allowed"
                )
            }
            Self::UnknownWorkflow => formatter.write_str("workflow is not registered"),
            Self::UnknownAccount => formatter.write_str("workflow account is not registered"),
            Self::UnknownInstrument => {
                formatter.write_str("workflow instrument has no registered terms")
            }
            Self::RevisionMustAdvance { current } => {
                write!(formatter, "workflow revision must advance past {current}")
            }
            Self::StaleRevision { current } => {
                write!(
                    formatter,
                    "workflow revision is stale; current is {current}"
                )
            }
            Self::Completed => formatter.write_str("workflow has completed"),
            Self::Expired => formatter.write_str("workflow has expired"),
            Self::NotReady(blockers) => write!(formatter, "workflow is not ready: {blockers:?}"),
            Self::Owner(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for WorkflowError {}

/// The part of a definition or layout validation refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowInvalid {
    Identity,
    Revision,
    Name,
    Level,
    Time,
    /// No trigger or too many.
    Triggers,
    /// No node, too many, a repeated identity or the trigger's identity on a node.
    Nodes,
    /// A link to a node that does not exist or to the node itself.
    Link,
    Cycle,
    /// A node the trigger never reaches.
    Unreachable,
    /// No action.
    Actions,
    Quantity,
    Protection,
    /// A tick distance out of its bounds.
    Ticks,
    /// A money amount not at the account currency's scale.
    Amount,
    Repeat,
    Cooldown,
    Expiry,
    Layout,
    /// A price trigger, price check or entry in a workflow on any symbol.
    Scope,
}

/// One node a run passes, decided before any action runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkflowPathStep {
    Branch(WorkflowNodeId, bool),
    /// An action with the symbols it acts on.
    Action(WorkflowNodeId, WorkflowAction, WorkflowTargets),
}

impl WorkflowStep {
    /// The nodes this one leads to.
    pub fn links(self) -> impl Iterator<Item = WorkflowNodeId> {
        let (first, second) = match self {
            Self::If {
                then, otherwise, ..
            } => (then, otherwise),
            Self::Action { next, .. } => (next, None),
        };
        first.into_iter().chain(second)
    }
}

impl WorkflowDefinition {
    #[must_use]
    pub fn node(&self, id: WorkflowNodeId) -> Option<&WorkflowNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Every action anywhere in the graph.
    pub fn actions(&self) -> impl Iterator<Item = WorkflowAction> + '_ {
        self.nodes.iter().filter_map(|node| match node.step {
            WorkflowStep::Action { action, .. } => Some(action),
            WorkflowStep::If { .. } => None,
        })
    }

    /// Every check of a condition trigger and of every If anywhere in the graph.
    pub fn checks(&self) -> impl Iterator<Item = WorkflowCheck> + '_ {
        self.trigger_conditions()
            .chain(self.nodes.iter().filter_map(|node| match &node.step {
                WorkflowStep::If { condition, .. } => Some(condition),
                WorkflowStep::Action { .. } => None,
            }))
            .flat_map(WorkflowCondition::checks)
    }

    /// The condition of every condition trigger.
    pub fn trigger_conditions(&self) -> impl Iterator<Item = &WorkflowCondition> + '_ {
        self.triggers.iter().filter_map(|trigger| match trigger {
            WorkflowTrigger::ConditionMet(condition) => Some(condition),
            WorkflowTrigger::PriceCrosses { .. }
            | WorkflowTrigger::TimeOfDay(_)
            | WorkflowTrigger::Every { .. }
            | WorkflowTrigger::PositionOpened
            | WorkflowTrigger::PositionClosed => None,
        })
    }

    /// Whether a block reads one symbol's price or trades it: a price trigger, a price check
    /// or an entry, which a workflow on any symbol cannot have.
    #[must_use]
    pub fn needs_one_symbol(&self) -> bool {
        self.triggers
            .iter()
            .any(|trigger| matches!(trigger, WorkflowTrigger::PriceCrosses { .. }))
            || self.checks().any(WorkflowCheck::reads_one_symbol_price)
            || self
                .actions()
                .any(|action| matches!(action, WorkflowAction::Enter { .. }))
    }

    /// The symbols a run fired by a price or time trigger starts on.
    pub(crate) fn all_targets(&self) -> WorkflowTargets {
        match &self.scope {
            WorkflowScope::Instrument(instrument_id) => {
                WorkflowTargets::Only(BTreeSet::from([instrument_id.clone()]))
            }
            WorkflowScope::AnySymbol => WorkflowTargets::Every,
        }
    }

    /// The earliest moment strictly after `unix_nanos` at which a timed trigger fires.
    #[must_use]
    pub fn next_timed_trigger_after(&self, unix_nanos: i64) -> Option<i64> {
        self.triggers
            .iter()
            .filter_map(|trigger| trigger.next_after(unix_nanos))
            .min()
    }

    /// Validates the definition on its own; the owner also checks it against the
    /// account and instrument terms.
    ///
    /// # Errors
    /// Returns the first block that is out of its bounds.
    pub fn validate(&self) -> Result<(), WorkflowInvalid> {
        if self.workflow_id.is_empty()
            || self.workflow_id.len() > MAXIMUM_WORKFLOW_ID_BYTES
            || !self
                .workflow_id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(WorkflowInvalid::Identity);
        }
        if self.revision == 0 {
            return Err(WorkflowInvalid::Revision);
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > MAXIMUM_WORKFLOW_NAME_CHARS
            || self.name.chars().any(char::is_control)
        {
            return Err(WorkflowInvalid::Name);
        }
        if self.triggers.is_empty() || self.triggers.len() > MAXIMUM_WORKFLOW_TRIGGERS {
            return Err(WorkflowInvalid::Triggers);
        }
        for trigger in &self.triggers {
            match *trigger {
                WorkflowTrigger::PriceCrosses { level, .. } => validate_level(level)?,
                WorkflowTrigger::TimeOfDay(time) => validate_minute(time.minute_of_day)?,
                WorkflowTrigger::Every { minutes } => {
                    if minutes == 0 || minutes > MINUTES_PER_DAY {
                        return Err(WorkflowInvalid::Time);
                    }
                }
                WorkflowTrigger::PositionOpened | WorkflowTrigger::PositionClosed => {}
                WorkflowTrigger::ConditionMet(condition) => {
                    condition.checks().try_for_each(validate_check)?;
                }
            }
        }
        self.validate_graph()?;
        if self.scope == WorkflowScope::AnySymbol && self.needs_one_symbol() {
            return Err(WorkflowInvalid::Scope);
        }
        for node in &self.nodes {
            match node.step {
                WorkflowStep::If { condition, .. } => {
                    condition.checks().try_for_each(validate_check)?;
                }
                WorkflowStep::Action { action, .. } => validate_action(action)?,
            }
        }
        match self.limits.repeat {
            WorkflowRepeat::Once | WorkflowRepeat::Always => {}
            WorkflowRepeat::PerSession { maximum } => {
                if !(1..=MAXIMUM_WORKFLOW_SESSION_FIRINGS).contains(&maximum) {
                    return Err(WorkflowInvalid::Repeat);
                }
            }
        }
        if self.limits.cooldown_seconds > MAXIMUM_WORKFLOW_COOLDOWN_SECONDS {
            return Err(WorkflowInvalid::Cooldown);
        }
        if self
            .limits
            .expires_unix_nanos
            .is_some_and(|expiry| expiry <= 0)
        {
            return Err(WorkflowInvalid::Expiry);
        }
        Ok(())
    }

    /// Bounded, uniquely named nodes whose links resolve, reached from `start` without a
    /// cycle, with at least one action.
    fn validate_graph(&self) -> Result<(), WorkflowInvalid> {
        if self.nodes.is_empty() || self.nodes.len() > MAXIMUM_WORKFLOW_NODES {
            return Err(WorkflowInvalid::Nodes);
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            if node.id == WorkflowNodeId::TRIGGER || !ids.insert(node.id) {
                return Err(WorkflowInvalid::Nodes);
            }
        }
        if !ids.contains(&self.start) {
            return Err(WorkflowInvalid::Link);
        }
        for node in &self.nodes {
            if node
                .step
                .links()
                .any(|link| link == node.id || !ids.contains(&link))
            {
                return Err(WorkflowInvalid::Link);
            }
        }
        // Iterative depth-first walk: `open` holds the nodes on the current path.
        let mut finished = BTreeSet::new();
        let mut open = BTreeSet::new();
        let mut stack = vec![(self.start, false)];
        while let Some((id, leaving)) = stack.pop() {
            if leaving {
                open.remove(&id);
                finished.insert(id);
                continue;
            }
            if finished.contains(&id) {
                continue;
            }
            if !open.insert(id) {
                return Err(WorkflowInvalid::Cycle);
            }
            stack.push((id, true));
            let node = self.node(id).ok_or(WorkflowInvalid::Link)?;
            for link in node.step.links() {
                if open.contains(&link) {
                    return Err(WorkflowInvalid::Cycle);
                }
                if !finished.contains(&link) {
                    stack.push((link, false));
                }
            }
        }
        if finished.len() != self.nodes.len() {
            return Err(WorkflowInvalid::Unreachable);
        }
        if self.actions().next().is_none() {
            return Err(WorkflowInvalid::Actions);
        }
        Ok(())
    }

    /// The nodes one run passes from `start` on `targets`, deciding every If with `decide`,
    /// which may narrow the targets its `then` branch goes on with. Fails with what stops a
    /// check on the path from being read (a price check without a current quote).
    pub(crate) fn resolve_path(
        &self,
        mut targets: WorkflowTargets,
        mut decide: impl FnMut(
            &WorkflowCondition,
            &mut WorkflowTargets,
        ) -> Result<bool, WorkflowBlocker>,
    ) -> Result<Vec<WorkflowPathStep>, WorkflowBlocker> {
        let mut path = Vec::new();
        let mut next = Some(self.start);
        // An acyclic graph visits each node at most once; the bound guards a corrupt one.
        while let Some(id) = next.take() {
            if path.len() >= self.nodes.len() {
                break;
            }
            let Some(node) = self.node(id) else {
                break;
            };
            match node.step {
                WorkflowStep::If {
                    condition,
                    then,
                    otherwise,
                } => {
                    let held = decide(&condition, &mut targets)?;
                    path.push(WorkflowPathStep::Branch(id, held));
                    next = if held { then } else { otherwise };
                }
                WorkflowStep::Action {
                    action,
                    next: after,
                } => {
                    path.push(WorkflowPathStep::Action(id, action, targets.clone()));
                    next = after;
                }
            }
        }
        Ok(path)
    }
}

impl WorkflowLayout {
    /// Positions only for the trigger and the definition's nodes, each at most once and
    /// within the board bound.
    ///
    /// # Errors
    /// Returns [`WorkflowInvalid::Layout`] for any other position.
    pub fn validate(&self, definition: &WorkflowDefinition) -> Result<(), WorkflowInvalid> {
        let mut seen = BTreeSet::new();
        let bound = -MAXIMUM_WORKFLOW_LAYOUT_COORDINATE..=MAXIMUM_WORKFLOW_LAYOUT_COORDINATE;
        for position in &self.positions {
            let known = position.node == WorkflowNodeId::TRIGGER
                || definition.node(position.node).is_some();
            if !known
                || !seen.insert(position.node)
                || !bound.contains(&position.x)
                || !bound.contains(&position.y)
            {
                return Err(WorkflowInvalid::Layout);
            }
        }
        Ok(())
    }
}

const fn validate_level(level: FixedPoint) -> Result<(), WorkflowInvalid> {
    if level.units() <= 0 {
        Err(WorkflowInvalid::Level)
    } else {
        Ok(())
    }
}

const fn validate_minute(minute: u16) -> Result<(), WorkflowInvalid> {
    if minute >= MINUTES_PER_DAY {
        Err(WorkflowInvalid::Time)
    } else {
        Ok(())
    }
}

fn validate_check(check: WorkflowCheck) -> Result<(), WorkflowInvalid> {
    let ticks = 1..=MAXIMUM_WORKFLOW_PROTECTION_TICKS;
    match check {
        WorkflowCheck::Position(_)
        | WorkflowCheck::Weekday { .. }
        | WorkflowCheck::OpenProfit { .. }
        | WorkflowCheck::DayProfit { .. } => Ok(()),
        WorkflowCheck::TimeInTrade { minutes, .. } => {
            if (1..=MAXIMUM_WORKFLOW_TRADE_MINUTES).contains(&minutes) {
                Ok(())
            } else {
                Err(WorkflowInvalid::Time)
            }
        }
        WorkflowCheck::OpenProfitTicks {
            ticks: distance, ..
        } => {
            if ticks.contains(&distance.unsigned_abs()) {
                Ok(())
            } else {
                Err(WorkflowInvalid::Ticks)
            }
        }
        WorkflowCheck::Pullback { ticks: distance } => {
            if ticks.contains(&distance) {
                Ok(())
            } else {
                Err(WorkflowInvalid::Ticks)
            }
        }
        WorkflowCheck::Between {
            start_minute,
            end_minute,
            ..
        } => {
            validate_minute(start_minute)?;
            validate_minute(end_minute)?;
            if start_minute == end_minute {
                return Err(WorkflowInvalid::Time);
            }
            Ok(())
        }
        WorkflowCheck::Price { level, .. } => validate_level(level),
        WorkflowCheck::PriceRange { low, high } => {
            validate_level(low)?;
            if low.scale() != high.scale() || high.units() <= low.units() {
                return Err(WorkflowInvalid::Level);
            }
            Ok(())
        }
    }
}

impl WorkflowCondition {
    /// A test of one check.
    #[must_use]
    pub const fn single(check: WorkflowCheck) -> Self {
        let mut checks = [None; MAXIMUM_WORKFLOW_CHECKS];
        checks[0] = Some(check);
        Self {
            join: WorkflowJoin::All,
            checks,
        }
    }

    /// `None` for no checks or more than [`MAXIMUM_WORKFLOW_CHECKS`].
    #[must_use]
    pub fn joined(join: WorkflowJoin, checks: &[WorkflowCheck]) -> Option<Self> {
        if checks.is_empty() || checks.len() > MAXIMUM_WORKFLOW_CHECKS {
            return None;
        }
        let mut slots = [None; MAXIMUM_WORKFLOW_CHECKS];
        for (slot, check) in slots.iter_mut().zip(checks) {
            *slot = Some(*check);
        }
        Some(Self {
            join,
            checks: slots,
        })
    }

    #[must_use]
    pub const fn join(&self) -> WorkflowJoin {
        self.join
    }

    pub fn checks(&self) -> impl Iterator<Item = WorkflowCheck> + '_ {
        self.checks.iter().flatten().copied()
    }

    /// Reads every check, so a test never holds on a check it could not read. Fails with
    /// what stops the first unreadable check.
    pub(crate) fn holds(
        &self,
        mut holds: impl FnMut(WorkflowCheck) -> Result<bool, WorkflowBlocker>,
    ) -> Result<bool, WorkflowBlocker> {
        let mut all = true;
        let mut any = false;
        for check in self.checks() {
            let held = holds(check)?;
            all &= held;
            any |= held;
        }
        Ok(match self.join {
            WorkflowJoin::All => all,
            WorkflowJoin::Any => any,
        })
    }
}

impl WorkflowCheck {
    /// Whether the check reads the account's position, so on any symbol it is read for each
    /// position the run acts on.
    #[must_use]
    pub const fn reads_position(self) -> bool {
        matches!(
            self,
            Self::Position(_)
                | Self::TimeInTrade { .. }
                | Self::OpenProfitTicks { .. }
                | Self::OpenProfit { .. }
                | Self::Pullback { .. }
        )
    }

    /// Whether the check compares one symbol's price with a level.
    #[must_use]
    pub const fn reads_one_symbol_price(self) -> bool {
        matches!(self, Self::Price { .. } | Self::PriceRange { .. })
    }

    /// Whether the check reads the owner's quote.
    #[must_use]
    pub const fn reads_quote(self) -> bool {
        matches!(
            self,
            Self::Price { .. }
                | Self::PriceRange { .. }
                | Self::OpenProfitTicks { .. }
                | Self::Pullback { .. }
        )
    }

    /// Whether the check's answer can change with the time alone.
    #[must_use]
    pub const fn reads_clock(self) -> bool {
        matches!(
            self,
            Self::Between { .. } | Self::Weekday { .. } | Self::TimeInTrade { .. }
        )
    }
}

impl WorkflowWeekdays {
    /// Monday to Friday.
    pub const WEEKDAYS: Self = Self(0b001_1111);

    /// `None` for an empty set or a bit past Sunday.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits == 0 || bits >= 1 << 7 {
            None
        } else {
            Some(Self(bits))
        }
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `day` counts from Monday as 0.
    #[must_use]
    pub const fn contains(self, day: u8) -> bool {
        day < 7 && self.0 & (1 << day) != 0
    }
}

impl WorkflowTrigger {
    /// The first moment strictly after `unix_nanos` at which a timed trigger fires.
    #[must_use]
    pub fn next_after(self, unix_nanos: i64) -> Option<i64> {
        match self {
            Self::PriceCrosses { .. }
            | Self::PositionOpened
            | Self::PositionClosed
            | Self::ConditionMet(_) => None,
            Self::TimeOfDay(time) => Some(time.next_after(unix_nanos)),
            Self::Every { minutes } => {
                let period = i64::from(minutes) * 60 * NANOS_PER_SECOND;
                let day = DAY_SECONDS * NANOS_PER_SECOND;
                let day_start = unix_nanos.div_euclid(day) * day;
                let next =
                    day_start + (unix_nanos - day_start).div_euclid(period) * period + period;
                // A period that does not divide the day restarts at the next midnight.
                Some(next.min(day_start + day))
            }
        }
    }
}

fn validate_action(action: WorkflowAction) -> Result<(), WorkflowInvalid> {
    match action {
        WorkflowAction::Enter {
            quantity,
            protection,
            ..
        } => {
            if quantity.units() <= 0 {
                return Err(WorkflowInvalid::Quantity);
            }
            let ticks = 1..=MAXIMUM_WORKFLOW_PROTECTION_TICKS;
            if protection.is_some_and(|protection| {
                !ticks.contains(&protection.stop_ticks) || !ticks.contains(&protection.target_ticks)
            }) {
                return Err(WorkflowInvalid::Protection);
            }
            Ok(())
        }
        // A lock refuses only later entries, so a flatten, a cancel or a notice may
        // follow it.
        WorkflowAction::LockAccount
        | WorkflowAction::Flatten
        | WorkflowAction::CancelOrders
        | WorkflowAction::Notify => Ok(()),
    }
}

impl WorkflowTimeZone {
    fn local_seconds(self, unix_seconds: i64) -> i64 {
        match self {
            Self::Utc => unix_seconds,
            Self::Chicago => unix_seconds + chicago_utc_offset_seconds(unix_seconds),
        }
    }

    fn unix_seconds(self, local_seconds: i64) -> i64 {
        match self {
            Self::Utc => local_seconds,
            Self::Chicago => chicago_local_to_unix_seconds(local_seconds),
        }
    }

    /// The local day of the week at `unix_nanos`, Monday as 0.
    #[must_use]
    pub fn weekday(self, unix_nanos: i64) -> u8 {
        let day = self
            .local_seconds(unix_nanos.div_euclid(NANOS_PER_SECOND))
            .div_euclid(DAY_SECONDS);
        // 1970-01-01 was a Thursday.
        u8::try_from((day + 3).rem_euclid(7)).unwrap_or(0)
    }

    /// The local minute of the day at `unix_nanos`.
    #[must_use]
    pub fn minute_of_day(self, unix_nanos: i64) -> u16 {
        let local = self.local_seconds(unix_nanos.div_euclid(NANOS_PER_SECOND));
        u16::try_from(local.rem_euclid(DAY_SECONDS) / 60).unwrap_or(0)
    }
}

impl WorkflowClockTime {
    /// The first instant strictly after `unix_nanos` at this local time.
    #[must_use]
    pub fn next_after(self, unix_nanos: i64) -> i64 {
        let local = self
            .zone
            .local_seconds(unix_nanos.div_euclid(NANOS_PER_SECOND));
        let day = local.div_euclid(DAY_SECONDS);
        let minute = i64::from(self.minute_of_day) * 60;
        // A daylight-saving change moves the local day by at most an hour, so the
        // occurrence is within the next two local days.
        (0..=2)
            .map(|offset| {
                self.zone
                    .unix_seconds((day + offset) * DAY_SECONDS + minute)
                    .saturating_mul(NANOS_PER_SECOND)
            })
            .find(|candidate| *candidate > unix_nanos)
            .unwrap_or_else(|| {
                self.zone
                    .unix_seconds((day + 3) * DAY_SECONDS + minute)
                    .saturating_mul(NANOS_PER_SECOND)
            })
    }
}

/// Whether the local minute at `unix_nanos` is inside the window; a window whose end is
/// earlier than its start spans midnight.
pub(crate) fn time_window_holds(
    zone: WorkflowTimeZone,
    start_minute: u16,
    end_minute: u16,
    unix_nanos: i64,
) -> bool {
    let minute = zone.minute_of_day(unix_nanos);
    if start_minute < end_minute {
        (start_minute..end_minute).contains(&minute)
    } else {
        minute >= start_minute || minute < end_minute
    }
}

impl PositionRequirement {
    pub(crate) const fn holds(self, signed_units: i64) -> bool {
        match self {
            Self::Flat => signed_units == 0,
            Self::Long => signed_units > 0,
            Self::Short => signed_units < 0,
            Self::Open => signed_units != 0,
        }
    }
}

impl WorkflowRecord {
    /// The record of a newly accepted revision: paused, keeping the firing counts and the
    /// history of the revisions before it.
    pub(crate) fn accepted(
        definition: WorkflowDefinition,
        layout: WorkflowLayout,
        previous: Option<&Self>,
    ) -> Self {
        Self {
            definition,
            layout,
            state: WorkflowState::Paused,
            session_start_unix_nanos: previous.map_or(0, |record| record.session_start_unix_nanos),
            session_firings: previous.map_or(0, |record| record.session_firings),
            total_firings: previous.map_or(0, |record| record.total_firings),
            last_sequence: previous.map_or(0, |record| record.last_sequence),
            last_attempt_unix_nanos: previous.and_then(|record| record.last_attempt_unix_nanos),
        }
    }

    /// Resets the per-session count when `now` is in a later risk session.
    pub(crate) fn roll_session(&mut self, boundary: RiskSessionBoundary, now: i64) {
        let start = boundary.session_start_containing(now);
        if start != self.session_start_unix_nanos {
            self.session_start_unix_nanos = start;
            self.session_firings = 0;
        }
    }

    /// A `Once` workflow has no session cap: its firing completes it instead.
    pub(crate) fn session_cap_reached(&self) -> bool {
        match self.definition.limits.repeat {
            WorkflowRepeat::Once | WorkflowRepeat::Always => false,
            WorkflowRepeat::PerSession { maximum } => self.session_firings >= maximum,
        }
    }

    pub(crate) fn cooling_down(&self, now: i64) -> bool {
        let cooldown = i64::from(self.definition.limits.cooldown_seconds) * NANOS_PER_SECOND;
        self.last_attempt_unix_nanos
            .is_some_and(|last| now < last.saturating_add(cooldown))
    }
}

/// The client order id of one action node of one firing. Derived from the firing so a
/// replay of the same firing can never send a second order.
pub(crate) fn workflow_client_order_id(
    workflow_id: &str,
    revision: u32,
    sequence: u64,
    node: WorkflowNodeId,
) -> Result<ClientOrderId, WorkflowError> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut write = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    write(workflow_id.as_bytes());
    write(&[0]);
    write(&revision.to_le_bytes());
    write(&sequence.to_le_bytes());
    write(&node.0.to_le_bytes());
    ClientOrderId::try_new(format!("wf-{hash:016x}"))
        .map_err(|error| WorkflowError::Owner(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn nanos(rfc3339: &str) -> i64 {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("timestamp")
            .timestamp_nanos_opt()
            .expect("in range")
    }

    const IF_FLAT: WorkflowNodeId = WorkflowNodeId(1);
    const BUY: WorkflowNodeId = WorkflowNodeId(2);
    const FLATTEN: WorkflowNodeId = WorkflowNodeId(3);

    fn enter() -> WorkflowAction {
        WorkflowAction::Enter {
            side: OrderSide::Buy,
            quantity: FixedPoint::try_new(1, 0).expect("quantity"),
            protection: Some(WorkflowProtection {
                stop_ticks: 8,
                target_ticks: 16,
            }),
        }
    }

    /// If flat, buy; otherwise flatten.
    fn definition() -> WorkflowDefinition {
        WorkflowDefinition {
            workflow_id: "wf-1".into(),
            revision: 1,
            name: "Breakout".into(),
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            scope: WorkflowScope::Instrument(
                InstrumentId::try_new("instrument:fixture:CME:ESZ6").expect("id"),
            ),
            triggers: vec![WorkflowTrigger::PriceCrosses {
                direction: PriceDirection::Above,
                level: FixedPoint::try_new(500_000, 2).expect("level"),
            }],
            start: IF_FLAT,
            nodes: vec![
                WorkflowNode {
                    id: IF_FLAT,
                    step: WorkflowStep::If {
                        condition: WorkflowCondition::single(WorkflowCheck::Position(
                            PositionRequirement::Flat,
                        )),
                        then: Some(BUY),
                        otherwise: Some(FLATTEN),
                    },
                },
                WorkflowNode {
                    id: BUY,
                    step: WorkflowStep::Action {
                        action: enter(),
                        next: None,
                    },
                },
                WorkflowNode {
                    id: FLATTEN,
                    step: WorkflowStep::Action {
                        action: WorkflowAction::Flatten,
                        next: None,
                    },
                },
            ],
            limits: WorkflowLimits {
                repeat: WorkflowRepeat::Once,
                cooldown_seconds: 0,
                expires_unix_nanos: None,
            },
        }
    }

    fn set_step(definition: &mut WorkflowDefinition, id: WorkflowNodeId, step: WorkflowStep) {
        definition
            .nodes
            .iter_mut()
            .find(|node| node.id == id)
            .expect("node")
            .step = step;
    }

    #[test]
    fn a_definition_validates_every_block() {
        assert_eq!(definition().validate(), Ok(()));
        let mut bad = definition();
        bad.workflow_id = "Bad Id".into();
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Identity));
        let mut bad = definition();
        bad.limits.repeat = WorkflowRepeat::PerSession { maximum: 51 };
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Repeat));
        let mut bad = definition();
        bad.triggers = vec![WorkflowTrigger::TimeOfDay(WorkflowClockTime {
            zone: WorkflowTimeZone::Utc,
            minute_of_day: MINUTES_PER_DAY,
        })];
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Time));
        let mut bad = definition();
        bad.triggers = vec![WorkflowTrigger::Every { minutes: 0 }];
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Time));
        let mut bad = definition();
        bad.triggers.clear();
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Triggers));
        let mut bad = definition();
        bad.triggers = vec![WorkflowTrigger::Every { minutes: 5 }; MAXIMUM_WORKFLOW_TRIGGERS + 1];
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Triggers));
        // A lock may lead on to a flatten or a notice.
        let mut locked = definition();
        set_step(
            &mut locked,
            BUY,
            WorkflowStep::Action {
                action: WorkflowAction::LockAccount,
                next: Some(FLATTEN),
            },
        );
        assert_eq!(locked.validate(), Ok(()));
        let mut always = definition();
        always.limits.repeat = WorkflowRepeat::Always;
        assert_eq!(always.validate(), Ok(()));
        let mut bad = definition();
        set_step(
            &mut bad,
            IF_FLAT,
            WorkflowStep::If {
                condition: WorkflowCondition::single(WorkflowCheck::Price {
                    direction: PriceDirection::Below,
                    level: FixedPoint::try_new(0, 2).expect("level"),
                }),
                then: Some(BUY),
                otherwise: Some(FLATTEN),
            },
        );
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Level));
        let range = |low, high| {
            let mut bad = definition();
            set_step(
                &mut bad,
                IF_FLAT,
                WorkflowStep::If {
                    condition: WorkflowCondition::single(WorkflowCheck::PriceRange {
                        low: FixedPoint::try_new(low, 2).expect("low"),
                        high: FixedPoint::try_new(high, 2).expect("high"),
                    }),
                    then: Some(BUY),
                    otherwise: Some(FLATTEN),
                },
            );
            bad.validate()
        };
        assert_eq!(range(100, 200), Ok(()));
        assert_eq!(range(200, 200), Err(WorkflowInvalid::Level));
        assert_eq!(range(0, 200), Err(WorkflowInvalid::Level));
    }

    #[test]
    fn trade_checks_stay_within_their_bounds() {
        let check = |check| {
            let mut bad = definition();
            set_step(
                &mut bad,
                IF_FLAT,
                WorkflowStep::If {
                    condition: WorkflowCondition::single(check),
                    then: Some(BUY),
                    otherwise: Some(FLATTEN),
                },
            );
            bad.validate()
        };
        let compare = WorkflowCompare::AtLeast;
        for (minutes, expected) in [
            (0, Err(WorkflowInvalid::Time)),
            (1, Ok(())),
            (MAXIMUM_WORKFLOW_TRADE_MINUTES, Ok(())),
            (
                MAXIMUM_WORKFLOW_TRADE_MINUTES + 1,
                Err(WorkflowInvalid::Time),
            ),
        ] {
            assert_eq!(
                check(WorkflowCheck::TimeInTrade { compare, minutes }),
                expected
            );
        }
        for (ticks, expected) in [
            (0, Err(WorkflowInvalid::Ticks)),
            (-8, Ok(())),
            (10_001, Err(WorkflowInvalid::Ticks)),
        ] {
            assert_eq!(
                check(WorkflowCheck::OpenProfitTicks { compare, ticks }),
                expected
            );
        }
        assert_eq!(
            check(WorkflowCheck::Pullback { ticks: 0 }),
            Err(WorkflowInvalid::Ticks)
        );
        assert_eq!(check(WorkflowCheck::Pullback { ticks: 4 }), Ok(()));
    }

    #[test]
    fn a_condition_joins_its_checks_by_all_or_any() {
        let flat = WorkflowCheck::Position(PositionRequirement::Flat);
        let long = WorkflowCheck::Position(PositionRequirement::Long);
        let holds = |check| Ok(check == flat);
        let all = WorkflowCondition::joined(WorkflowJoin::All, &[flat, long]).expect("all");
        let any = WorkflowCondition::joined(WorkflowJoin::Any, &[flat, long]).expect("any");
        assert_eq!(all.holds(holds), Ok(false));
        assert_eq!(any.holds(holds), Ok(true));
        assert_eq!(all.checks().collect::<Vec<_>>(), vec![flat, long]);
        // A check it cannot read leaves the whole test unread.
        assert_eq!(
            any.holds(|check| if check == long {
                Ok(true)
            } else {
                Err(WorkflowBlocker::PositionUnknown)
            }),
            Err(WorkflowBlocker::PositionUnknown)
        );
        assert_eq!(WorkflowCondition::joined(WorkflowJoin::All, &[]), None);
        assert_eq!(
            WorkflowCondition::joined(WorkflowJoin::Any, &[flat; 5]),
            None
        );
        assert_eq!(
            WorkflowCondition::joined(WorkflowJoin::All, &[flat]),
            Some(WorkflowCondition::single(flat))
        );
    }

    #[test]
    fn weekdays_follow_the_zone() {
        assert_eq!(WorkflowWeekdays::from_bits(0), None);
        assert_eq!(WorkflowWeekdays::from_bits(0x80), None);
        let weekdays = WorkflowWeekdays::WEEKDAYS;
        assert!(weekdays.contains(0) && weekdays.contains(4) && !weekdays.contains(5));
        // 2026-07-04 01:00 UTC is a Saturday in UTC and still Friday evening in Chicago.
        let at = nanos("2026-07-04T01:00:00Z");
        assert_eq!(WorkflowTimeZone::Utc.weekday(at), 5);
        assert_eq!(WorkflowTimeZone::Chicago.weekday(at), 4);
    }

    #[test]
    fn an_interval_trigger_counts_from_utc_midnight() {
        let every = |minutes| WorkflowTrigger::Every { minutes };
        assert_eq!(
            every(15).next_after(nanos("2026-07-01T09:07:30Z")),
            Some(nanos("2026-07-01T09:15:00Z"))
        );
        assert_eq!(
            every(15).next_after(nanos("2026-07-01T09:15:00Z")),
            Some(nanos("2026-07-01T09:30:00Z"))
        );
        // 7 does not divide the day, so the last period before midnight is shorter.
        assert_eq!(
            every(7).next_after(nanos("2026-07-01T23:58:00Z")),
            Some(nanos("2026-07-02T00:00:00Z"))
        );
        let mut definition = definition();
        definition.triggers.push(every(60));
        definition
            .triggers
            .push(WorkflowTrigger::TimeOfDay(WorkflowClockTime {
                zone: WorkflowTimeZone::Utc,
                minute_of_day: 9 * 60 + 30,
            }));
        assert_eq!(definition.validate(), Ok(()));
        assert_eq!(
            definition.next_timed_trigger_after(nanos("2026-07-01T09:07:00Z")),
            Some(nanos("2026-07-01T09:30:00Z"))
        );
    }

    #[test]
    fn the_graph_must_be_bounded_linked_acyclic_and_reachable() {
        let mut bad = definition();
        bad.nodes.clear();
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Nodes));
        let mut bad = definition();
        bad.nodes[2].id = BUY;
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Nodes));
        let mut bad = definition();
        bad.nodes[0].id = WorkflowNodeId::TRIGGER;
        bad.start = WorkflowNodeId::TRIGGER;
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Nodes));
        let mut bad = definition();
        bad.start = WorkflowNodeId(9);
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Link));
        let mut bad = definition();
        set_step(
            &mut bad,
            BUY,
            WorkflowStep::Action {
                action: enter(),
                next: Some(BUY),
            },
        );
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Link));
        let mut bad = definition();
        set_step(
            &mut bad,
            FLATTEN,
            WorkflowStep::Action {
                action: WorkflowAction::Flatten,
                next: Some(IF_FLAT),
            },
        );
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Cycle));
        let mut bad = definition();
        bad.start = BUY;
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Unreachable));
        let mut only_ifs = definition();
        only_ifs.nodes.truncate(1);
        set_step(
            &mut only_ifs,
            IF_FLAT,
            WorkflowStep::If {
                condition: WorkflowCondition::single(WorkflowCheck::Position(
                    PositionRequirement::Flat,
                )),
                then: None,
                otherwise: None,
            },
        );
        assert_eq!(only_ifs.validate(), Err(WorkflowInvalid::Actions));

        // Both branches may meet again at one node.
        let mut merged = definition();
        set_step(
            &mut merged,
            FLATTEN,
            WorkflowStep::Action {
                action: WorkflowAction::Flatten,
                next: Some(BUY),
            },
        );
        assert_eq!(merged.validate(), Ok(()));
    }

    #[test]
    fn a_run_follows_the_branch_its_condition_decides() {
        let definition = definition();
        let targets = definition.all_targets();
        assert_eq!(
            definition.resolve_path(targets.clone(), |_, _| Ok(true)),
            Ok(vec![
                WorkflowPathStep::Branch(IF_FLAT, true),
                WorkflowPathStep::Action(BUY, enter(), targets.clone()),
            ])
        );
        assert_eq!(
            definition.resolve_path(targets.clone(), |_, _| Ok(false)),
            Ok(vec![
                WorkflowPathStep::Branch(IF_FLAT, false),
                WorkflowPathStep::Action(FLATTEN, WorkflowAction::Flatten, targets.clone()),
            ])
        );
        assert_eq!(
            definition.resolve_path(targets, |_, _| Err(WorkflowBlocker::QuoteUnavailable)),
            Err(WorkflowBlocker::QuoteUnavailable)
        );
    }

    #[test]
    fn a_then_branch_goes_on_with_the_targets_its_if_narrowed() {
        let mut definition = definition();
        definition.scope = WorkflowScope::AnySymbol;
        set_step(
            &mut definition,
            BUY,
            WorkflowStep::Action {
                action: WorkflowAction::CancelOrders,
                next: None,
            },
        );
        let passed = WorkflowTargets::Only(BTreeSet::from([InstrumentId::try_new(
            "instrument:fixture:CME:NQZ6",
        )
        .expect("id")]));
        let narrowed = passed.clone();
        assert_eq!(
            definition.resolve_path(WorkflowTargets::Every, move |_, targets| {
                *targets = narrowed.clone();
                Ok(true)
            }),
            Ok(vec![
                WorkflowPathStep::Branch(IF_FLAT, true),
                WorkflowPathStep::Action(BUY, WorkflowAction::CancelOrders, passed),
            ])
        );
    }

    #[test]
    fn a_workflow_on_any_symbol_refuses_blocks_that_need_one() {
        let mut any = definition();
        any.scope = WorkflowScope::AnySymbol;
        // The price trigger and the entry each need one symbol.
        assert_eq!(any.validate(), Err(WorkflowInvalid::Scope));
        any.triggers = vec![WorkflowTrigger::Every { minutes: 5 }];
        assert_eq!(any.validate(), Err(WorkflowInvalid::Scope));
        set_step(
            &mut any,
            BUY,
            WorkflowStep::Action {
                action: WorkflowAction::CancelOrders,
                next: None,
            },
        );
        assert_eq!(any.validate(), Ok(()));
        set_step(
            &mut any,
            IF_FLAT,
            WorkflowStep::If {
                condition: WorkflowCondition::single(WorkflowCheck::Price {
                    direction: PriceDirection::Above,
                    level: FixedPoint::try_new(500_000, 2).expect("level"),
                }),
                then: Some(BUY),
                otherwise: Some(FLATTEN),
            },
        );
        assert_eq!(any.validate(), Err(WorkflowInvalid::Scope));
    }

    #[test]
    fn a_layout_names_only_known_nodes_within_the_board() {
        let definition = definition();
        let at = |node, x, y| WorkflowNodePosition { node, x, y };
        let layout = WorkflowLayout {
            positions: vec![at(WorkflowNodeId::TRIGGER, 0, 0), at(BUY, 240, -80)],
        };
        assert_eq!(layout.validate(&definition), Ok(()));
        for positions in [
            vec![at(WorkflowNodeId(7), 0, 0)],
            vec![at(BUY, 0, 0), at(BUY, 10, 10)],
            vec![at(BUY, MAXIMUM_WORKFLOW_LAYOUT_COORDINATE + 1, 0)],
        ] {
            assert_eq!(
                WorkflowLayout { positions }.validate(&definition),
                Err(WorkflowInvalid::Layout)
            );
        }
    }

    #[test]
    fn a_condition_trigger_is_validated_and_read_like_an_if() {
        let day_profit = WorkflowCheck::DayProfit {
            compare: WorkflowCompare::AtLeast,
            amount: FixedPoint::try_new(50_000, 2).expect("amount"),
        };
        let mut definition = definition();
        definition.triggers = vec![WorkflowTrigger::ConditionMet(WorkflowCondition::single(
            day_profit,
        ))];
        assert_eq!(definition.validate(), Ok(()));
        assert_eq!(definition.next_timed_trigger_after(0), None);
        // Its checks count with the If's, for scales, quotes and the scope.
        assert!(definition.checks().any(|check| check == day_profit));
        assert_eq!(definition.trigger_conditions().count(), 1);
        definition.triggers = vec![WorkflowTrigger::ConditionMet(WorkflowCondition::single(
            WorkflowCheck::Pullback { ticks: 0 },
        ))];
        assert_eq!(definition.validate(), Err(WorkflowInvalid::Ticks));
        let mut any = definition.clone();
        any.scope = WorkflowScope::AnySymbol;
        set_step(
            &mut any,
            BUY,
            WorkflowStep::Action {
                action: WorkflowAction::Notify,
                next: None,
            },
        );
        any.triggers = vec![WorkflowTrigger::ConditionMet(WorkflowCondition::single(
            WorkflowCheck::Price {
                direction: PriceDirection::Above,
                level: FixedPoint::try_new(500_000, 2).expect("level"),
            },
        ))];
        assert_eq!(any.validate(), Err(WorkflowInvalid::Scope));
        any.triggers = vec![WorkflowTrigger::ConditionMet(WorkflowCondition::single(
            day_profit,
        ))];
        assert_eq!(any.validate(), Ok(()));
        assert!(!day_profit.reads_position() && !day_profit.reads_quote());
        assert!(
            WorkflowCheck::Between {
                zone: WorkflowTimeZone::Utc,
                start_minute: 0,
                end_minute: 60,
            }
            .reads_clock()
        );
    }

    #[test]
    fn chicago_times_follow_daylight_saving() {
        let open = WorkflowClockTime {
            zone: WorkflowTimeZone::Chicago,
            minute_of_day: 8 * 60 + 30,
        };
        // July: daylight time, UTC-5.
        assert_eq!(
            open.next_after(nanos("2026-07-01T12:00:00Z")),
            nanos("2026-07-01T13:30:00Z")
        );
        assert_eq!(
            open.next_after(nanos("2026-07-01T13:30:00Z")),
            nanos("2026-07-02T13:30:00Z")
        );
        // January: standard time, UTC-6.
        assert_eq!(
            open.next_after(nanos("2026-01-15T00:00:00Z")),
            nanos("2026-01-15T14:30:00Z")
        );
        // Across the 2026-11-01 fall back.
        assert_eq!(
            open.next_after(nanos("2026-10-31T14:00:00Z")),
            nanos("2026-11-01T14:30:00Z")
        );
        assert_eq!(
            WorkflowTimeZone::Chicago.minute_of_day(nanos("2026-07-01T13:30:00Z")),
            8 * 60 + 30
        );
    }

    #[test]
    fn a_time_window_can_span_midnight() {
        let holds = |at| time_window_holds(WorkflowTimeZone::Utc, 22 * 60, 2 * 60, nanos(at));
        assert!(holds("2026-07-01T23:00:00Z"));
        assert!(holds("2026-07-01T01:59:00Z"));
        assert!(!holds("2026-07-01T02:00:00Z"));
    }

    #[test]
    fn client_order_ids_are_stable_per_firing_node() {
        let id = |revision, sequence, node| {
            workflow_client_order_id("wf-1", revision, sequence, WorkflowNodeId(node)).expect("id")
        };
        let first = id(1, 1, 2);
        assert_eq!(first, id(1, 1, 2));
        assert_ne!(first, id(1, 2, 2));
        assert_ne!(first, id(1, 1, 3));
        assert_ne!(first, id(2, 1, 2));
    }
}
