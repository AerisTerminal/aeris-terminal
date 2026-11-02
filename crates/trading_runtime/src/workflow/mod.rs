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
/// The most firings one workflow may make in one risk session.
pub const MAXIMUM_WORKFLOW_SESSION_FIRINGS: u32 = 50;
pub const MAXIMUM_WORKFLOW_COOLDOWN_SECONDS: u32 = 86_400;
pub const MAXIMUM_WORKFLOW_PROTECTION_TICKS: u32 = 10_000;
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
    pub instrument_id: InstrumentId,
    pub trigger: WorkflowTrigger,
    /// The node the trigger leads to.
    pub start: WorkflowNodeId,
    /// Every If and action node; each is reachable from `start` and none is reached twice
    /// on one run because the graph has no cycle.
    pub nodes: Vec<WorkflowNode>,
    pub limits: WorkflowLimits,
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

/// Every condition is read from the owner's state at the moment the trigger fires, before
/// any action of that run, so an order a run places never changes which branch it took.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowCondition {
    /// The account's position in the workflow instrument.
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
}

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
    /// Closes the account's position in the workflow instrument.
    Flatten,
    /// Cancels the account's working orders in the workflow instrument.
    CancelOrders,
    /// Locks the account against new orders until the user unlocks it.
    LockAccount,
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
    /// No node, too many, a repeated identity or the trigger's identity on a node.
    Nodes,
    /// A link to a node that does not exist or to the node itself.
    Link,
    Cycle,
    /// A node the trigger never reaches.
    Unreachable,
    /// No action, or a lock with a node after it.
    Actions,
    Quantity,
    Protection,
    Repeat,
    Cooldown,
    Expiry,
    Layout,
}

/// One node a run passes, decided before any action runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkflowPathStep {
    Branch(WorkflowNodeId, bool),
    Action(WorkflowNodeId, WorkflowAction),
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

    /// Every condition anywhere in the graph.
    pub fn conditions(&self) -> impl Iterator<Item = WorkflowCondition> + '_ {
        self.nodes.iter().filter_map(|node| match node.step {
            WorkflowStep::If { condition, .. } => Some(condition),
            WorkflowStep::Action { .. } => None,
        })
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
        match self.trigger {
            WorkflowTrigger::PriceCrosses { level, .. } => validate_level(level)?,
            WorkflowTrigger::TimeOfDay(time) => validate_minute(time.minute_of_day)?,
        }
        self.validate_graph()?;
        for node in &self.nodes {
            match node.step {
                WorkflowStep::If { condition, .. } => validate_condition(condition)?,
                WorkflowStep::Action { action, next } => validate_action(action, next)?,
            }
        }
        match self.limits.repeat {
            WorkflowRepeat::Once => {}
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

    /// The nodes one run passes from `start`, deciding every If with `holds`. `None` when a
    /// condition cannot be read (a price condition without a current quote).
    pub(crate) fn resolve_path(
        &self,
        mut holds: impl FnMut(WorkflowCondition) -> Option<bool>,
    ) -> Option<Vec<WorkflowPathStep>> {
        let mut path = Vec::new();
        let mut next = Some(self.start);
        // An acyclic graph visits each node at most once; the bound guards a corrupt one.
        while let Some(id) = next.take() {
            if path.len() >= self.nodes.len() {
                break;
            }
            let node = self.node(id)?;
            match node.step {
                WorkflowStep::If {
                    condition,
                    then,
                    otherwise,
                } => {
                    let held = holds(condition)?;
                    path.push(WorkflowPathStep::Branch(id, held));
                    next = if held { then } else { otherwise };
                }
                WorkflowStep::Action {
                    action,
                    next: after,
                } => {
                    path.push(WorkflowPathStep::Action(id, action));
                    next = after;
                }
            }
        }
        Some(path)
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

fn validate_condition(condition: WorkflowCondition) -> Result<(), WorkflowInvalid> {
    match condition {
        WorkflowCondition::Position(_) => Ok(()),
        WorkflowCondition::Between {
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
        WorkflowCondition::Price { level, .. } => validate_level(level),
    }
}

fn validate_action(
    action: WorkflowAction,
    next: Option<WorkflowNodeId>,
) -> Result<(), WorkflowInvalid> {
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
        // A locked account refuses every later order, so nothing follows a lock.
        WorkflowAction::LockAccount if next.is_some() => Err(WorkflowInvalid::Actions),
        WorkflowAction::LockAccount | WorkflowAction::Flatten | WorkflowAction::CancelOrders => {
            Ok(())
        }
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
            WorkflowRepeat::Once => false,
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
            instrument_id: InstrumentId::try_new("instrument:fixture:CME:ESZ6").expect("id"),
            trigger: WorkflowTrigger::PriceCrosses {
                direction: PriceDirection::Above,
                level: FixedPoint::try_new(500_000, 2).expect("level"),
            },
            start: IF_FLAT,
            nodes: vec![
                WorkflowNode {
                    id: IF_FLAT,
                    step: WorkflowStep::If {
                        condition: WorkflowCondition::Position(PositionRequirement::Flat),
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
        bad.trigger = WorkflowTrigger::TimeOfDay(WorkflowClockTime {
            zone: WorkflowTimeZone::Utc,
            minute_of_day: MINUTES_PER_DAY,
        });
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Time));
        let mut bad = definition();
        set_step(
            &mut bad,
            BUY,
            WorkflowStep::Action {
                action: WorkflowAction::LockAccount,
                next: Some(FLATTEN),
            },
        );
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Actions));
        let mut bad = definition();
        set_step(
            &mut bad,
            IF_FLAT,
            WorkflowStep::If {
                condition: WorkflowCondition::Price {
                    direction: PriceDirection::Below,
                    level: FixedPoint::try_new(0, 2).expect("level"),
                },
                then: Some(BUY),
                otherwise: Some(FLATTEN),
            },
        );
        assert_eq!(bad.validate(), Err(WorkflowInvalid::Level));
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
                condition: WorkflowCondition::Position(PositionRequirement::Flat),
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
        assert_eq!(
            definition.resolve_path(|_| Some(true)),
            Some(vec![
                WorkflowPathStep::Branch(IF_FLAT, true),
                WorkflowPathStep::Action(BUY, enter()),
            ])
        );
        assert_eq!(
            definition.resolve_path(|_| Some(false)),
            Some(vec![
                WorkflowPathStep::Branch(IF_FLAT, false),
                WorkflowPathStep::Action(FLATTEN, WorkflowAction::Flatten),
            ])
        );
        assert_eq!(definition.resolve_path(|_| None), None);
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
