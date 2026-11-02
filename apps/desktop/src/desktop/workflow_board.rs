//! The draft a workflow board edits: the trigger, the If and action blocks with their links
//! and board positions, and the text the user typed into each field. `build` turns it into
//! exactly the definition and layout the trading owner validates; the board never talks to
//! the owner itself.

use aeris_desktop::trading::OrderQuantity;
use aeris_instruments::InstrumentId;
use aeris_trading::{FixedPoint, OrderSide, TradingAccountId};
use aeris_trading_runtime::{
    MAXIMUM_WORKFLOW_CHECKS, MAXIMUM_WORKFLOW_COOLDOWN_SECONDS, MAXIMUM_WORKFLOW_LAYOUT_COORDINATE,
    MAXIMUM_WORKFLOW_NAME_CHARS, MAXIMUM_WORKFLOW_NODES, MAXIMUM_WORKFLOW_PROTECTION_TICKS,
    MAXIMUM_WORKFLOW_SESSION_FIRINGS, MAXIMUM_WORKFLOW_TRIGGERS, MINUTES_PER_DAY,
    PositionRequirement, PriceDirection, WorkflowAction, WorkflowCheck, WorkflowClockTime,
    WorkflowCondition, WorkflowDefinition, WorkflowJoin, WorkflowLayout, WorkflowLimits,
    WorkflowNode, WorkflowNodeId, WorkflowNodePosition, WorkflowProtection, WorkflowRepeat,
    WorkflowStep, WorkflowTimeZone, WorkflowTrigger, WorkflowView, WorkflowWeekdays,
};
use num_traits::ToPrimitive;
use std::collections::{BTreeMap, BTreeSet};

pub(super) const NODE_WIDTH: f32 = 232.0;
pub(super) const NODE_HEIGHT: f32 = 156.0;
/// The folder tab above a block's body, which names what the block is.
pub(super) const NODE_TAB_HEIGHT: f32 = 26.0;
const PORT_HIT_RADIUS: f32 = 12.0;
const COLUMN_SPACING: f32 = 296.0;
const ROW_SPACING: f32 = 188.0;
/// The empty board kept around the blocks when the view fits them.
const FIT_MARGIN: f32 = 48.0;
pub(super) const MINIMUM_ZOOM: f32 = 0.5;
pub(super) const MAXIMUM_ZOOM: f32 = 1.5;
const DEFAULT_SESSION_MAXIMUM: &str = "3";
const DEFAULT_COOLDOWN_SECONDS: &str = "60";
pub(super) const DEFAULT_QUANTITY: &str = "1";
const DEFAULT_TRIGGER_TIME: &str = "08:30";
const DEFAULT_TRIGGER_MINUTES: &str = "5";

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

    /// Where the port sits down the right edge of its block's body.
    pub(super) fn offset_y(self) -> f32 {
        let body = NODE_HEIGHT - NODE_TAB_HEIGHT;
        NODE_TAB_HEIGHT
            + match self {
                Self::Trigger | Self::Next(_) => body / 2.0,
                Self::Then(_) => body * 0.36,
                Self::Otherwise(_) => body * 0.72,
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
    Every {
        minutes: String,
    },
}

/// The trigger kinds the inspector offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TriggerKind {
    Rises,
    Falls,
    Time,
    Every,
}

impl TriggerKind {
    pub(super) const ALL: [Self; 4] = [Self::Rises, Self::Falls, Self::Time, Self::Every];
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
            Self::Every { .. } => TriggerKind::Every,
        }
    }

    fn of_kind(kind: TriggerKind) -> Self {
        match kind {
            TriggerKind::Rises => Self::Price {
                direction: PriceDirection::Above,
                level: String::new(),
            },
            TriggerKind::Falls => Self::Price {
                direction: PriceDirection::Below,
                level: String::new(),
            },
            TriggerKind::Time => Self::Time {
                zone: WorkflowTimeZone::Chicago,
                time: DEFAULT_TRIGGER_TIME.to_string(),
            },
            TriggerKind::Every => Self::Every {
                minutes: DEFAULT_TRIGGER_MINUTES.to_string(),
            },
        }
    }

    /// The one text field each trigger has.
    fn text(&self) -> &String {
        match self {
            Self::Price { level, .. } => level,
            Self::Time { time, .. } => time,
            Self::Every { minutes } => minutes,
        }
    }

    fn text_mut(&mut self) -> &mut String {
        match self {
            Self::Price { level, .. } => level,
            Self::Time { time, .. } => time,
            Self::Every { minutes } => minutes,
        }
    }

    /// The field of the trigger at `index`; each kind names its own, so an input never
    /// carries a level into a time.
    pub(super) const fn field(&self, index: usize) -> DraftField {
        match self {
            Self::Price { .. } => DraftField::TriggerLevel(index),
            Self::Time { .. } => DraftField::TriggerTime(index),
            Self::Every { .. } => DraftField::TriggerMinutes(index),
        }
    }
}

/// One check of an If.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CheckDraft {
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
    PriceRange {
        low: String,
        high: String,
    },
    Weekday {
        zone: WorkflowTimeZone,
        days: WorkflowWeekdays,
    },
}

/// The check kinds an If offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CheckKind {
    Position,
    Time,
    Price,
    PriceRange,
    Weekday,
}

impl CheckKind {
    pub(super) const ALL: [Self; 5] = [
        Self::Position,
        Self::Time,
        Self::Price,
        Self::PriceRange,
        Self::Weekday,
    ];

    fn check(self) -> CheckDraft {
        match self {
            Self::Position => CheckDraft::Position(PositionRequirement::Flat),
            Self::Time => CheckDraft::Between {
                zone: WorkflowTimeZone::Chicago,
                start: "08:30".to_string(),
                end: "15:00".to_string(),
            },
            Self::Price => CheckDraft::Price {
                direction: PriceDirection::Above,
                level: String::new(),
            },
            Self::PriceRange => CheckDraft::PriceRange {
                low: String::new(),
                high: String::new(),
            },
            Self::Weekday => CheckDraft::Weekday {
                zone: WorkflowTimeZone::Chicago,
                days: WorkflowWeekdays::WEEKDAYS,
            },
        }
    }
}

impl CheckDraft {
    pub(super) const fn kind(&self) -> CheckKind {
        match self {
            Self::Position(_) => CheckKind::Position,
            Self::Between { .. } => CheckKind::Time,
            Self::Price { .. } => CheckKind::Price,
            Self::PriceRange { .. } => CheckKind::PriceRange,
            Self::Weekday { .. } => CheckKind::Weekday,
        }
    }

    /// The text fields of the check `check` names.
    pub(super) fn fields(&self, check: CheckRef) -> Vec<DraftField> {
        match self {
            Self::Price { .. } => vec![DraftField::Level(check)],
            Self::Between { .. } => {
                vec![DraftField::WindowStart(check), DraftField::WindowEnd(check)]
            }
            Self::PriceRange { .. } => {
                vec![DraftField::RangeLow(check), DraftField::RangeHigh(check)]
            }
            Self::Position(_) | Self::Weekday { .. } => Vec::new(),
        }
    }

    fn text(&self, field: DraftField) -> Option<&str> {
        let text = match (self, field) {
            (Self::Price { level, .. }, DraftField::Level(_)) => level,
            (Self::Between { start, .. }, DraftField::WindowStart(_)) => start,
            (Self::Between { end, .. }, DraftField::WindowEnd(_)) => end,
            (Self::PriceRange { low, .. }, DraftField::RangeLow(_)) => low,
            (Self::PriceRange { high, .. }, DraftField::RangeHigh(_)) => high,
            _ => return None,
        };
        Some(text)
    }

    fn text_mut(&mut self, field: DraftField) -> Option<&mut String> {
        match (self, field) {
            (Self::Price { level, .. }, DraftField::Level(_)) => Some(level),
            (Self::Between { start, .. }, DraftField::WindowStart(_)) => Some(start),
            (Self::Between { end, .. }, DraftField::WindowEnd(_)) => Some(end),
            (Self::PriceRange { low, .. }, DraftField::RangeLow(_)) => Some(low),
            (Self::PriceRange { high, .. }, DraftField::RangeHigh(_)) => Some(high),
            _ => None,
        }
    }

    /// Adds or removes one day of a weekday check (0 is Monday), keeping at least one day.
    pub(super) fn toggle_day(&mut self, day: u8) {
        if let Self::Weekday { days, .. } = self
            && day < 7
            && let Some(toggled) = WorkflowWeekdays::from_bits(days.bits() ^ (1 << day))
        {
            *days = toggled;
        }
    }

    fn parse(
        &self,
        node: WorkflowNodeId,
        scales: InstrumentScales,
    ) -> Result<WorkflowCheck, DraftProblem> {
        Ok(match self {
            Self::Position(requirement) => WorkflowCheck::Position(*requirement),
            Self::Between { zone, start, end } => {
                let (start_minute, end_minute) = parse_clock(start)
                    .zip(parse_clock(end))
                    .filter(|(start, end)| start != end)
                    .ok_or(DraftProblem::Window(node))?;
                WorkflowCheck::Between {
                    zone: *zone,
                    start_minute,
                    end_minute,
                }
            }
            Self::Price { direction, level } => WorkflowCheck::Price {
                direction: *direction,
                level: parse_level(level, scales.price).ok_or(DraftProblem::Level {
                    node,
                    decimals: scales.price,
                })?,
            },
            Self::PriceRange { low, high } => {
                let (low, high) = parse_level(low, scales.price)
                    .zip(parse_level(high, scales.price))
                    .filter(|(low, high)| high.units() > low.units())
                    .ok_or(DraftProblem::Range {
                        node,
                        decimals: scales.price,
                    })?;
                WorkflowCheck::PriceRange { low, high }
            }
            Self::Weekday { zone, days } => WorkflowCheck::Weekday {
                zone: *zone,
                days: *days,
            },
        })
    }
}

/// An If's checks, of which all or any must hold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ConditionDraft {
    pub(super) join: WorkflowJoin,
    /// One to [`MAXIMUM_WORKFLOW_CHECKS`] checks.
    pub(super) checks: Vec<CheckDraft>,
}

impl ConditionDraft {
    fn single(check: CheckDraft) -> Self {
        Self {
            join: WorkflowJoin::All,
            checks: vec![check],
        }
    }
}

/// One check of one If block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CheckRef {
    pub(super) node: WorkflowNodeId,
    pub(super) index: usize,
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
    /// An If is named by its first check.
    pub(super) fn kind(&self) -> BlockKind {
        match self {
            Self::If { condition, .. } => BlockKind::If(
                condition
                    .checks
                    .first()
                    .map_or(CheckKind::Position, CheckDraft::kind),
            ),
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

/// The blocks the palette adds; an If starts with one check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BlockKind {
    If(CheckKind),
    Buy,
    Sell,
    Flatten,
    CancelOrders,
    LockAccount,
}

impl BlockKind {
    pub(super) const CHECKS: [Self; 5] = [
        Self::If(CheckKind::Position),
        Self::If(CheckKind::Time),
        Self::If(CheckKind::Price),
        Self::If(CheckKind::PriceRange),
        Self::If(CheckKind::Weekday),
    ];
    pub(super) const ACTIONS: [Self; 5] = [
        Self::Buy,
        Self::Sell,
        Self::Flatten,
        Self::CancelOrders,
        Self::LockAccount,
    ];

    fn block(self) -> BlockDraft {
        let condition = |check: CheckKind| BlockDraft::If {
            condition: ConditionDraft::single(check.check()),
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
            Self::If(check) => condition(check),
            Self::Buy => action(enter(OrderSide::Buy)),
            Self::Sell => action(enter(OrderSide::Sell)),
            Self::Flatten => action(ActionDraft::Flatten),
            Self::CancelOrders => action(ActionDraft::CancelOrders),
            Self::LockAccount => action(ActionDraft::LockAccount),
        }
    }
}

/// The example workflows the page offers to start from. Each opens as an unsaved draft
/// whose prices are left for the user to set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkflowExample {
    /// Buy with a bracket when the price breaks up through a level, unless already in.
    Breakout,
    /// Buy a drop to a level only in the morning session, once per session.
    SessionDip,
    /// Close everything shortly before the futures close, every session.
    FlatBeforeClose,
    /// On a break down, close a long and lock the account; otherwise cancel working orders.
    ExitAndLock,
}

impl WorkflowExample {
    pub(super) const ALL: [Self; 4] = [
        Self::Breakout,
        Self::SessionDip,
        Self::FlatBeforeClose,
        Self::ExitAndLock,
    ];

    /// The example as a draft on `account_id` and `instrument_id`, laid out by depth.
    pub(super) fn draft(
        self,
        name: String,
        account_id: Option<TradingAccountId>,
        instrument_id: Option<InstrumentId>,
    ) -> WorkflowDraft {
        let (trigger, blocks, repeat) = self.blocks();
        let mut draft = WorkflowDraft::new(name, account_id, instrument_id);
        draft.triggers = vec![trigger];
        draft.repeat = repeat;
        if repeat == RepeatDraft::PerSession {
            draft.session_maximum = "1".to_string();
        }
        draft.start = Some(WorkflowNodeId(1));
        draft.nodes = (1..)
            .zip(blocks)
            .map(|(number, block)| NodeDraft {
                id: WorkflowNodeId(number),
                at: BoardPoint::default(),
                block,
            })
            .collect();
        draft.arrange(&BTreeMap::new());
        draft
    }

    /// The example's trigger, its blocks numbered from 1 in order with the start first,
    /// and how often it repeats.
    fn blocks(self) -> (TriggerDraft, Vec<BlockDraft>, RepeatDraft) {
        let id = WorkflowNodeId;
        let check = |check, then: Option<u16>, otherwise: Option<u16>| BlockDraft::If {
            condition: ConditionDraft::single(check),
            then: then.map(id),
            otherwise: otherwise.map(id),
        };
        let act = |action, next: Option<u16>| BlockDraft::Action {
            action,
            next: next.map(id),
        };
        let buy = |stop: &str, target: &str| ActionDraft::Enter {
            side: OrderSide::Buy,
            quantity: DEFAULT_QUANTITY.to_string(),
            stop: stop.to_string(),
            target: target.to_string(),
        };
        let price = |direction| TriggerDraft::Price {
            direction,
            level: String::new(),
        };
        match self {
            Self::Breakout => (
                price(PriceDirection::Above),
                vec![
                    check(
                        CheckDraft::Position(PositionRequirement::Flat),
                        Some(2),
                        None,
                    ),
                    act(buy("20", "40"), None),
                ],
                RepeatDraft::Once,
            ),
            Self::SessionDip => (
                price(PriceDirection::Below),
                vec![
                    check(
                        CheckDraft::Between {
                            zone: WorkflowTimeZone::Chicago,
                            start: "08:30".to_string(),
                            end: "11:00".to_string(),
                        },
                        Some(2),
                        None,
                    ),
                    check(
                        CheckDraft::Position(PositionRequirement::Flat),
                        Some(3),
                        None,
                    ),
                    act(buy("16", "32"), None),
                ],
                RepeatDraft::PerSession,
            ),
            Self::FlatBeforeClose => (
                TriggerDraft::Time {
                    zone: WorkflowTimeZone::Chicago,
                    time: "15:55".to_string(),
                },
                vec![
                    check(
                        CheckDraft::Position(PositionRequirement::Open),
                        Some(2),
                        None,
                    ),
                    act(ActionDraft::Flatten, Some(3)),
                    act(ActionDraft::CancelOrders, None),
                ],
                RepeatDraft::PerSession,
            ),
            Self::ExitAndLock => (
                price(PriceDirection::Below),
                vec![
                    check(
                        CheckDraft::Position(PositionRequirement::Long),
                        Some(2),
                        Some(4),
                    ),
                    act(ActionDraft::Flatten, Some(3)),
                    act(ActionDraft::LockAccount, None),
                    act(ActionDraft::CancelOrders, None),
                ],
                RepeatDraft::Once,
            ),
        }
    }
}

/// A text field of the draft, which an inspector input edits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DraftField {
    Name,
    /// The level of the price trigger at this index.
    TriggerLevel(usize),
    TriggerTime(usize),
    TriggerMinutes(usize),
    SessionMaximum,
    Cooldown,
    Level(CheckRef),
    WindowStart(CheckRef),
    WindowEnd(CheckRef),
    RangeLow(CheckRef),
    RangeHigh(CheckRef),
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
    /// No trigger, or more than [`MAXIMUM_WORKFLOW_TRIGGERS`].
    Triggers,
    TriggerLevel {
        decimals: u8,
    },
    TriggerTime,
    TriggerMinutes,
    NoStart,
    Unconnected(WorkflowNodeId),
    NoAction,
    /// An If without a check, or with more than [`MAXIMUM_WORKFLOW_CHECKS`].
    Checks(WorkflowNodeId),
    Level {
        node: WorkflowNodeId,
        decimals: u8,
    },
    Window(WorkflowNodeId),
    Range {
        node: WorkflowNodeId,
        decimals: u8,
    },
    Quantity(WorkflowNodeId),
    Protection(WorkflowNodeId),
    SessionMaximum,
    Cooldown,
}

impl DraftProblem {
    /// The block the problem belongs to, for the board to mark.
    pub(super) const fn item(self) -> Option<BoardItem> {
        match self {
            Self::Triggers
            | Self::TriggerLevel { .. }
            | Self::TriggerTime
            | Self::TriggerMinutes
            | Self::NoStart => Some(BoardItem::Trigger),
            Self::Unconnected(node)
            | Self::Checks(node)
            | Self::Level { node, .. }
            | Self::Window(node)
            | Self::Range { node, .. }
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

    /// The view that shows the board region `extent` whole and centered in a board of
    /// `size` pixels, never enlarged past 100%; a region too large even at the smallest zoom
    /// keeps its top-left corner in view.
    pub(super) fn fitting(extent: (BoardPoint, BoardPoint), size: BoardPoint) -> Self {
        let (low, high) = extent;
        let width = (high.x - low.x).max(1.0);
        let height = (high.y - low.y).max(1.0);
        let zoom = ((size.x - 2.0 * FIT_MARGIN) / width)
            .min((size.y - 2.0 * FIT_MARGIN) / height)
            .clamp(MINIMUM_ZOOM, 1.0);
        let offset =
            |available: f32, length: f32| ((available - length * zoom) / 2.0).max(FIT_MARGIN);
        Self {
            pan: BoardPoint::new(
                offset(size.x, width) - low.x * zoom,
                offset(size.y, height) - low.y * zoom,
            ),
            zoom,
        }
    }
}

/// A workflow as the board edits it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WorkflowDraft {
    pub(super) saved: Option<SavedRevision>,
    pub(super) name: String,
    pub(super) account_id: Option<TradingAccountId>,
    pub(super) instrument_id: Option<InstrumentId>,
    /// One to [`MAXIMUM_WORKFLOW_TRIGGERS`] triggers, drawn as one block.
    pub(super) triggers: Vec<TriggerDraft>,
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
            triggers: vec![TriggerDraft::of_kind(TriggerKind::Rises)],
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
            triggers: definition
                .triggers
                .iter()
                .copied()
                .map(trigger_draft)
                .collect(),
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

    /// The checks of an If block, for the inspector to edit.
    pub(super) fn condition_mut(&mut self, id: WorkflowNodeId) -> Option<&mut ConditionDraft> {
        match &mut self.node_mut(id)?.block {
            BlockDraft::If { condition, .. } => Some(condition),
            BlockDraft::Action { .. } => None,
        }
    }

    fn check(&self, check: CheckRef) -> Option<&CheckDraft> {
        match &self.node(check.node)?.block {
            BlockDraft::If { condition, .. } => condition.checks.get(check.index),
            BlockDraft::Action { .. } => None,
        }
    }

    pub(super) fn check_mut(&mut self, check: CheckRef) -> Option<&mut CheckDraft> {
        self.condition_mut(check.node)?.checks.get_mut(check.index)
    }

    /// Adds a position check to an If; `false` when it already has the most checks.
    pub(super) fn add_check(&mut self, node: WorkflowNodeId) -> bool {
        match self.condition_mut(node) {
            Some(condition) if condition.checks.len() < MAXIMUM_WORKFLOW_CHECKS => {
                condition.checks.push(CheckKind::Position.check());
                true
            }
            _ => false,
        }
    }

    /// Removes a check; an If keeps its last one.
    pub(super) fn remove_check(&mut self, check: CheckRef) {
        if let Some(condition) = self.condition_mut(check.node)
            && condition.checks.len() > 1
            && check.index < condition.checks.len()
        {
            condition.checks.remove(check.index);
        }
    }

    pub(super) fn set_check_kind(&mut self, check: CheckRef, kind: CheckKind) {
        if let Some(current) = self.check_mut(check)
            && current.kind() != kind
        {
            *current = kind.check();
        }
    }

    pub(super) fn trigger_mut(&mut self, index: usize) -> Option<&mut TriggerDraft> {
        self.triggers.get_mut(index)
    }

    /// Adds a price trigger; `false` when the workflow already has the most triggers.
    pub(super) fn add_trigger(&mut self) -> bool {
        if self.triggers.len() >= MAXIMUM_WORKFLOW_TRIGGERS {
            return false;
        }
        self.triggers
            .push(TriggerDraft::of_kind(TriggerKind::Rises));
        true
    }

    /// Removes a trigger; a workflow keeps its last one.
    pub(super) fn remove_trigger(&mut self, index: usize) {
        if self.triggers.len() > 1 && index < self.triggers.len() {
            self.triggers.remove(index);
        }
    }

    /// Changes what one trigger waits for; a crossing that only changes direction keeps its
    /// level.
    pub(super) fn set_trigger_kind(&mut self, index: usize, kind: TriggerKind) {
        let Some(trigger) = self.triggers.get_mut(index) else {
            return;
        };
        if trigger.kind() == kind {
            return;
        }
        match (trigger, kind) {
            (TriggerDraft::Price { direction, .. }, TriggerKind::Rises) => {
                *direction = PriceDirection::Above;
            }
            (TriggerDraft::Price { direction, .. }, TriggerKind::Falls) => {
                *direction = PriceDirection::Below;
            }
            (trigger, kind) => *trigger = TriggerDraft::of_kind(kind),
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
    /// action an action. An If changes its first check and keeps the others, a buy turned
    /// into a sell keeps its quantity and bracket, and a lock drops its next link because
    /// nothing runs after it.
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
            ) => {
                if let (Some(first), Some(check)) = (
                    condition.checks.first_mut(),
                    replacement.checks.into_iter().next(),
                ) {
                    *first = check;
                }
            }
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
        Some(BoardPoint::new(
            at.x,
            at.y + NODE_TAB_HEIGHT + (NODE_HEIGHT - NODE_TAB_HEIGHT) / 2.0,
        ))
    }

    /// The top-left and bottom-right corners around every block.
    pub(super) fn extent(&self) -> (BoardPoint, BoardPoint) {
        let (low, high) = self.nodes.iter().map(|node| node.at).fold(
            (self.trigger_at, self.trigger_at),
            |(low, high), at| {
                (
                    BoardPoint::new(low.x.min(at.x), low.y.min(at.y)),
                    BoardPoint::new(high.x.max(at.x), high.y.max(at.y)),
                )
            },
        );
        (
            low,
            BoardPoint::new(high.x + NODE_WIDTH, high.y + NODE_HEIGHT),
        )
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
            DraftField::TriggerLevel(index)
            | DraftField::TriggerTime(index)
            | DraftField::TriggerMinutes(index) => self
                .triggers
                .get(index)
                .filter(|trigger| trigger.field(index) == field)?
                .text(),
            DraftField::SessionMaximum => &self.session_maximum,
            DraftField::Cooldown => &self.cooldown,
            DraftField::Level(check)
            | DraftField::WindowStart(check)
            | DraftField::WindowEnd(check)
            | DraftField::RangeLow(check)
            | DraftField::RangeHigh(check) => return self.check(check)?.text(field),
            DraftField::Quantity(id) | DraftField::StopTicks(id) | DraftField::TargetTicks(id) => {
                return self.node(id)?.field(field);
            }
        };
        Some(text)
    }

    /// Replaces a field's text; `false` when the draft has no such field.
    pub(super) fn set_field(&mut self, field: DraftField, value: String) -> bool {
        let slot = match field {
            DraftField::Name => Some(&mut self.name),
            DraftField::TriggerLevel(index)
            | DraftField::TriggerTime(index)
            | DraftField::TriggerMinutes(index) => self
                .triggers
                .get_mut(index)
                .filter(|trigger| trigger.field(index) == field)
                .map(TriggerDraft::text_mut),
            DraftField::SessionMaximum => Some(&mut self.session_maximum),
            DraftField::Cooldown => Some(&mut self.cooldown),
            DraftField::Level(check)
            | DraftField::WindowStart(check)
            | DraftField::WindowEnd(check)
            | DraftField::RangeLow(check)
            | DraftField::RangeHigh(check) => self
                .check_mut(check)
                .and_then(|check| check.text_mut(field)),
            DraftField::Quantity(id) | DraftField::StopTicks(id) | DraftField::TargetTicks(id) => {
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
        if self.triggers.is_empty() || self.triggers.len() > MAXIMUM_WORKFLOW_TRIGGERS {
            return Err(DraftProblem::Triggers);
        }
        let triggers = self
            .triggers
            .iter()
            .map(|trigger| trigger.parse(scales))
            .collect::<Result<Vec<_>, DraftProblem>>()?;
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
            triggers,
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
            Self::Every { minutes } => WorkflowTrigger::Every {
                minutes: parse_bounded(minutes, 1, u32::from(MINUTES_PER_DAY))
                    .and_then(|minutes| u16::try_from(minutes).ok())
                    .ok_or(DraftProblem::TriggerMinutes)?,
            },
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
            BlockDraft::If { condition, .. } => condition
                .checks
                .iter()
                .enumerate()
                .flat_map(|(index, check)| {
                    check.fields(CheckRef {
                        node: self.id,
                        index,
                    })
                })
                .collect(),
            BlockDraft::Action {
                action: ActionDraft::Enter { .. },
                ..
            } => vec![
                DraftField::Quantity(self.id),
                DraftField::StopTicks(self.id),
                DraftField::TargetTicks(self.id),
            ],
            BlockDraft::Action { .. } => Vec::new(),
        }
    }

    /// An entry's quantity and bracket text.
    fn field(&self, field: DraftField) -> Option<&str> {
        let text = match (&self.block, field) {
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
        let checks = self
            .checks
            .iter()
            .map(|check| check.parse(node, scales))
            .collect::<Result<Vec<_>, DraftProblem>>()?;
        WorkflowCondition::joined(self.join, &checks).ok_or(DraftProblem::Checks(node))
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
        WorkflowTrigger::Every { minutes } => TriggerDraft::Every {
            minutes: minutes.to_string(),
        },
    }
}

fn check_draft(check: WorkflowCheck) -> CheckDraft {
    match check {
        WorkflowCheck::Position(requirement) => CheckDraft::Position(requirement),
        WorkflowCheck::Between {
            zone,
            start_minute,
            end_minute,
        } => CheckDraft::Between {
            zone,
            start: clock_text(start_minute),
            end: clock_text(end_minute),
        },
        WorkflowCheck::Price { direction, level } => CheckDraft::Price {
            direction,
            level: decimal_text(level),
        },
        WorkflowCheck::PriceRange { low, high } => CheckDraft::PriceRange {
            low: decimal_text(low),
            high: decimal_text(high),
        },
        WorkflowCheck::Weekday { zone, days } => CheckDraft::Weekday { zone, days },
    }
}

fn block_draft(step: WorkflowStep) -> BlockDraft {
    match step {
        WorkflowStep::If {
            condition,
            then,
            otherwise,
        } => BlockDraft::If {
            condition: ConditionDraft {
                join: condition.join(),
                checks: condition.checks().map(check_draft).collect(),
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
        assert!(draft.set_field(DraftField::TriggerLevel(0), "6012.25".to_string()));
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
            .add_block(BlockKind::If(CheckKind::Position), Some(BoardItem::Trigger))
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
            definition.triggers,
            vec![WorkflowTrigger::PriceCrosses {
                direction: PriceDirection::Above,
                level: FixedPoint::try_new(601_225, 2).expect("level"),
            }]
        );
        assert_eq!(
            definition.nodes,
            vec![
                WorkflowNode {
                    id: check,
                    step: WorkflowStep::If {
                        condition: WorkflowCondition::single(WorkflowCheck::Position(
                            PositionRequirement::Flat,
                        )),
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
        draft.change_block(check, BlockKind::If(CheckKind::Time));
        assert_eq!(draft.link(OutputPort::Then(check)), Some(buy));
        assert_eq!(draft.link(OutputPort::Otherwise(check)), Some(close));
        // An If never becomes an action.
        draft.change_block(check, BlockKind::Flatten);
        assert_eq!(
            draft.node(check).expect("if").block.kind(),
            BlockKind::If(CheckKind::Time)
        );
        assert_eq!(draft.connect(OutputPort::Next(close), buy), Ok(()));
        draft.change_block(close, BlockKind::LockAccount);
        assert_eq!(draft.link(OutputPort::Next(close)), None);
        for kind in BlockKind::CHECKS.into_iter().chain(BlockKind::ACTIONS) {
            assert_eq!(kind.block().kind(), kind);
        }
    }

    #[test]
    fn several_triggers_and_joined_checks_build_and_reopen() {
        let (mut draft, check, _, _) = branching();
        assert!(draft.add_trigger());
        draft.set_trigger_kind(1, TriggerKind::Every);
        assert_eq!(draft.field(DraftField::TriggerMinutes(1)), Some("5"));
        assert_eq!(draft.field(DraftField::TriggerLevel(1)), None);
        assert!(draft.add_trigger());
        draft.set_trigger_kind(2, TriggerKind::Time);
        assert!(draft.add_trigger());
        assert!(!draft.add_trigger(), "at most four triggers");
        draft.remove_trigger(3);
        assert!(draft.set_field(DraftField::TriggerMinutes(1), "15".to_string()));
        let range = CheckRef {
            node: check,
            index: 1,
        };
        let weekday = CheckRef {
            node: check,
            index: 2,
        };
        assert!(draft.add_check(check));
        draft.set_check_kind(range, CheckKind::PriceRange);
        assert!(draft.add_check(check));
        draft.set_check_kind(weekday, CheckKind::Weekday);
        if let Some(condition) = draft.condition_mut(check) {
            condition.join = WorkflowJoin::Any;
        }
        assert_eq!(
            draft.build("wf-1".to_string(), 1, Some(SCALES)).err(),
            Some(DraftProblem::Range {
                node: check,
                decimals: 2
            })
        );
        assert!(draft.set_field(DraftField::RangeLow(range), "6000".to_string()));
        assert!(draft.set_field(DraftField::RangeHigh(range), "5990".to_string()));
        assert!(
            draft.build("wf-1".to_string(), 1, Some(SCALES)).is_err(),
            "the high must be above the low"
        );
        assert!(draft.set_field(DraftField::RangeHigh(range), "6010.5".to_string()));
        if let Some(check) = draft.check_mut(weekday) {
            check.toggle_day(4);
            for day in 0..4 {
                check.toggle_day(day);
            }
        }
        // The last day stays: a weekday check never names no day.
        assert_eq!(
            draft.check_mut(weekday).cloned(),
            Some(CheckDraft::Weekday {
                zone: WorkflowTimeZone::Chicago,
                days: WorkflowWeekdays::from_bits(0b1000).expect("thursday"),
            })
        );
        let (definition, layout) = draft
            .build("wf-1".to_string(), 2, Some(SCALES))
            .expect("valid");
        assert_eq!(definition.validate(), Ok(()));
        assert_eq!(definition.triggers.len(), 3);
        assert_eq!(
            definition.triggers[1],
            WorkflowTrigger::Every { minutes: 15 }
        );
        let condition = match definition.node(check).map(|node| node.step) {
            Some(WorkflowStep::If { condition, .. }) => condition,
            other => panic!("{other:?}"),
        };
        assert_eq!(condition.join(), WorkflowJoin::Any);
        assert_eq!(
            condition.checks().nth(1),
            Some(WorkflowCheck::PriceRange {
                low: FixedPoint::try_new(600_000, 2).expect("low"),
                high: FixedPoint::try_new(601_050, 2).expect("high"),
            })
        );
        draft.remove_check(range);
        assert_eq!(
            draft.node(check).expect("if").fields(),
            Vec::<DraftField>::new(),
            "the range's fields leave with it"
        );
        let view = WorkflowView {
            definition: definition.clone(),
            layout,
            state: WorkflowState::Paused,
            session_firings: 0,
            total_firings: 0,
            last_attempt_unix_nanos: None,
            next_due_unix_nanos: None,
            blockers: Vec::new(),
            recent_firings: Vec::new(),
        };
        assert_eq!(
            WorkflowDraft::from_view(&view)
                .build("wf-1".to_string(), 2, Some(SCALES))
                .map(|(definition, _)| definition),
            Ok(definition)
        );
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
                draft.set_field(DraftField::TriggerLevel(0), "6012.255".to_string());
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
    fn every_example_builds_once_its_prices_are_set() {
        for example in WorkflowExample::ALL {
            let mut draft = example.draft(
                "Example".to_string(),
                Some(TradingAccountId::try_new("practice-1").expect("account")),
                Some(InstrumentId::try_new("instrument:fixture:CME:ESZ6").expect("instrument")),
            );
            let unset = draft.build("wf-1".to_string(), 1, Some(SCALES));
            if matches!(draft.triggers[0], TriggerDraft::Price { .. }) {
                assert_eq!(
                    unset.err(),
                    Some(DraftProblem::TriggerLevel { decimals: 2 }),
                    "{example:?}"
                );
                assert!(draft.set_field(DraftField::TriggerLevel(0), "6012.25".to_string()));
            }
            let (definition, layout) = draft
                .build("wf-1".to_string(), 1, Some(SCALES))
                .unwrap_or_else(|problem| panic!("{example:?}: {problem:?}"));
            assert_eq!(definition.validate(), Ok(()), "{example:?}");
            assert_eq!(layout.validate(&definition), Ok(()), "{example:?}");
            let spots = layout
                .positions
                .iter()
                .map(|position| (position.x, position.y))
                .collect::<BTreeSet<_>>();
            assert_eq!(spots.len(), layout.positions.len(), "{example:?} overlaps");
        }
    }

    #[test]
    fn the_fitted_view_shows_every_block() {
        let draft = WorkflowExample::ExitAndLock.draft("Example".to_string(), None, None);
        let size = BoardPoint::new(1200.0, 800.0);
        let camera = BoardCamera::fitting(draft.extent(), size);
        assert!(camera.zoom <= 1.0);
        let (low, high) = draft.extent();
        let (low, high) = (camera.to_screen(low), camera.to_screen(high));
        assert!(low.x >= 0.0 && low.y >= 0.0, "{low:?}");
        assert!(high.x <= size.x && high.y <= size.y, "{high:?}");
        // A board too small for the blocks keeps their top-left corner in view.
        let cramped = BoardCamera::fitting(draft.extent(), BoardPoint::new(300.0, 200.0));
        assert!((cramped.zoom - MINIMUM_ZOOM).abs() < f32::EPSILON);
        assert!(cramped.to_screen(draft.extent().0).x >= 0.0);
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
