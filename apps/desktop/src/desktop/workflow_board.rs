//! The draft a workflow board edits: the trigger, the If and action blocks with their links
//! and board positions, and the text the user typed into each field. `build` turns it into
//! exactly the definition and layout the trading owner validates; the board never talks to
//! the owner itself.

use aeris_desktop::trading::OrderQuantity;
use aeris_instruments::InstrumentId;
use aeris_trading::{FixedPoint, OrderSide, TradingAccountId};
use aeris_trading_runtime::{
    MAXIMUM_WORKFLOW_COOLDOWN_SECONDS, MAXIMUM_WORKFLOW_LAYOUT_COORDINATE,
    MAXIMUM_WORKFLOW_NAME_CHARS, MAXIMUM_WORKFLOW_NODES, MAXIMUM_WORKFLOW_PROTECTION_TICKS,
    MAXIMUM_WORKFLOW_SESSION_FIRINGS, PositionRequirement, PriceDirection, WorkflowAction,
    WorkflowClockTime, WorkflowCondition, WorkflowDefinition, WorkflowLayout, WorkflowLimits,
    WorkflowNode, WorkflowNodeId, WorkflowNodePosition, WorkflowProtection, WorkflowRepeat,
    WorkflowStep, WorkflowTimeZone, WorkflowTrigger, WorkflowView,
};
use num_traits::ToPrimitive;
use std::collections::{BTreeMap, BTreeSet};

pub(super) const NODE_WIDTH: f32 = 216.0;
pub(super) const NODE_HEIGHT: f32 = 76.0;
const PORT_HIT_RADIUS: f32 = 12.0;
const COLUMN_SPACING: f32 = 288.0;
const ROW_SPACING: f32 = 112.0;
pub(super) const MINIMUM_ZOOM: f32 = 0.5;
pub(super) const MAXIMUM_ZOOM: f32 = 1.5;
const DEFAULT_SESSION_MAXIMUM: &str = "3";
const DEFAULT_COOLDOWN_SECONDS: &str = "60";
pub(super) const DEFAULT_QUANTITY: &str = "1";
const DEFAULT_TRIGGER_TIME: &str = "08:30";

/// A point in board coordinates, or in board-local screen pixels for [`BoardCamera`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct BoardPoint {
    pub(super) x: f32,
    pub(super) y: f32,
}

impl BoardPoint {
    pub(super) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Something the board draws as a block.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum BoardItem {
    Trigger,
    Node(WorkflowNodeId),
}

/// An output a link leaves from: the trigger's, an action's next, or an If's yes and no.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutputPort {
    Trigger,
    Next(WorkflowNodeId),
    Then(WorkflowNodeId),
    Otherwise(WorkflowNodeId),
}

impl OutputPort {
    pub(super) const fn item(self) -> BoardItem {
        match self {
            Self::Trigger => BoardItem::Trigger,
            Self::Next(id) | Self::Then(id) | Self::Otherwise(id) => BoardItem::Node(id),
        }
    }

    /// Where the port sits down the right edge of its block.
    pub(super) fn offset_y(self) -> f32 {
        match self {
            Self::Trigger | Self::Next(_) => NODE_HEIGHT / 2.0,
            Self::Then(_) => NODE_HEIGHT * 0.34,
            Self::Otherwise(_) => NODE_HEIGHT * 0.72,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BoardHit {
    Output(OutputPort),
    Item(BoardItem),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TriggerDraft {
    Price {
        direction: PriceDirection,
        level: String,
    },
    Time {
        zone: WorkflowTimeZone,
        time: String,
    },
}

/// The trigger kinds the inspector offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TriggerKind {
    Rises,
    Falls,
    Time,
}

impl TriggerDraft {
    pub(super) const fn kind(&self) -> TriggerKind {
        match self {
            Self::Price {
                direction: PriceDirection::Above,
                ..
            } => TriggerKind::Rises,
            Self::Price {
                direction: PriceDirection::Below,
                ..
            } => TriggerKind::Falls,
            Self::Time { .. } => TriggerKind::Time,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ConditionDraft {
    Position(PositionRequirement),
    Between {
        zone: WorkflowTimeZone,
        start: String,
        end: String,
    },
    Price {
        direction: PriceDirection,
        level: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ActionDraft {
    Enter {
        side: OrderSide,
        quantity: String,
        stop: String,
        target: String,
    },
    Flatten,
    CancelOrders,
    LockAccount,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BlockDraft {
    If {
        condition: ConditionDraft,
        then: Option<WorkflowNodeId>,
        otherwise: Option<WorkflowNodeId>,
    },
    Action {
        action: ActionDraft,
        next: Option<WorkflowNodeId>,
    },
}

impl BlockDraft {
    pub(super) const fn kind(&self) -> BlockKind {
        match self {
            Self::If { condition, .. } => match condition {
                ConditionDraft::Position(_) => BlockKind::IfPosition,
                ConditionDraft::Between { .. } => BlockKind::IfTime,
                ConditionDraft::Price { .. } => BlockKind::IfPrice,
            },
            Self::Action { action, .. } => match action {
                ActionDraft::Enter {
                    side: OrderSide::Buy,
                    ..
                } => BlockKind::Buy,
                ActionDraft::Enter {
                    side: OrderSide::Sell,
                    ..
                } => BlockKind::Sell,
                ActionDraft::Flatten => BlockKind::Flatten,
                ActionDraft::CancelOrders => BlockKind::CancelOrders,
                ActionDraft::LockAccount => BlockKind::LockAccount,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct NodeDraft {
    pub(super) id: WorkflowNodeId,
    pub(super) at: BoardPoint,
    pub(super) block: BlockDraft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RepeatDraft {
    Once,
    PerSession,
}

/// The blocks the palette adds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BlockKind {
    IfPosition,
    IfTime,
    IfPrice,
    Buy,
    Sell,
    Flatten,
    CancelOrders,
    LockAccount,
}

impl BlockKind {
    pub(super) const CHECKS: [Self; 3] = [Self::IfPosition, Self::IfTime, Self::IfPrice];
    pub(super) const ACTIONS: [Self; 5] = [
        Self::Buy,
        Self::Sell,
        Self::Flatten,
        Self::CancelOrders,
        Self::LockAccount,
    ];
    #[cfg(test)]
    const ALL: [Self; 8] = [
        Self::IfPosition,
        Self::IfTime,
        Self::IfPrice,
        Self::Buy,
        Self::Sell,
        Self::Flatten,
        Self::CancelOrders,
        Self::LockAccount,
    ];

    fn block(self) -> BlockDraft {
        let condition = |condition| BlockDraft::If {
            condition,
            then: None,
            otherwise: None,
        };
        let action = |action| BlockDraft::Action { action, next: None };
        let enter = |side| ActionDraft::Enter {
            side,
            quantity: DEFAULT_QUANTITY.to_string(),
            stop: String::new(),
            target: String::new(),
        };
        match self {
            Self::IfPosition => condition(ConditionDraft::Position(PositionRequirement::Flat)),
            Self::IfTime => condition(ConditionDraft::Between {
                zone: WorkflowTimeZone::Chicago,
                start: "08:30".to_string(),
                end: "15:00".to_string(),
            }),
            Self::IfPrice => condition(ConditionDraft::Price {
                direction: PriceDirection::Above,
                level: String::new(),
            }),
            Self::Buy => action(enter(OrderSide::Buy)),
            Self::Sell => action(enter(OrderSide::Sell)),
            Self::Flatten => action(ActionDraft::Flatten),
            Self::CancelOrders => action(ActionDraft::CancelOrders),
            Self::LockAccount => action(ActionDraft::LockAccount),
        }
    }
}

/// A text field of the draft, which an inspector input edits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DraftField {
    Name,
    TriggerLevel,
    TriggerTime,
    SessionMaximum,
    Cooldown,
    Level(WorkflowNodeId),
    WindowStart(WorkflowNodeId),
    WindowEnd(WorkflowNodeId),
    Quantity(WorkflowNodeId),
    StopTicks(WorkflowNodeId),
    TargetTicks(WorkflowNodeId),
}

/// The accepted revision a draft was opened from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SavedRevision {
    pub(super) workflow_id: String,
    pub(super) revision: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct InstrumentScales {
    pub(super) price: u8,
    pub(super) quantity: u8,
}

/// The first part of a draft that does not yet make a valid workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DraftProblem {
    Name,
    Account,
    Symbol,
    TriggerLevel { decimals: u8 },
    TriggerTime,
    NoStart,
    Unconnected(WorkflowNodeId),
    NoAction,
    Level { node: WorkflowNodeId, decimals: u8 },
    Window(WorkflowNodeId),
    Quantity(WorkflowNodeId),
    Protection(WorkflowNodeId),
    SessionMaximum,
    Cooldown,
}

impl DraftProblem {
    /// The block the problem belongs to, for the board to mark.
    pub(super) const fn item(self) -> Option<BoardItem> {
        match self {
            Self::TriggerLevel { .. } | Self::TriggerTime | Self::NoStart => {
                Some(BoardItem::Trigger)
            }
            Self::Unconnected(node)
            | Self::Level { node, .. }
            | Self::Window(node)
            | Self::Quantity(node)
            | Self::Protection(node) => Some(BoardItem::Node(node)),
            Self::Name
            | Self::Account
            | Self::Symbol
            | Self::NoAction
            | Self::SessionMaximum
            | Self::Cooldown => None,
        }
    }
}

/// Why a link was not made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LinkRefusal {
    /// The link would let a run come back to a block it already passed.
    Loop,
    Invalid,
}

/// Where the board is looking: board-local pixels of the board origin, and the zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BoardCamera {
    pub(super) pan: BoardPoint,
    pub(super) zoom: f32,
}

impl Default for BoardCamera {
    fn default() -> Self {
        Self {
            pan: BoardPoint::new(48.0, 64.0),
            zoom: 1.0,
        }
    }
}

impl BoardCamera {
    pub(super) fn to_board(self, screen: BoardPoint) -> BoardPoint {
        BoardPoint::new(
            (screen.x - self.pan.x) / self.zoom,
            (screen.y - self.pan.y) / self.zoom,
        )
    }

    pub(super) fn to_screen(self, board: BoardPoint) -> BoardPoint {
        BoardPoint::new(
            board.x * self.zoom + self.pan.x,
            board.y * self.zoom + self.pan.y,
        )
    }

    /// Zooms by `factor`, keeping the board point under `screen` in place.
    pub(super) fn zoom_at(&mut self, screen: BoardPoint, factor: f32) {
        let anchor = self.to_board(screen);
        self.zoom = (self.zoom * factor).clamp(MINIMUM_ZOOM, MAXIMUM_ZOOM);
        self.pan = BoardPoint::new(
            screen.x - anchor.x * self.zoom,
            screen.y - anchor.y * self.zoom,
        );
    }

    pub(super) fn pan_by(&mut self, dx: f32, dy: f32) {
        self.pan = BoardPoint::new(self.pan.x + dx, self.pan.y + dy);
    }
}

/// A workflow as the board edits it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WorkflowDraft {
    pub(super) saved: Option<SavedRevision>,
    pub(super) name: String,
    pub(super) account_id: Option<TradingAccountId>,
    pub(super) instrument_id: Option<InstrumentId>,
    pub(super) trigger: TriggerDraft,
    pub(super) trigger_at: BoardPoint,
    pub(super) start: Option<WorkflowNodeId>,
    pub(super) nodes: Vec<NodeDraft>,
    pub(super) repeat: RepeatDraft,
    pub(super) session_maximum: String,
    pub(super) cooldown: String,
}

impl WorkflowDraft {
    /// A new workflow: a price trigger and nothing after it yet.
    pub(super) fn new(
        name: String,
        account_id: Option<TradingAccountId>,
        instrument_id: Option<InstrumentId>,
    ) -> Self {
        Self {
            saved: None,
            name,
            account_id,
            instrument_id,
            trigger: TriggerDraft::Price {
                direction: PriceDirection::Above,
                level: String::new(),
            },
            trigger_at: BoardPoint::default(),
            start: None,
            nodes: Vec::new(),
            repeat: RepeatDraft::Once,
            session_maximum: DEFAULT_SESSION_MAXIMUM.to_string(),
            cooldown: DEFAULT_COOLDOWN_SECONDS.to_string(),
        }
    }

    /// The accepted revision as editable text, with its saved block positions; blocks the
    /// layout does not place are arranged by depth.
    pub(super) fn from_view(view: &WorkflowView) -> Self {
        let definition = &view.definition;
        let positions = view
            .layout
            .positions
            .iter()
            .map(|position| {
                (
                    position.node,
                    BoardPoint::new(board_coordinate(position.x), board_coordinate(position.y)),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let (repeat, session_maximum) = match definition.limits.repeat {
            WorkflowRepeat::Once => (RepeatDraft::Once, DEFAULT_SESSION_MAXIMUM.to_string()),
            WorkflowRepeat::PerSession { maximum } => {
                (RepeatDraft::PerSession, maximum.to_string())
            }
        };
        let mut draft = Self {
            saved: Some(SavedRevision {
                workflow_id: definition.workflow_id.clone(),
                revision: definition.revision,
            }),
            name: definition.name.clone(),
            account_id: Some(definition.account_id.clone()),
            instrument_id: Some(definition.instrument_id.clone()),
            trigger: trigger_draft(definition.trigger),
            trigger_at: BoardPoint::default(),
            start: Some(definition.start),
            nodes: definition
                .nodes
                .iter()
                .map(|node| NodeDraft {
                    id: node.id,
                    at: BoardPoint::default(),
                    block: block_draft(node.step),
                })
                .collect(),
            repeat,
            session_maximum,
            cooldown: definition.limits.cooldown_seconds.to_string(),
        };
        draft.arrange(&positions);
        draft
    }

    /// Places every item at its saved position, and each one without a position in a
    /// column by its distance from the trigger.
    fn arrange(&mut self, positions: &BTreeMap<WorkflowNodeId, BoardPoint>) {
        let depths = self.depths();
        let mut rows = BTreeMap::<usize, f32>::new();
        let mut place = |id: WorkflowNodeId| {
            if let Some(at) = positions.get(&id) {
                return *at;
            }
            let depth = depths.get(&id).copied().unwrap_or(0);
            let row = rows.entry(depth).or_insert(0.0);
            let at = BoardPoint::new(depth.to_f32().unwrap_or(0.0) * COLUMN_SPACING, *row);
            *row += ROW_SPACING;
            at
        };
        self.trigger_at = place(WorkflowNodeId::TRIGGER);
        for node in &mut self.nodes {
            node.at = place(node.id);
        }
    }

    /// The fewest links from the trigger to each item.
    fn depths(&self) -> BTreeMap<WorkflowNodeId, usize> {
        let mut depths = BTreeMap::from([(WorkflowNodeId::TRIGGER, 0)]);
        let mut frontier = self.start.into_iter().collect::<Vec<_>>();
        let mut depth = 1;
        while !frontier.is_empty() && depth <= self.nodes.len() {
            let mut next = Vec::new();
            for id in frontier {
                if depths.contains_key(&id) {
                    continue;
                }
                depths.insert(id, depth);
                if let Some(node) = self.node(id) {
                    next.extend(node.links());
                }
            }
            frontier = next;
            depth += 1;
        }
        depths
    }

    pub(super) fn node(&self, id: WorkflowNodeId) -> Option<&NodeDraft> {
        self.nodes.iter().find(|node| node.id == id)
    }

    fn node_mut(&mut self, id: WorkflowNodeId) -> Option<&mut NodeDraft> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    pub(super) fn position(&self, item: BoardItem) -> Option<BoardPoint> {
        match item {
            BoardItem::Trigger => Some(self.trigger_at),
            BoardItem::Node(id) => self.node(id).map(|node| node.at),
        }
    }

    /// The check of an If block, for the inspector to edit.
    pub(super) fn condition_mut(&mut self, id: WorkflowNodeId) -> Option<&mut ConditionDraft> {
        match &mut self.node_mut(id)?.block {
            BlockDraft::If { condition, .. } => Some(condition),
            BlockDraft::Action { .. } => None,
        }
    }

    /// Switches the trigger between a price crossing up, down, and a time of day; a crossing
    /// that only changes direction keeps its level.
    pub(super) fn set_trigger_kind(&mut self, kind: TriggerKind) {
        let direction = match kind {
            TriggerKind::Rises => PriceDirection::Above,
            TriggerKind::Falls => PriceDirection::Below,
            TriggerKind::Time => {
                if matches!(self.trigger, TriggerDraft::Price { .. }) {
                    self.trigger = TriggerDraft::Time {
                        zone: WorkflowTimeZone::Chicago,
                        time: DEFAULT_TRIGGER_TIME.to_string(),
                    };
                }
                return;
            }
        };
        match &mut self.trigger {
            TriggerDraft::Price {
                direction: current, ..
            } => *current = direction,
            TriggerDraft::Time { .. } => {
                self.trigger = TriggerDraft::Price {
                    direction,
                    level: String::new(),
                };
            }
        }
    }

    /// Moves an item, keeping it inside the board bound the owner accepts.
    pub(super) fn move_item(&mut self, item: BoardItem, at: BoardPoint) {
        let bound = board_coordinate(MAXIMUM_WORKFLOW_LAYOUT_COORDINATE);
        let at = BoardPoint::new(at.x.clamp(-bound, bound), at.y.clamp(-bound, bound));
        match item {
            BoardItem::Trigger => self.trigger_at = at,
            BoardItem::Node(id) => {
                if let Some(node) = self.node_mut(id) {
                    node.at = at;
                }
            }
        }
    }

    /// Every output port the board draws, the trigger's first.
    pub(super) fn output_ports(&self) -> Vec<OutputPort> {
        std::iter::once(OutputPort::Trigger)
            .chain(self.nodes.iter().flat_map(NodeDraft::output_ports))
            .collect()
    }

    pub(super) fn link(&self, port: OutputPort) -> Option<WorkflowNodeId> {
        match port {
            OutputPort::Trigger => self.start,
            OutputPort::Next(id) => match self.node(id)?.block {
                BlockDraft::Action { next, .. } => next,
                BlockDraft::If { .. } => None,
            },
            OutputPort::Then(id) => match self.node(id)?.block {
                BlockDraft::If { then, .. } => then,
                BlockDraft::Action { .. } => None,
            },
            OutputPort::Otherwise(id) => match self.node(id)?.block {
                BlockDraft::If { otherwise, .. } => otherwise,
                BlockDraft::Action { .. } => None,
            },
        }
    }

    /// Every link, as its output port and the block it leads to.
    pub(super) fn links(&self) -> Vec<(OutputPort, WorkflowNodeId)> {
        self.output_ports()
            .into_iter()
            .filter_map(|port| Some((port, self.link(port)?)))
            .collect()
    }

    fn set_link(&mut self, port: OutputPort, target: Option<WorkflowNodeId>) {
        let slot = match port {
            OutputPort::Trigger => Some(&mut self.start),
            OutputPort::Next(id) => match self.node_mut(id).map(|node| &mut node.block) {
                Some(BlockDraft::Action { action, next })
                    if *action != ActionDraft::LockAccount =>
                {
                    Some(next)
                }
                _ => None,
            },
            OutputPort::Then(id) => match self.node_mut(id).map(|node| &mut node.block) {
                Some(BlockDraft::If { then, .. }) => Some(then),
                _ => None,
            },
            OutputPort::Otherwise(id) => match self.node_mut(id).map(|node| &mut node.block) {
                Some(BlockDraft::If { otherwise, .. }) => Some(otherwise),
                _ => None,
            },
        };
        if let Some(slot) = slot {
            *slot = target;
        }
    }

    /// Links `port` to `target`, replacing the port's previous link.
    pub(super) fn connect(
        &mut self,
        port: OutputPort,
        target: WorkflowNodeId,
    ) -> Result<(), LinkRefusal> {
        if self.node(target).is_none() || !self.output_ports().contains(&port) {
            return Err(LinkRefusal::Invalid);
        }
        if let BoardItem::Node(source) = port.item()
            && (source == target || self.reaches(target, source))
        {
            return Err(LinkRefusal::Loop);
        }
        self.set_link(port, Some(target));
        Ok(())
    }

    pub(super) fn disconnect(&mut self, port: OutputPort) {
        self.set_link(port, None);
    }

    /// Whether a run that reaches `from` can go on to reach `to`.
    fn reaches(&self, from: WorkflowNodeId, to: WorkflowNodeId) -> bool {
        self.reachable_from(from).contains(&to)
    }

    fn reachable_from(&self, start: WorkflowNodeId) -> BTreeSet<WorkflowNodeId> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(node) = self.node(id) {
                stack.extend(node.links());
            }
        }
        seen
    }

    /// Adds a block beside `after`, linked from its first free output, and returns its
    /// identity; `None` when the workflow already holds the most blocks.
    pub(super) fn add_block(
        &mut self,
        kind: BlockKind,
        after: Option<BoardItem>,
    ) -> Option<WorkflowNodeId> {
        if self.nodes.len() >= MAXIMUM_WORKFLOW_NODES {
            return None;
        }
        let used = self
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<BTreeSet<_>>();
        let id = (1..=u16::MAX)
            .map(WorkflowNodeId)
            .find(|id| !used.contains(id))?;
        let anchor = after
            .and_then(|item| self.position(item))
            .unwrap_or_else(|| self.rightmost());
        let at = self.free_spot(BoardPoint::new(anchor.x + COLUMN_SPACING, anchor.y));
        self.nodes.push(NodeDraft {
            id,
            at,
            block: kind.block(),
        });
        let free_output = after.and_then(|item| {
            self.output_ports()
                .into_iter()
                .find(|port| port.item() == item && self.link(*port).is_none())
        });
        if let Some(port) = free_output {
            self.set_link(port, Some(id));
        }
        Some(id)
    }

    fn rightmost(&self) -> BoardPoint {
        self.nodes
            .iter()
            .map(|node| node.at)
            .fold(self.trigger_at, |rightmost, at| {
                if at.x > rightmost.x { at } else { rightmost }
            })
    }

    /// The first spot at or below `at` that no block covers.
    fn free_spot(&self, at: BoardPoint) -> BoardPoint {
        let covered = |spot: BoardPoint| {
            std::iter::once(self.trigger_at)
                .chain(self.nodes.iter().map(|node| node.at))
                .any(|other| {
                    (other.x - spot.x).abs() < NODE_WIDTH && (other.y - spot.y).abs() < NODE_HEIGHT
                })
        };
        let mut spot = at;
        for _ in 0..=MAXIMUM_WORKFLOW_NODES {
            if !covered(spot) {
                break;
            }
            spot.y += ROW_SPACING;
        }
        spot
    }

    /// Changes what a block checks or does, keeping its links: an If stays an If and an
    /// action an action. A buy turned into a sell keeps its quantity and bracket, and a lock
    /// drops its next link because nothing runs after it.
    pub(super) fn change_block(&mut self, id: WorkflowNodeId, kind: BlockKind) {
        let Some(node) = self.node_mut(id) else {
            return;
        };
        if node.block.kind() == kind {
            return;
        }
        match (&mut node.block, kind.block()) {
            (
                BlockDraft::If { condition, .. },
                BlockDraft::If {
                    condition: replacement,
                    ..
                },
            ) => *condition = replacement,
            (
                BlockDraft::Action { action, next },
                BlockDraft::Action {
                    action: replacement,
                    ..
                },
            ) => {
                match (&mut *action, replacement) {
                    (
                        ActionDraft::Enter { side, .. },
                        ActionDraft::Enter {
                            side: replacement, ..
                        },
                    ) => *side = replacement,
                    (action, replacement) => *action = replacement,
                }
                if *action == ActionDraft::LockAccount {
                    *next = None;
                }
            }
            (BlockDraft::If { .. }, BlockDraft::Action { .. })
            | (BlockDraft::Action { .. }, BlockDraft::If { .. }) => {}
        }
    }

    /// Removes a block and every link to it.
    pub(super) fn remove_node(&mut self, id: WorkflowNodeId) {
        self.nodes.retain(|node| node.id != id);
        let unlink = |link: &mut Option<WorkflowNodeId>| {
            if *link == Some(id) {
                *link = None;
            }
        };
        unlink(&mut self.start);
        for node in &mut self.nodes {
            match &mut node.block {
                BlockDraft::If {
                    then, otherwise, ..
                } => {
                    unlink(then);
                    unlink(otherwise);
                }
                BlockDraft::Action { next, .. } => unlink(next),
            }
        }
    }

    pub(super) fn output_anchor(&self, port: OutputPort) -> Option<BoardPoint> {
        let at = self.position(port.item())?;
        Some(BoardPoint::new(at.x + NODE_WIDTH, at.y + port.offset_y()))
    }

    pub(super) fn input_anchor(&self, id: WorkflowNodeId) -> Option<BoardPoint> {
        let at = self.node(id)?.at;
        Some(BoardPoint::new(at.x, at.y + NODE_HEIGHT / 2.0))
    }

    /// What is under a board point: an output port first, then the topmost block.
    pub(super) fn hit(&self, point: BoardPoint) -> Option<BoardHit> {
        let port = self.output_ports().into_iter().find(|port| {
            self.output_anchor(*port).is_some_and(|anchor| {
                (anchor.x - point.x).hypot(anchor.y - point.y) <= PORT_HIT_RADIUS
            })
        });
        if let Some(port) = port {
            return Some(BoardHit::Output(port));
        }
        let covers = |at: BoardPoint| {
            (at.x..=at.x + NODE_WIDTH).contains(&point.x)
                && (at.y..=at.y + NODE_HEIGHT).contains(&point.y)
        };
        self.nodes
            .iter()
            .rev()
            .find(|node| covers(node.at))
            .map(|node| BoardItem::Node(node.id))
            .or_else(|| covers(self.trigger_at).then_some(BoardItem::Trigger))
            .map(BoardHit::Item)
    }

    pub(super) fn field(&self, field: DraftField) -> Option<&str> {
        let text = match field {
            DraftField::Name => &self.name,
            DraftField::TriggerLevel => match &self.trigger {
                TriggerDraft::Price { level, .. } => level,
                TriggerDraft::Time { .. } => return None,
            },
            DraftField::TriggerTime => match &self.trigger {
                TriggerDraft::Time { time, .. } => time,
                TriggerDraft::Price { .. } => return None,
            },
            DraftField::SessionMaximum => &self.session_maximum,
            DraftField::Cooldown => &self.cooldown,
            DraftField::Level(id)
            | DraftField::WindowStart(id)
            | DraftField::WindowEnd(id)
            | DraftField::Quantity(id)
            | DraftField::StopTicks(id)
            | DraftField::TargetTicks(id) => return self.node(id)?.field(field),
        };
        Some(text)
    }

    /// Replaces a field's text; `false` when the draft has no such field.
    pub(super) fn set_field(&mut self, field: DraftField, value: String) -> bool {
        let slot = match field {
            DraftField::Name => Some(&mut self.name),
            DraftField::TriggerLevel => match &mut self.trigger {
                TriggerDraft::Price { level, .. } => Some(level),
                TriggerDraft::Time { .. } => None,
            },
            DraftField::TriggerTime => match &mut self.trigger {
                TriggerDraft::Time { time, .. } => Some(time),
                TriggerDraft::Price { .. } => None,
            },
            DraftField::SessionMaximum => Some(&mut self.session_maximum),
            DraftField::Cooldown => Some(&mut self.cooldown),
            DraftField::Level(id)
            | DraftField::WindowStart(id)
            | DraftField::WindowEnd(id)
            | DraftField::Quantity(id)
            | DraftField::StopTicks(id)
            | DraftField::TargetTicks(id) => {
                self.node_mut(id).and_then(|node| node.field_mut(field))
            }
        };
        slot.map(|slot| *slot = value).is_some()
    }

    /// The block positions, in whole board units within the owner's bound.
    pub(super) fn layout(&self) -> WorkflowLayout {
        WorkflowLayout {
            positions: std::iter::once((WorkflowNodeId::TRIGGER, self.trigger_at))
                .chain(self.nodes.iter().map(|node| (node.id, node.at)))
                .map(|(node, at)| WorkflowNodePosition {
                    node,
                    x: layout_coordinate(at.x),
                    y: layout_coordinate(at.y),
                })
                .collect(),
        }
    }

    /// The definition and layout the owner receives, or the first problem in the draft.
    pub(super) fn build(
        &self,
        workflow_id: String,
        revision: u32,
        scales: Option<InstrumentScales>,
    ) -> Result<(WorkflowDefinition, WorkflowLayout), DraftProblem> {
        let name = self.name.trim();
        if name.is_empty()
            || name.chars().count() > MAXIMUM_WORKFLOW_NAME_CHARS
            || name.chars().any(char::is_control)
        {
            return Err(DraftProblem::Name);
        }
        let account_id = self.account_id.clone().ok_or(DraftProblem::Account)?;
        let (instrument_id, scales) = self
            .instrument_id
            .clone()
            .zip(scales)
            .ok_or(DraftProblem::Symbol)?;
        let trigger = self.trigger.parse(scales)?;
        let start = self.start.ok_or(DraftProblem::NoStart)?;
        let reachable = self.reachable_from(start);
        if let Some(node) = self.nodes.iter().find(|node| !reachable.contains(&node.id)) {
            return Err(DraftProblem::Unconnected(node.id));
        }
        let nodes = self
            .nodes
            .iter()
            .map(|node| {
                Ok(WorkflowNode {
                    id: node.id,
                    step: node.step(scales)?,
                })
            })
            .collect::<Result<Vec<_>, DraftProblem>>()?;
        if !nodes
            .iter()
            .any(|node| matches!(node.step, WorkflowStep::Action { .. }))
        {
            return Err(DraftProblem::NoAction);
        }
        let definition = WorkflowDefinition {
            workflow_id,
            revision,
            name: name.to_string(),
            account_id,
            instrument_id,
            trigger,
            start,
            nodes,
            limits: self.limits()?,
        };
        Ok((definition, self.layout()))
    }

    fn limits(&self) -> Result<WorkflowLimits, DraftProblem> {
        Ok(match self.repeat {
            RepeatDraft::Once => WorkflowLimits {
                repeat: WorkflowRepeat::Once,
                cooldown_seconds: 0,
                expires_unix_nanos: None,
            },
            RepeatDraft::PerSession => WorkflowLimits {
                repeat: WorkflowRepeat::PerSession {
                    maximum: parse_bounded(
                        &self.session_maximum,
                        1,
                        MAXIMUM_WORKFLOW_SESSION_FIRINGS,
                    )
                    .ok_or(DraftProblem::SessionMaximum)?,
                },
                cooldown_seconds: parse_bounded(
                    &self.cooldown,
                    0,
                    MAXIMUM_WORKFLOW_COOLDOWN_SECONDS,
                )
                .ok_or(DraftProblem::Cooldown)?,
                expires_unix_nanos: None,
            },
        })
    }
}

impl TriggerDraft {
    fn parse(&self, scales: InstrumentScales) -> Result<WorkflowTrigger, DraftProblem> {
        Ok(match self {
            Self::Price { direction, level } => WorkflowTrigger::PriceCrosses {
                direction: *direction,
                level: parse_level(level, scales.price).ok_or(DraftProblem::TriggerLevel {
                    decimals: scales.price,
                })?,
            },
            Self::Time { zone, time } => WorkflowTrigger::TimeOfDay(WorkflowClockTime {
                zone: *zone,
                minute_of_day: parse_clock(time).ok_or(DraftProblem::TriggerTime)?,
            }),
        })
    }
}

impl NodeDraft {
    fn links(&self) -> impl Iterator<Item = WorkflowNodeId> {
        let (first, second) = match self.block {
            BlockDraft::If {
                then, otherwise, ..
            } => (then, otherwise),
            BlockDraft::Action { next, .. } => (next, None),
        };
        first.into_iter().chain(second)
    }

    /// An If has a yes and a no output; an action one next output, except a lock, after
    /// which nothing can run.
    pub(super) fn output_ports(&self) -> Vec<OutputPort> {
        match &self.block {
            BlockDraft::If { .. } => {
                vec![OutputPort::Then(self.id), OutputPort::Otherwise(self.id)]
            }
            BlockDraft::Action {
                action: ActionDraft::LockAccount,
                ..
            } => Vec::new(),
            BlockDraft::Action { .. } => vec![OutputPort::Next(self.id)],
        }
    }

    /// The text fields this block shows in the inspector.
    pub(super) fn fields(&self) -> Vec<DraftField> {
        match &self.block {
            BlockDraft::If {
                condition: ConditionDraft::Price { .. },
                ..
            } => vec![DraftField::Level(self.id)],
            BlockDraft::If {
                condition: ConditionDraft::Between { .. },
                ..
            } => vec![
                DraftField::WindowStart(self.id),
                DraftField::WindowEnd(self.id),
            ],
            BlockDraft::Action {
                action: ActionDraft::Enter { .. },
                ..
            } => vec![
                DraftField::Quantity(self.id),
                DraftField::StopTicks(self.id),
                DraftField::TargetTicks(self.id),
            ],
            BlockDraft::If { .. } | BlockDraft::Action { .. } => Vec::new(),
        }
    }

    fn field(&self, field: DraftField) -> Option<&str> {
        let text = match (&self.block, field) {
            (
                BlockDraft::If {
                    condition: ConditionDraft::Price { level, .. },
                    ..
                },
                DraftField::Level(_),
            ) => level,
            (
                BlockDraft::If {
                    condition: ConditionDraft::Between { start, .. },
                    ..
                },
                DraftField::WindowStart(_),
            ) => start,
            (
                BlockDraft::If {
                    condition: ConditionDraft::Between { end, .. },
                    ..
                },
                DraftField::WindowEnd(_),
            ) => end,
            (
                BlockDraft::Action {
                    action:
                        ActionDraft::Enter {
                            quantity,
                            stop,
                            target,
                            ..
                        },
                    ..
                },
                field,
            ) => match field {
                DraftField::Quantity(_) => quantity,
                DraftField::StopTicks(_) => stop,
                DraftField::TargetTicks(_) => target,
                _ => return None,
            },
            _ => return None,
        };
        Some(text)
    }

    fn field_mut(&mut self, field: DraftField) -> Option<&mut String> {
        match (&mut self.block, field) {
            (
                BlockDraft::If {
                    condition: ConditionDraft::Price { level, .. },
                    ..
                },
                DraftField::Level(_),
            ) => Some(level),
            (
                BlockDraft::If {
                    condition: ConditionDraft::Between { start, end, .. },
                    ..
                },
                DraftField::WindowStart(_) | DraftField::WindowEnd(_),
            ) => Some(if matches!(field, DraftField::WindowStart(_)) {
                start
            } else {
                end
            }),
            (
                BlockDraft::Action {
                    action:
                        ActionDraft::Enter {
                            quantity,
                            stop,
                            target,
                            ..
                        },
                    ..
                },
                DraftField::Quantity(_) | DraftField::StopTicks(_) | DraftField::TargetTicks(_),
            ) => Some(match field {
                DraftField::Quantity(_) => quantity,
                DraftField::StopTicks(_) => stop,
                _ => target,
            }),
            _ => None,
        }
    }

    fn step(&self, scales: InstrumentScales) -> Result<WorkflowStep, DraftProblem> {
        Ok(match &self.block {
            BlockDraft::If {
                condition,
                then,
                otherwise,
            } => WorkflowStep::If {
                condition: condition.parse(self.id, scales)?,
                then: *then,
                otherwise: *otherwise,
            },
            BlockDraft::Action { action, next } => WorkflowStep::Action {
                action: action.parse(self.id, scales)?,
                next: *next,
            },
        })
    }
}

impl ConditionDraft {
    fn parse(
        &self,
        node: WorkflowNodeId,
        scales: InstrumentScales,
    ) -> Result<WorkflowCondition, DraftProblem> {
        Ok(match self {
            Self::Position(requirement) => WorkflowCondition::Position(*requirement),
            Self::Between { zone, start, end } => {
                let (start_minute, end_minute) = parse_clock(start)
                    .zip(parse_clock(end))
                    .filter(|(start, end)| start != end)
                    .ok_or(DraftProblem::Window(node))?;
                WorkflowCondition::Between {
                    zone: *zone,
                    start_minute,
                    end_minute,
                }
            }
            Self::Price { direction, level } => WorkflowCondition::Price {
                direction: *direction,
                level: parse_level(level, scales.price).ok_or(DraftProblem::Level {
                    node,
                    decimals: scales.price,
                })?,
            },
        })
    }
}

impl ActionDraft {
    fn parse(
        &self,
        node: WorkflowNodeId,
        scales: InstrumentScales,
    ) -> Result<WorkflowAction, DraftProblem> {
        Ok(match self {
            Self::Enter {
                side,
                quantity,
                stop,
                target,
            } => WorkflowAction::Enter {
                side: *side,
                quantity: OrderQuantity::parse(quantity)
                    .and_then(|quantity| quantity.fixed_point(scales.quantity))
                    .ok_or(DraftProblem::Quantity(node))?,
                protection: parse_protection(stop, target, node)?,
            },
            Self::Flatten => WorkflowAction::Flatten,
            Self::CancelOrders => WorkflowAction::CancelOrders,
            Self::LockAccount => WorkflowAction::LockAccount,
        })
    }
}

fn trigger_draft(trigger: WorkflowTrigger) -> TriggerDraft {
    match trigger {
        WorkflowTrigger::PriceCrosses { direction, level } => TriggerDraft::Price {
            direction,
            level: decimal_text(level),
        },
        WorkflowTrigger::TimeOfDay(time) => TriggerDraft::Time {
            zone: time.zone,
            time: clock_text(time.minute_of_day),
        },
    }
}

fn block_draft(step: WorkflowStep) -> BlockDraft {
    match step {
        WorkflowStep::If {
            condition,
            then,
            otherwise,
        } => BlockDraft::If {
            condition: match condition {
                WorkflowCondition::Position(requirement) => ConditionDraft::Position(requirement),
                WorkflowCondition::Between {
                    zone,
                    start_minute,
                    end_minute,
                } => ConditionDraft::Between {
                    zone,
                    start: clock_text(start_minute),
                    end: clock_text(end_minute),
                },
                WorkflowCondition::Price { direction, level } => ConditionDraft::Price {
                    direction,
                    level: decimal_text(level),
                },
            },
            then,
            otherwise,
        },
        WorkflowStep::Action { action, next } => BlockDraft::Action {
            action: match action {
                WorkflowAction::Enter {
                    side,
                    quantity,
                    protection,
                } => ActionDraft::Enter {
                    side,
                    quantity: decimal_text(quantity),
                    stop: protection.map_or_else(String::new, |p| p.stop_ticks.to_string()),
                    target: protection.map_or_else(String::new, |p| p.target_ticks.to_string()),
                },
                WorkflowAction::Flatten => ActionDraft::Flatten,
                WorkflowAction::CancelOrders => ActionDraft::CancelOrders,
                WorkflowAction::LockAccount => ActionDraft::LockAccount,
            },
            next,
        },
    }
}

/// A fixed-point value as plain digits that parse back to the same value.
fn decimal_text(value: FixedPoint) -> String {
    super::market_price_text(value.units(), u32::from(value.scale()))
}

pub(super) fn clock_text(minute_of_day: u16) -> String {
    format!("{:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
}

fn board_coordinate(value: i32) -> f32 {
    value.to_f32().unwrap_or(0.0)
}

fn layout_coordinate(value: f32) -> i32 {
    let bound = board_coordinate(MAXIMUM_WORKFLOW_LAYOUT_COORDINATE);
    value.round().clamp(-bound, bound).to_i32().unwrap_or(0)
}

/// A positive decimal price at exactly the instrument's price scale.
fn parse_level(text: &str, scale: u8) -> Option<FixedPoint> {
    let text = text.trim();
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let units = aeris_market_data::parse_decimal_to_fixed(text, u32::from(scale)).ok()?;
    if units <= 0 {
        return None;
    }
    FixedPoint::try_new(units, scale).ok()
}

/// `HH:MM` on a 24-hour clock as a minute of the day.
fn parse_clock(text: &str) -> Option<u16> {
    let (hours, minutes) = text.trim().split_once(':')?;
    let field = |value: &str, limit: u16| {
        (!value.is_empty() && value.len() <= 2 && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u16>().ok())
            .flatten()
            .filter(|value| *value < limit)
    };
    Some(field(hours, 24)? * 60 + field(minutes, 60)?)
}

fn parse_bounded(text: &str, minimum: u32, maximum: u32) -> Option<u32> {
    text.trim()
        .parse::<u32>()
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
}

/// Both distances or neither: a bracket needs a stop and a target. Refused when only one is
/// given or either is out of range.
fn parse_protection(
    stop: &str,
    target: &str,
    node: WorkflowNodeId,
) -> Result<Option<WorkflowProtection>, DraftProblem> {
    if stop.trim().is_empty() && target.trim().is_empty() {
        return Ok(None);
    }
    let ticks = |text: &str| {
        parse_bounded(text, 1, MAXIMUM_WORKFLOW_PROTECTION_TICKS)
            .ok_or(DraftProblem::Protection(node))
    };
    Ok(Some(WorkflowProtection {
        stop_ticks: ticks(stop)?,
        target_ticks: ticks(target)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_trading_runtime::WorkflowState;

    const SCALES: InstrumentScales = InstrumentScales {
        price: 2,
        quantity: 0,
    };

    fn draft() -> WorkflowDraft {
        let mut draft = WorkflowDraft::new(
            " ES breakout ".to_string(),
            Some(TradingAccountId::try_new("practice-1").expect("account")),
            Some(InstrumentId::try_new("instrument:fixture:CME:ESZ6").expect("instrument")),
        );
        assert!(draft.set_field(DraftField::TriggerLevel, "6012.25".to_string()));
        draft
    }

    /// Trigger → If flat → yes: buy 2 with a bracket, no: close the position.
    fn branching() -> (
        WorkflowDraft,
        WorkflowNodeId,
        WorkflowNodeId,
        WorkflowNodeId,
    ) {
        let mut draft = draft();
        let check = draft
            .add_block(BlockKind::IfPosition, Some(BoardItem::Trigger))
            .expect("if");
        let buy = draft
            .add_block(BlockKind::Buy, Some(BoardItem::Node(check)))
            .expect("buy");
        let close = draft
            .add_block(BlockKind::Flatten, Some(BoardItem::Node(check)))
            .expect("close");
        for (field, value) in [
            (DraftField::Quantity(buy), "2"),
            (DraftField::StopTicks(buy), "8"),
            (DraftField::TargetTicks(buy), "16"),
        ] {
            assert!(draft.set_field(field, value.to_string()));
        }
        (draft, check, buy, close)
    }

    #[test]
    fn the_board_builds_the_exact_branching_definition() {
        let (draft, check, buy, close) = branching();
        let (definition, layout) = draft
            .build("wf-1".to_string(), 1, Some(SCALES))
            .expect("valid");
        assert_eq!(definition.name, "ES breakout");
        assert_eq!(definition.start, check);
        assert_eq!(
            definition.trigger,
            WorkflowTrigger::PriceCrosses {
                direction: PriceDirection::Above,
                level: FixedPoint::try_new(601_225, 2).expect("level"),
            }
        );
        assert_eq!(
            definition.nodes,
            vec![
                WorkflowNode {
                    id: check,
                    step: WorkflowStep::If {
                        condition: WorkflowCondition::Position(PositionRequirement::Flat),
                        then: Some(buy),
                        otherwise: Some(close),
                    },
                },
                WorkflowNode {
                    id: buy,
                    step: WorkflowStep::Action {
                        action: WorkflowAction::Enter {
                            side: OrderSide::Buy,
                            quantity: FixedPoint::try_new(2, 0).expect("quantity"),
                            protection: Some(WorkflowProtection {
                                stop_ticks: 8,
                                target_ticks: 16,
                            }),
                        },
                        next: None,
                    },
                },
                WorkflowNode {
                    id: close,
                    step: WorkflowStep::Action {
                        action: WorkflowAction::Flatten,
                        next: None,
                    },
                },
            ]
        );
        assert_eq!(definition.validate(), Ok(()));
        assert_eq!(layout.validate(&definition), Ok(()));
        assert_eq!(layout.positions.len(), 4);
        // The two branches never land on one spot.
        let at = |id| draft.node(id).expect("node").at;
        assert_ne!(at(buy), at(close));
    }

    #[test]
    fn links_never_make_a_loop() {
        let (mut draft, check, buy, close) = branching();
        assert_eq!(
            draft.connect(OutputPort::Next(buy), check),
            Err(LinkRefusal::Loop)
        );
        assert_eq!(
            draft.connect(OutputPort::Next(buy), buy),
            Err(LinkRefusal::Loop)
        );
        // Both branches may meet again.
        assert_eq!(draft.connect(OutputPort::Next(close), buy), Ok(()));
        assert_eq!(
            draft.connect(OutputPort::Next(buy), close),
            Err(LinkRefusal::Loop)
        );
        let lock = draft.add_block(BlockKind::LockAccount, None).expect("lock");
        assert_eq!(
            draft.connect(OutputPort::Next(lock), buy),
            Err(LinkRefusal::Invalid)
        );
        assert_eq!(
            draft.connect(OutputPort::Then(buy), close),
            Err(LinkRefusal::Invalid)
        );
        assert_eq!(
            draft.build("wf-1".to_string(), 1, Some(SCALES)),
            Err(DraftProblem::Unconnected(lock))
        );
        draft.remove_node(lock);
        assert!(draft.build("wf-1".to_string(), 1, Some(SCALES)).is_ok());
    }

    #[test]
    fn changing_a_block_keeps_its_links_and_entry() {
        let (mut draft, check, buy, close) = branching();
        draft.change_block(buy, BlockKind::Sell);
        assert_eq!(draft.node(buy).expect("sell").block.kind(), BlockKind::Sell);
        assert_eq!(draft.field(DraftField::StopTicks(buy)), Some("8"));
        draft.change_block(check, BlockKind::IfTime);
        assert_eq!(draft.link(OutputPort::Then(check)), Some(buy));
        assert_eq!(draft.link(OutputPort::Otherwise(check)), Some(close));
        // An If never becomes an action.
        draft.change_block(check, BlockKind::Flatten);
        assert_eq!(
            draft.node(check).expect("if").block.kind(),
            BlockKind::IfTime
        );
        assert_eq!(draft.connect(OutputPort::Next(close), buy), Ok(()));
        draft.change_block(close, BlockKind::LockAccount);
        assert_eq!(draft.link(OutputPort::Next(close)), None);
        for kind in BlockKind::ALL {
            assert_eq!(kind.block().kind(), kind);
        }
    }

    #[test]
    fn removing_a_block_removes_its_links() {
        let (mut draft, check, buy, _) = branching();
        draft.remove_node(buy);
        assert_eq!(draft.link(OutputPort::Then(check)), None);
        draft.remove_node(check);
        assert_eq!(draft.start, None);
        assert_eq!(
            draft.build("wf-1".to_string(), 1, Some(SCALES)),
            Err(DraftProblem::NoStart)
        );
    }

    #[test]
    fn the_first_problem_is_named() {
        let (draft, check, buy, _) = branching();
        let problem = |change: &dyn Fn(&mut WorkflowDraft)| {
            let mut draft = draft.clone();
            change(&mut draft);
            draft
                .build("wf-1".to_string(), 1, Some(SCALES))
                .expect_err("refused")
        };
        assert_eq!(
            problem(&|draft| draft.name = "  ".to_string()),
            DraftProblem::Name
        );
        assert_eq!(
            problem(&|draft| draft.account_id = None),
            DraftProblem::Account
        );
        assert_eq!(
            problem(&|draft| {
                draft.set_field(DraftField::TriggerLevel, "6012.255".to_string());
            }),
            DraftProblem::TriggerLevel { decimals: 2 }
        );
        assert_eq!(
            problem(&|draft| {
                draft.set_field(DraftField::Quantity(buy), "1.5".to_string());
            }),
            DraftProblem::Quantity(buy)
        );
        assert_eq!(
            problem(&|draft| {
                draft.set_field(DraftField::TargetTicks(buy), String::new());
            }),
            DraftProblem::Protection(buy)
        );
        assert_eq!(
            problem(&|draft| {
                draft.repeat = RepeatDraft::PerSession;
                draft.session_maximum = "51".to_string();
            }),
            DraftProblem::SessionMaximum
        );
        let only_if = |draft: &mut WorkflowDraft| {
            draft.nodes.retain(|node| node.id == check);
            draft.disconnect(OutputPort::Then(check));
            draft.disconnect(OutputPort::Otherwise(check));
        };
        assert_eq!(problem(&only_if), DraftProblem::NoAction);
        assert_eq!(
            draft.build("wf-1".to_string(), 1, None),
            Err(DraftProblem::Symbol)
        );
    }

    #[test]
    fn a_saved_workflow_reopens_as_the_same_draft() {
        let (draft, _, buy, _) = branching();
        let (definition, layout) = draft
            .build("wf-1".to_string(), 3, Some(SCALES))
            .expect("valid");
        let view = WorkflowView {
            definition: definition.clone(),
            layout: WorkflowLayout {
                positions: layout
                    .positions
                    .iter()
                    .copied()
                    .filter(|position| position.node != buy)
                    .collect(),
            },
            state: WorkflowState::Paused,
            session_firings: 0,
            total_firings: 0,
            last_attempt_unix_nanos: None,
            next_due_unix_nanos: None,
            blockers: Vec::new(),
            recent_firings: Vec::new(),
        };
        let reopened = WorkflowDraft::from_view(&view);
        assert_eq!(
            reopened.saved,
            Some(SavedRevision {
                workflow_id: "wf-1".to_string(),
                revision: 3,
            })
        );
        assert_eq!(
            reopened
                .build("wf-1".to_string(), 3, Some(SCALES))
                .map(|(definition, _)| definition),
            Ok(definition)
        );
        // The unplaced block lands in its depth column.
        assert!(
            (reopened.node(buy).expect("buy").at.x - 2.0 * COLUMN_SPACING).abs() < f32::EPSILON
        );
    }

    #[test]
    fn hits_find_ports_before_blocks() {
        let (draft, check, _, _) = branching();
        let at = draft.node(check).expect("if").at;
        assert_eq!(
            draft.hit(BoardPoint::new(at.x + 10.0, at.y + 10.0)),
            Some(BoardHit::Item(BoardItem::Node(check)))
        );
        let yes = draft.output_anchor(OutputPort::Then(check)).expect("yes");
        assert_eq!(
            draft.hit(BoardPoint::new(yes.x + 4.0, yes.y)),
            Some(BoardHit::Output(OutputPort::Then(check)))
        );
        assert_eq!(draft.hit(BoardPoint::new(-500.0, -500.0)), None);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let mut camera = BoardCamera::default();
        let pointer = BoardPoint::new(300.0, 200.0);
        let before = camera.to_board(pointer);
        camera.zoom_at(pointer, 1.25);
        let after = camera.to_board(pointer);
        assert!((before.x - after.x).abs() < 1e-3 && (before.y - after.y).abs() < 1e-3);
        camera.zoom_at(pointer, 100.0);
        assert!((camera.zoom - MAXIMUM_ZOOM).abs() < f32::EPSILON);
    }

    #[test]
    fn clock_and_level_parsing_reject_out_of_range_text() {
        for time in ["24:00", "8", "08:60", "8:5:1", "ab:cd"] {
            assert_eq!(parse_clock(time), None, "{time}");
        }
        assert_eq!(parse_clock("15:55"), Some(955));
        for level in ["", "0", "-5", "6012.255", "1e3", "abc"] {
            assert_eq!(parse_level(level, 2), None, "{level}");
        }
        let node = WorkflowNodeId(1);
        assert_eq!(parse_protection(" ", "", node), Ok(None));
        assert_eq!(
            parse_protection("8", "", node),
            Err(DraftProblem::Protection(node))
        );
        assert_eq!(
            parse_protection("0", "16", node),
            Err(DraftProblem::Protection(node))
        );
    }
}
