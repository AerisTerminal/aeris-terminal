//! Durable workflows: each accepted definition with its board layout and counters, and its
//! newest firings.

use super::{StepTx, TradingStore, database_error, params};
use crate::workflow::{
    PositionRequirement, PriceDirection, WORKFLOW_FIRING_HISTORY, WorkflowAction,
    WorkflowActionResult, WorkflowBlocker, WorkflowClockTime, WorkflowCondition,
    WorkflowDefinition, WorkflowError, WorkflowFiring, WorkflowFiringOutcome, WorkflowLayout,
    WorkflowLimits, WorkflowNode, WorkflowNodeId, WorkflowNodePosition, WorkflowProtection,
    WorkflowRecord, WorkflowRepeat, WorkflowState, WorkflowStep, WorkflowStepOutcome,
    WorkflowStepResult, WorkflowTimeZone, WorkflowTrigger,
};
use aeris_instruments::InstrumentId;
use aeris_trading::{ClientOrderId, FixedPoint, OrderSide, TradingAccountId};
use serde::{Deserialize, Serialize};

fn store_error(error: rusqlite::Error) -> WorkflowError {
    WorkflowError::Owner(database_error(error))
}

fn corrupt(detail: impl std::fmt::Display) -> WorkflowError {
    WorkflowError::Owner(format!("stored workflow is invalid: {detail}"))
}

fn encode(value: &impl Serialize) -> Result<String, WorkflowError> {
    serde_json::to_string(value).map_err(|error| WorkflowError::Owner(error.to_string()))
}

impl TradingStore {
    /// A store still waiting for its group conversion has not reached the workflow tables.
    fn workflow_tables_present(&self) -> Result<bool, WorkflowError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = 'workflows')",
                [],
                |row| row.get(0),
            )
            .map_err(store_error)
    }

    /// Every workflow with its retained firings, oldest first.
    pub(crate) fn load_workflows(
        &self,
    ) -> Result<Vec<(WorkflowRecord, Vec<WorkflowFiring>)>, WorkflowError> {
        if !self.workflow_tables_present()? {
            return Ok(Vec::new());
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT workflow_id, account_id, revision, state, definition_json, layout_json,
                    session_start_unix_nanos, session_firings, total_firings, last_sequence,
                    last_attempt_unix_nanos
                 FROM workflows ORDER BY workflow_id",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u32>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ),
                    (
                        row.get::<_, i64>(6)?,
                        row.get::<_, u32>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                    ),
                ))
            })
            .map_err(store_error)?;
        let mut workflows = Vec::new();
        for row in rows {
            let (
                (workflow_id, account_id, revision, state, definition_json, layout_json),
                (
                    session_start_unix_nanos,
                    session_firings,
                    total_firings,
                    last_sequence,
                    last_attempt_unix_nanos,
                ),
            ) = row.map_err(store_error)?;
            let stored: StoredDefinition =
                serde_json::from_str(&definition_json).map_err(corrupt)?;
            let definition = stored.into_definition(workflow_id, revision, account_id)?;
            definition
                .validate()
                .map_err(|invalid| corrupt(format!("{invalid:?}")))?;
            let layout: Vec<StoredNodePosition> =
                serde_json::from_str(&layout_json).map_err(corrupt)?;
            let layout = WorkflowLayout {
                positions: layout
                    .into_iter()
                    .map(StoredNodePosition::into_position)
                    .collect(),
            };
            layout
                .validate(&definition)
                .map_err(|invalid| corrupt(format!("{invalid:?}")))?;
            let record = WorkflowRecord {
                layout,
                state: WorkflowState::parse(&state).ok_or_else(|| corrupt("state"))?,
                session_start_unix_nanos,
                session_firings,
                total_firings: u64::try_from(total_firings).map_err(corrupt)?,
                last_sequence: u64::try_from(last_sequence).map_err(corrupt)?,
                last_attempt_unix_nanos,
                definition,
            };
            let firings = self.load_workflow_firings(&record.definition.workflow_id)?;
            workflows.push((record, firings));
        }
        Ok(workflows)
    }

    fn load_workflow_firings(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<WorkflowFiring>, WorkflowError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT sequence, revision, fired_unix_nanos, outcome_json
                 FROM workflow_firings WHERE workflow_id = ?1 ORDER BY sequence",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map([workflow_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(store_error)?;
        let mut firings = Vec::new();
        for row in rows {
            let (sequence, revision, fired_unix_nanos, outcome_json) = row.map_err(store_error)?;
            let outcome: StoredOutcome = serde_json::from_str(&outcome_json).map_err(corrupt)?;
            firings.push(WorkflowFiring {
                sequence: u64::try_from(sequence).map_err(corrupt)?,
                revision,
                fired_unix_nanos,
                outcome: outcome.into_outcome()?,
            });
        }
        Ok(firings)
    }

    /// Writes one workflow and, when given, one of its firings in a single transaction,
    /// keeping only the newest [`WORKFLOW_FIRING_HISTORY`] firings.
    pub(crate) fn put_workflow(
        &mut self,
        record: &WorkflowRecord,
        firing: Option<&WorkflowFiring>,
    ) -> Result<(), WorkflowError> {
        let definition = &record.definition;
        let definition_json = encode(&StoredDefinition::from_definition(definition))?;
        let layout_json = encode(&StoredNodePosition::from_layout(&record.layout))?;
        let total_firings = i64::try_from(record.total_firings).map_err(corrupt)?;
        let last_sequence = i64::try_from(record.last_sequence).map_err(corrupt)?;
        let transaction = self.connection.transaction().map_err(store_error)?;
        transaction
            .execute(
                "INSERT INTO workflows(workflow_id, account_id, revision, state, definition_json,
                    layout_json, session_start_unix_nanos, session_firings, total_firings,
                    last_sequence, last_attempt_unix_nanos)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(workflow_id) DO UPDATE SET account_id=excluded.account_id,
                    revision=excluded.revision, state=excluded.state,
                    definition_json=excluded.definition_json, layout_json=excluded.layout_json,
                    session_start_unix_nanos=excluded.session_start_unix_nanos,
                    session_firings=excluded.session_firings,
                    total_firings=excluded.total_firings, last_sequence=excluded.last_sequence,
                    last_attempt_unix_nanos=excluded.last_attempt_unix_nanos",
                params![
                    definition.workflow_id,
                    definition.account_id.as_str(),
                    definition.revision,
                    record.state.as_str(),
                    definition_json,
                    layout_json,
                    record.session_start_unix_nanos,
                    record.session_firings,
                    total_firings,
                    last_sequence,
                    record.last_attempt_unix_nanos,
                ],
            )
            .map_err(store_error)?;
        if let Some(firing) = firing {
            let outcome_json = encode(&StoredOutcome::from_outcome(&firing.outcome))?;
            transaction
                .execute(
                    "INSERT OR REPLACE INTO workflow_firings(workflow_id, sequence, revision,
                        fired_unix_nanos, outcome_json) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        definition.workflow_id,
                        i64::try_from(firing.sequence).map_err(corrupt)?,
                        firing.revision,
                        firing.fired_unix_nanos,
                        outcome_json,
                    ],
                )
                .map_err(store_error)?;
            transaction
                .execute(
                    "DELETE FROM workflow_firings WHERE workflow_id = ?1 AND sequence NOT IN (
                        SELECT sequence FROM workflow_firings WHERE workflow_id = ?1
                        ORDER BY sequence DESC LIMIT ?2)",
                    params![
                        definition.workflow_id,
                        i64::try_from(WORKFLOW_FIRING_HISTORY).map_err(corrupt)?
                    ],
                )
                .map_err(store_error)?;
        }
        StepTx::commit_revision(transaction).map_err(store_error)
    }

    /// Replaces only where the board draws one workflow's nodes.
    pub(crate) fn put_workflow_layout(
        &mut self,
        workflow_id: &str,
        layout: &WorkflowLayout,
    ) -> Result<(), WorkflowError> {
        let layout_json = encode(&StoredNodePosition::from_layout(layout))?;
        let transaction = self.connection.transaction().map_err(store_error)?;
        let changed = transaction
            .execute(
                "UPDATE workflows SET layout_json = ?2 WHERE workflow_id = ?1",
                params![workflow_id, layout_json],
            )
            .map_err(store_error)?;
        if changed != 1 {
            return Err(WorkflowError::UnknownWorkflow);
        }
        StepTx::commit_revision(transaction).map_err(store_error)
    }

    /// Removes one workflow and its firings.
    pub(crate) fn delete_workflow(&mut self, workflow_id: &str) -> Result<(), WorkflowError> {
        let transaction = self.connection.transaction().map_err(store_error)?;
        transaction
            .execute(
                "DELETE FROM workflow_firings WHERE workflow_id = ?1",
                [workflow_id],
            )
            .map_err(store_error)?;
        transaction
            .execute(
                "DELETE FROM workflows WHERE workflow_id = ?1",
                [workflow_id],
            )
            .map_err(store_error)?;
        StepTx::commit_revision(transaction).map_err(store_error)
    }
}

/// The stored shape of a definition; its identity, revision and account are columns.
#[derive(Deserialize, Serialize)]
struct StoredDefinition {
    name: String,
    instrument_id: String,
    trigger: StoredTrigger,
    start: u16,
    nodes: Vec<StoredNode>,
    repeat: StoredRepeat,
    cooldown_seconds: u32,
    expires_unix_nanos: Option<i64>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct StoredFixed {
    units: i64,
    scale: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredTrigger {
    PriceCrosses {
        direction: StoredDirection,
        level: StoredFixed,
    },
    TimeOfDay {
        zone: StoredZone,
        minute_of_day: u16,
    },
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredDirection {
    Above,
    Below,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredZone {
    Utc,
    Chicago,
}

#[derive(Deserialize, Serialize)]
struct StoredNode {
    id: u16,
    step: StoredStep,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredStep {
    If {
        condition: StoredCondition,
        then: Option<u16>,
        otherwise: Option<u16>,
    },
    Action {
        action: StoredAction,
        next: Option<u16>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredCondition {
    Position {
        requirement: StoredRequirement,
    },
    Between {
        zone: StoredZone,
        start_minute: u16,
        end_minute: u16,
    },
    Price {
        direction: StoredDirection,
        level: StoredFixed,
    },
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredRequirement {
    Flat,
    Long,
    Short,
    Open,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredAction {
    Enter {
        side: StoredSide,
        quantity: StoredFixed,
        protection: Option<StoredProtection>,
    },
    Flatten,
    CancelOrders,
    LockAccount,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredSide {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct StoredProtection {
    stop_ticks: u32,
    target_ticks: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredRepeat {
    Once,
    PerSession { maximum: u32 },
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct StoredNodePosition {
    node: u16,
    x: i32,
    y: i32,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredOutcome {
    Executed { steps: Vec<StoredStepResult> },
    Blocked { blockers: Vec<StoredBlocker> },
    Interrupted,
}

#[derive(Deserialize, Serialize)]
struct StoredStepResult {
    node: u16,
    outcome: StoredStepOutcome,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredStepOutcome {
    Branch { held: bool },
    Action { result: StoredActionResult },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredActionResult {
    Placed { client_order_id: String },
    Flattened,
    Cancelled { orders: usize },
    Locked,
    Refused { detail: String },
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredBlocker {
    AccountMissing,
    InstrumentTermsMissing,
    AccountLocked,
    BrokerUnreachable,
    QuoteUnavailable,
}

impl StoredFixed {
    const fn from_fixed(value: FixedPoint) -> Self {
        Self {
            units: value.units(),
            scale: value.scale(),
        }
    }

    fn into_fixed(self) -> Result<FixedPoint, WorkflowError> {
        FixedPoint::try_new(self.units, self.scale).map_err(corrupt)
    }
}

impl StoredDirection {
    const fn from_direction(direction: PriceDirection) -> Self {
        match direction {
            PriceDirection::Above => Self::Above,
            PriceDirection::Below => Self::Below,
        }
    }

    const fn into_direction(self) -> PriceDirection {
        match self {
            Self::Above => PriceDirection::Above,
            Self::Below => PriceDirection::Below,
        }
    }
}

impl StoredZone {
    const fn from_zone(zone: WorkflowTimeZone) -> Self {
        match zone {
            WorkflowTimeZone::Utc => Self::Utc,
            WorkflowTimeZone::Chicago => Self::Chicago,
        }
    }

    const fn into_zone(self) -> WorkflowTimeZone {
        match self {
            Self::Utc => WorkflowTimeZone::Utc,
            Self::Chicago => WorkflowTimeZone::Chicago,
        }
    }
}

impl StoredNodePosition {
    fn from_layout(layout: &WorkflowLayout) -> Vec<Self> {
        layout
            .positions
            .iter()
            .map(|position| Self {
                node: position.node.0,
                x: position.x,
                y: position.y,
            })
            .collect()
    }

    const fn into_position(self) -> WorkflowNodePosition {
        WorkflowNodePosition {
            node: WorkflowNodeId(self.node),
            x: self.x,
            y: self.y,
        }
    }
}

impl StoredCondition {
    const fn from_condition(condition: WorkflowCondition) -> Self {
        match condition {
            WorkflowCondition::Position(requirement) => Self::Position {
                requirement: match requirement {
                    PositionRequirement::Flat => StoredRequirement::Flat,
                    PositionRequirement::Long => StoredRequirement::Long,
                    PositionRequirement::Short => StoredRequirement::Short,
                    PositionRequirement::Open => StoredRequirement::Open,
                },
            },
            WorkflowCondition::Between {
                zone,
                start_minute,
                end_minute,
            } => Self::Between {
                zone: StoredZone::from_zone(zone),
                start_minute,
                end_minute,
            },
            WorkflowCondition::Price { direction, level } => Self::Price {
                direction: StoredDirection::from_direction(direction),
                level: StoredFixed::from_fixed(level),
            },
        }
    }

    fn into_condition(self) -> Result<WorkflowCondition, WorkflowError> {
        Ok(match self {
            Self::Position { requirement } => WorkflowCondition::Position(match requirement {
                StoredRequirement::Flat => PositionRequirement::Flat,
                StoredRequirement::Long => PositionRequirement::Long,
                StoredRequirement::Short => PositionRequirement::Short,
                StoredRequirement::Open => PositionRequirement::Open,
            }),
            Self::Between {
                zone,
                start_minute,
                end_minute,
            } => WorkflowCondition::Between {
                zone: zone.into_zone(),
                start_minute,
                end_minute,
            },
            Self::Price { direction, level } => WorkflowCondition::Price {
                direction: direction.into_direction(),
                level: level.into_fixed()?,
            },
        })
    }
}

impl StoredAction {
    const fn from_action(action: WorkflowAction) -> Self {
        match action {
            WorkflowAction::Enter {
                side,
                quantity,
                protection,
            } => Self::Enter {
                side: match side {
                    OrderSide::Buy => StoredSide::Buy,
                    OrderSide::Sell => StoredSide::Sell,
                },
                quantity: StoredFixed::from_fixed(quantity),
                protection: match protection {
                    Some(protection) => Some(StoredProtection {
                        stop_ticks: protection.stop_ticks,
                        target_ticks: protection.target_ticks,
                    }),
                    None => None,
                },
            },
            WorkflowAction::Flatten => Self::Flatten,
            WorkflowAction::CancelOrders => Self::CancelOrders,
            WorkflowAction::LockAccount => Self::LockAccount,
        }
    }

    fn into_action(self) -> Result<WorkflowAction, WorkflowError> {
        Ok(match self {
            Self::Enter {
                side,
                quantity,
                protection,
            } => WorkflowAction::Enter {
                side: match side {
                    StoredSide::Buy => OrderSide::Buy,
                    StoredSide::Sell => OrderSide::Sell,
                },
                quantity: quantity.into_fixed()?,
                protection: protection.map(|protection| WorkflowProtection {
                    stop_ticks: protection.stop_ticks,
                    target_ticks: protection.target_ticks,
                }),
            },
            Self::Flatten => WorkflowAction::Flatten,
            Self::CancelOrders => WorkflowAction::CancelOrders,
            Self::LockAccount => WorkflowAction::LockAccount,
        })
    }
}

impl StoredNode {
    fn from_node(node: &WorkflowNode) -> Self {
        let id = |link: Option<WorkflowNodeId>| link.map(|link| link.0);
        Self {
            id: node.id.0,
            step: match node.step {
                WorkflowStep::If {
                    condition,
                    then,
                    otherwise,
                } => StoredStep::If {
                    condition: StoredCondition::from_condition(condition),
                    then: id(then),
                    otherwise: id(otherwise),
                },
                WorkflowStep::Action { action, next } => StoredStep::Action {
                    action: StoredAction::from_action(action),
                    next: id(next),
                },
            },
        }
    }

    fn into_node(self) -> Result<WorkflowNode, WorkflowError> {
        let id = |link: Option<u16>| link.map(WorkflowNodeId);
        Ok(WorkflowNode {
            id: WorkflowNodeId(self.id),
            step: match self.step {
                StoredStep::If {
                    condition,
                    then,
                    otherwise,
                } => WorkflowStep::If {
                    condition: condition.into_condition()?,
                    then: id(then),
                    otherwise: id(otherwise),
                },
                StoredStep::Action { action, next } => WorkflowStep::Action {
                    action: action.into_action()?,
                    next: id(next),
                },
            },
        })
    }
}

impl StoredDefinition {
    fn from_definition(definition: &WorkflowDefinition) -> Self {
        Self {
            name: definition.name.clone(),
            instrument_id: definition.instrument_id.as_str().to_string(),
            trigger: match definition.trigger {
                WorkflowTrigger::PriceCrosses { direction, level } => StoredTrigger::PriceCrosses {
                    direction: StoredDirection::from_direction(direction),
                    level: StoredFixed::from_fixed(level),
                },
                WorkflowTrigger::TimeOfDay(time) => StoredTrigger::TimeOfDay {
                    zone: StoredZone::from_zone(time.zone),
                    minute_of_day: time.minute_of_day,
                },
            },
            start: definition.start.0,
            nodes: definition.nodes.iter().map(StoredNode::from_node).collect(),
            repeat: match definition.limits.repeat {
                WorkflowRepeat::Once => StoredRepeat::Once,
                WorkflowRepeat::PerSession { maximum } => StoredRepeat::PerSession { maximum },
            },
            cooldown_seconds: definition.limits.cooldown_seconds,
            expires_unix_nanos: definition.limits.expires_unix_nanos,
        }
    }

    fn into_definition(
        self,
        workflow_id: String,
        revision: u32,
        account_id: String,
    ) -> Result<WorkflowDefinition, WorkflowError> {
        let trigger = match self.trigger {
            StoredTrigger::PriceCrosses { direction, level } => WorkflowTrigger::PriceCrosses {
                direction: direction.into_direction(),
                level: level.into_fixed()?,
            },
            StoredTrigger::TimeOfDay {
                zone,
                minute_of_day,
            } => WorkflowTrigger::TimeOfDay(WorkflowClockTime {
                zone: zone.into_zone(),
                minute_of_day,
            }),
        };
        let nodes = self
            .nodes
            .into_iter()
            .map(StoredNode::into_node)
            .collect::<Result<Vec<_>, WorkflowError>>()?;
        Ok(WorkflowDefinition {
            workflow_id,
            revision,
            name: self.name,
            account_id: TradingAccountId::try_new(account_id).map_err(corrupt)?,
            instrument_id: InstrumentId::try_new(self.instrument_id).map_err(corrupt)?,
            trigger,
            start: WorkflowNodeId(self.start),
            nodes,
            limits: WorkflowLimits {
                repeat: match self.repeat {
                    StoredRepeat::Once => WorkflowRepeat::Once,
                    StoredRepeat::PerSession { maximum } => WorkflowRepeat::PerSession { maximum },
                },
                cooldown_seconds: self.cooldown_seconds,
                expires_unix_nanos: self.expires_unix_nanos,
            },
        })
    }
}

impl StoredBlocker {
    const fn from_blocker(blocker: WorkflowBlocker) -> Self {
        match blocker {
            WorkflowBlocker::AccountMissing => Self::AccountMissing,
            WorkflowBlocker::InstrumentTermsMissing => Self::InstrumentTermsMissing,
            WorkflowBlocker::AccountLocked => Self::AccountLocked,
            WorkflowBlocker::BrokerUnreachable => Self::BrokerUnreachable,
            WorkflowBlocker::QuoteUnavailable => Self::QuoteUnavailable,
        }
    }

    const fn into_blocker(self) -> WorkflowBlocker {
        match self {
            Self::AccountMissing => WorkflowBlocker::AccountMissing,
            Self::InstrumentTermsMissing => WorkflowBlocker::InstrumentTermsMissing,
            Self::AccountLocked => WorkflowBlocker::AccountLocked,
            Self::BrokerUnreachable => WorkflowBlocker::BrokerUnreachable,
            Self::QuoteUnavailable => WorkflowBlocker::QuoteUnavailable,
        }
    }
}

impl StoredActionResult {
    fn from_result(result: &WorkflowActionResult) -> Self {
        match result {
            WorkflowActionResult::Placed { client_order_id } => Self::Placed {
                client_order_id: client_order_id.as_str().to_string(),
            },
            WorkflowActionResult::Flattened => Self::Flattened,
            WorkflowActionResult::Cancelled { orders } => Self::Cancelled { orders: *orders },
            WorkflowActionResult::Locked => Self::Locked,
            WorkflowActionResult::Refused { detail } => Self::Refused {
                detail: detail.clone(),
            },
        }
    }

    fn into_result(self) -> Result<WorkflowActionResult, WorkflowError> {
        Ok(match self {
            Self::Placed { client_order_id } => WorkflowActionResult::Placed {
                client_order_id: ClientOrderId::try_new(client_order_id).map_err(corrupt)?,
            },
            Self::Flattened => WorkflowActionResult::Flattened,
            Self::Cancelled { orders } => WorkflowActionResult::Cancelled { orders },
            Self::Locked => WorkflowActionResult::Locked,
            Self::Refused { detail } => WorkflowActionResult::Refused { detail },
        })
    }
}

impl StoredOutcome {
    fn from_outcome(outcome: &WorkflowFiringOutcome) -> Self {
        match outcome {
            WorkflowFiringOutcome::Executed(steps) => Self::Executed {
                steps: steps
                    .iter()
                    .map(|step| StoredStepResult {
                        node: step.node.0,
                        outcome: match &step.outcome {
                            WorkflowStepOutcome::Branch(held) => {
                                StoredStepOutcome::Branch { held: *held }
                            }
                            WorkflowStepOutcome::Action(result) => StoredStepOutcome::Action {
                                result: StoredActionResult::from_result(result),
                            },
                        },
                    })
                    .collect(),
            },
            WorkflowFiringOutcome::Blocked(blockers) => Self::Blocked {
                blockers: blockers
                    .iter()
                    .copied()
                    .map(StoredBlocker::from_blocker)
                    .collect(),
            },
            WorkflowFiringOutcome::Interrupted => Self::Interrupted,
        }
    }

    fn into_outcome(self) -> Result<WorkflowFiringOutcome, WorkflowError> {
        Ok(match self {
            Self::Executed { steps } => WorkflowFiringOutcome::Executed(
                steps
                    .into_iter()
                    .map(|step| {
                        Ok(WorkflowStepResult {
                            node: WorkflowNodeId(step.node),
                            outcome: match step.outcome {
                                StoredStepOutcome::Branch { held } => {
                                    WorkflowStepOutcome::Branch(held)
                                }
                                StoredStepOutcome::Action { result } => {
                                    WorkflowStepOutcome::Action(result.into_result()?)
                                }
                            },
                        })
                    })
                    .collect::<Result<Vec<_>, WorkflowError>>()?,
            ),
            Self::Blocked { blockers } => WorkflowFiringOutcome::Blocked(
                blockers
                    .into_iter()
                    .map(StoredBlocker::into_blocker)
                    .collect(),
            ),
            Self::Interrupted => WorkflowFiringOutcome::Interrupted,
        })
    }
}
