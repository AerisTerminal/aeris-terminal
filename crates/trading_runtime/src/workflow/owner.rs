//! The trading owner's side of workflows: registration, arming, evaluation against its own
//! verified quotes and clock, and the actions of each firing, which go through the same
//! admission, risk and venue paths as a manual command.

use super::ledger::TradeCycle;
use super::{
    BracketStrategyTemplate, BracketTarget, FlattenScope, PlaceOrder, Position, RiskLockKind,
    TradingCore, TradingInstrument, TradingService, VenueRoute, instrument_tick_units, respond,
};
use crate::risk::session::RiskSessionBoundary;
use crate::service::{Command, WorkflowCommand};
use crate::workflow::{
    MAXIMUM_WORKFLOW_TRIGGERS, MAXIMUM_WORKFLOWS, PriceDirection, WORKFLOW_FIRING_HISTORY,
    WorkflowAction, WorkflowActionResult, WorkflowBlocker, WorkflowCheck, WorkflowCompare,
    WorkflowCondition, WorkflowDefinition, WorkflowError, WorkflowFiring, WorkflowFiringOutcome,
    WorkflowInvalid, WorkflowLayout, WorkflowNodeId, WorkflowPathStep, WorkflowProtection,
    WorkflowRecord, WorkflowRepeat, WorkflowScope, WorkflowState, WorkflowStepOutcome,
    WorkflowStepResult, WorkflowTargets, WorkflowTrigger, WorkflowView, time_window_holds,
    workflow_client_order_id,
};
use aeris_instruments::InstrumentId;
use aeris_trading::{
    ClientOrderId, FixedPoint, OrderSide, OrderType, TimeInForce, TradingAccountId,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A time trigger reached more than this late (the computer slept or the owner stalled) is
/// skipped instead of firing away from its moment.
const TIME_TRIGGER_GRACE_NANOS: i64 = 60_000_000_000;
const NANOS_PER_MINUTE: i64 = 60_000_000_000;
/// A position or condition workflow's actions can open or close a position or change what a
/// condition reads, firing another; the owner follows such a chain within one step this many
/// times, and the next step goes on.
const STEP_EVALUATION_PASSES: usize = 4;

/// A condition trigger by its position in the definition, and the symbol it is read for
/// (`None` for the whole account).
type ConditionKey = (usize, Option<InstrumentId>);

/// One workflow as the owner holds it between steps.
pub(crate) struct WorkflowEntry {
    record: WorkflowRecord,
    /// The newest firings, oldest first.
    firings: VecDeque<WorkflowFiring>,
    /// Per price trigger, by its position in the definition: whether the last quote since
    /// arming was at or past its level; `None` until the first quote, which never fires.
    at_level: [Option<bool>; MAXIMUM_WORKFLOW_TRIGGERS],
    next_due_unix_nanos: Option<i64>,
    /// The position in each workflow symbol the last owner step saw, for the position
    /// triggers; `None` until the first step after arming, loading or accepting, which never
    /// fires. Bounded by the symbols the account has traded.
    position_marks: Option<BTreeMap<InstrumentId, PositionMark>>,
    /// The best exit price of each open position in the workflow's symbols seen while
    /// armed, for pullback checks; only open positions are kept.
    best_exits: BTreeMap<InstrumentId, BestExit>,
    /// Whether each condition trigger held at the last owner step; `None` until the first
    /// step after arming, loading or accepting, which never fires. Bounded by the triggers
    /// times the account's open positions.
    condition_marks: Option<BTreeMap<ConditionKey, bool>>,
    /// The next whole minute at which an armed condition trigger that reads the clock is
    /// read again, though no command or quote arrives.
    next_check_unix_nanos: Option<i64>,
    /// The day's realized P&L a condition trigger last read, kept while the owner revision
    /// and the session stay the same so a quote-only step does not sum the fills again.
    day_realized: Option<DayRealized>,
}

/// The realized P&L of the account's current risk session as of one owner revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DayRealized {
    revision: u64,
    session_start_unix_nanos: i64,
    realized: FixedPoint,
}

/// Which trade cycle the account's position is in and when it opened. Every close or
/// reversal moves the cycle and every open moves the open time, so two marks differ
/// whenever a position opened or closed between them, even within one step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PositionMark {
    cycle_id: i64,
    opened_unix_nanos: Option<i64>,
}

impl PositionMark {
    /// A symbol before its first fill.
    const UNTRADED: Self = Self {
        cycle_id: 0,
        opened_unix_nanos: None,
    };
}

/// The most favorable exit price of one position: the highest bid while long, the lowest
/// ask while short. It starts from the average entry, a price the market traded at while
/// the position was open, so after a restart a pullback can only read smaller than it was.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BestExit {
    mark: PositionMark,
    price: FixedPoint,
}

impl WorkflowEntry {
    const fn new(record: WorkflowRecord, firings: VecDeque<WorkflowFiring>) -> Self {
        Self {
            record,
            firings,
            at_level: [None; MAXIMUM_WORKFLOW_TRIGGERS],
            next_due_unix_nanos: None,
            position_marks: None,
            best_exits: BTreeMap::new(),
            condition_marks: None,
            next_check_unix_nanos: None,
            day_realized: None,
        }
    }
}

impl TradingService {
    /// Accepts a new workflow, or a new revision of one, exactly as the user reviewed it,
    /// with where its nodes sit on the board. Every accepted revision starts paused.
    ///
    /// # Errors
    /// Returns the refused block or layout, an unknown account or instrument, a revision
    /// that does not advance, the workflow bound, or an owner failure.
    pub fn register_workflow(
        &self,
        definition: WorkflowDefinition,
        layout: WorkflowLayout,
    ) -> Result<WorkflowView, WorkflowError> {
        self.request(|reply| {
            Command::Workflow(WorkflowCommand::Register(
                Box::new((definition, layout)),
                reply,
            ))
        })
        .map_err(WorkflowError::Owner)?
    }

    /// Moves the named revision's nodes on the board. The revision, its state, counters and
    /// history stay as they are.
    ///
    /// # Errors
    /// Returns an error for an unknown or stale revision, a position for an unknown node or
    /// off the board, or an owner failure.
    pub fn set_workflow_layout(
        &self,
        workflow_id: String,
        revision: u32,
        layout: WorkflowLayout,
    ) -> Result<WorkflowView, WorkflowError> {
        self.request(|reply| {
            Command::Workflow(WorkflowCommand::SetLayout(
                workflow_id,
                revision,
                layout,
                reply,
            ))
        })
        .map_err(WorkflowError::Owner)?
    }

    /// Arms the named revision so the owner evaluates its trigger.
    ///
    /// # Errors
    /// Returns an error for an unknown or stale revision, a completed or expired workflow,
    /// a blocker that cannot clear while armed, or an owner failure.
    pub fn arm_workflow(
        &self,
        workflow_id: String,
        revision: u32,
    ) -> Result<WorkflowView, WorkflowError> {
        self.request(|reply| Command::Workflow(WorkflowCommand::Arm(workflow_id, revision, reply)))
            .map_err(WorkflowError::Owner)?
    }

    /// Stops evaluating the named revision; its counters and history stay.
    ///
    /// # Errors
    /// Returns an error for an unknown or stale revision or an owner failure.
    pub fn pause_workflow(
        &self,
        workflow_id: String,
        revision: u32,
    ) -> Result<WorkflowView, WorkflowError> {
        self.request(|reply| {
            Command::Workflow(WorkflowCommand::Pause(workflow_id, revision, reply))
        })
        .map_err(WorkflowError::Owner)?
    }

    /// Removes a workflow and its firing history.
    ///
    /// # Errors
    /// Returns an error for an unknown workflow or an owner failure.
    pub fn delete_workflow(&self, workflow_id: String) -> Result<(), WorkflowError> {
        self.request(|reply| Command::Workflow(WorkflowCommand::Delete(workflow_id, reply)))
            .map_err(WorkflowError::Owner)?
    }
}

impl TradingCore {
    pub(super) fn handle_workflow_command(&mut self, command: WorkflowCommand) {
        match command {
            WorkflowCommand::Register(accepted, reply) => {
                let (definition, layout) = *accepted;
                respond(&reply, Ok(self.register_workflow(definition, layout)));
            }
            WorkflowCommand::SetLayout(workflow_id, revision, layout, reply) => {
                respond(
                    &reply,
                    Ok(self.set_workflow_layout(&workflow_id, revision, layout)),
                );
            }
            WorkflowCommand::Arm(workflow_id, revision, reply) => {
                respond(&reply, Ok(self.arm_workflow(&workflow_id, revision)));
            }
            WorkflowCommand::Pause(workflow_id, revision, reply) => {
                respond(&reply, Ok(self.pause_workflow(&workflow_id, revision)));
            }
            WorkflowCommand::Delete(workflow_id, reply) => {
                respond(&reply, Ok(self.delete_workflow(&workflow_id)));
            }
        }
    }

    pub(super) fn load_workflow_entries(&mut self) -> Result<(), WorkflowError> {
        for (record, firings) in self.store.load_workflows()? {
            let next_due_unix_nanos = next_due(&record, self.command_now);
            let next_check_unix_nanos = next_condition_check(&record, self.command_now);
            let workflow_id = record.definition.workflow_id.clone();
            let mut entry = WorkflowEntry::new(record, firings.into());
            entry.next_due_unix_nanos = next_due_unix_nanos;
            entry.next_check_unix_nanos = next_check_unix_nanos;
            self.workflows.insert(workflow_id, entry);
        }
        Ok(())
    }

    pub(super) fn register_workflow(
        &mut self,
        definition: WorkflowDefinition,
        layout: WorkflowLayout,
    ) -> Result<WorkflowView, WorkflowError> {
        definition.validate().map_err(WorkflowError::Invalid)?;
        layout
            .validate(&definition)
            .map_err(WorkflowError::Invalid)?;
        let account = self
            .state
            .accounts
            .get(&definition.account_id)
            .ok_or(WorkflowError::UnknownAccount)?;
        let instrument_scales = match definition.scope.instrument() {
            Some(instrument_id) => {
                let instrument = self
                    .state
                    .instruments
                    .get(instrument_id)
                    .ok_or(WorkflowError::UnknownInstrument)?;
                Some((instrument.price_scale, instrument.quantity_scale))
            }
            None => None,
        };
        check_scales(&definition, instrument_scales, account.currency_scale)?;
        match self.workflows.get(&definition.workflow_id) {
            Some(previous) if definition.revision <= previous.record.definition.revision => {
                return Err(WorkflowError::RevisionMustAdvance {
                    current: previous.record.definition.revision,
                });
            }
            None if self.workflows.len() >= MAXIMUM_WORKFLOWS => {
                return Err(WorkflowError::TooManyWorkflows);
            }
            _ => {}
        }
        let workflow_id = definition.workflow_id.clone();
        let record = WorkflowRecord::accepted(
            definition,
            layout,
            self.workflows.get(&workflow_id).map(|entry| &entry.record),
        );
        self.store.put_workflow(&record, None)?;
        let firings = self
            .workflows
            .remove(&workflow_id)
            .map(|entry| entry.firings)
            .unwrap_or_default();
        self.workflows
            .insert(workflow_id.clone(), WorkflowEntry::new(record, firings));
        self.bump_revision().map_err(WorkflowError::Owner)?;
        self.workflow_view(&workflow_id)
    }

    pub(super) fn arm_workflow(
        &mut self,
        workflow_id: &str,
        revision: u32,
    ) -> Result<WorkflowView, WorkflowError> {
        let mut record = self.current_workflow(workflow_id, revision)?.clone();
        match record.state {
            WorkflowState::Armed => return self.workflow_view(workflow_id),
            WorkflowState::Completed => return Err(WorkflowError::Completed),
            WorkflowState::Expired => return Err(WorkflowError::Expired),
            WorkflowState::Paused => {}
        }
        if is_expired(&record.definition, self.command_now) {
            record.state = WorkflowState::Expired;
            self.replace_workflow_record(record)?;
            return Err(WorkflowError::Expired);
        }
        let blockers = self
            .workflow_blockers(&record.definition)
            .into_iter()
            .filter(|blocker| blocker.prevents_arming())
            .collect::<Vec<_>>();
        if !blockers.is_empty() {
            return Err(WorkflowError::NotReady(blockers));
        }
        record.state = WorkflowState::Armed;
        self.replace_workflow_record(record)?;
        self.workflow_view(workflow_id)
    }

    pub(super) fn pause_workflow(
        &mut self,
        workflow_id: &str,
        revision: u32,
    ) -> Result<WorkflowView, WorkflowError> {
        let mut record = self.current_workflow(workflow_id, revision)?.clone();
        if record.state == WorkflowState::Armed {
            record.state = WorkflowState::Paused;
            self.replace_workflow_record(record)?;
        }
        self.workflow_view(workflow_id)
    }

    pub(super) fn set_workflow_layout(
        &mut self,
        workflow_id: &str,
        revision: u32,
        layout: WorkflowLayout,
    ) -> Result<WorkflowView, WorkflowError> {
        let record = self.current_workflow(workflow_id, revision)?;
        layout
            .validate(&record.definition)
            .map_err(WorkflowError::Invalid)?;
        if record.layout != layout {
            self.store.put_workflow_layout(workflow_id, &layout)?;
            if let Some(entry) = self.workflows.get_mut(workflow_id) {
                entry.record.layout = layout;
            }
            self.bump_revision().map_err(WorkflowError::Owner)?;
        }
        self.workflow_view(workflow_id)
    }

    pub(super) fn delete_workflow(&mut self, workflow_id: &str) -> Result<(), WorkflowError> {
        if !self.workflows.contains_key(workflow_id) {
            return Err(WorkflowError::UnknownWorkflow);
        }
        self.store.delete_workflow(workflow_id)?;
        self.workflows.remove(workflow_id);
        self.bump_revision().map_err(WorkflowError::Owner)
    }

    /// Forgets the workflows of a deleted account; their rows go with the account's.
    pub(super) fn purge_account_workflows(&mut self, account_id: &TradingAccountId) {
        self.workflows
            .retain(|_, entry| &entry.record.definition.account_id != account_id);
    }

    pub(super) fn workflow_views(&self) -> Vec<WorkflowView> {
        self.workflows
            .keys()
            .filter_map(|workflow_id| self.workflow_view(workflow_id).ok())
            .collect()
    }

    /// Every one-symbol workflow's instrument, so the working set carries the terms that
    /// name and price it.
    pub(super) fn workflow_instruments(&self) -> impl Iterator<Item = &InstrumentId> {
        self.workflows
            .values()
            .filter_map(|entry| entry.record.definition.scope.instrument())
    }

    /// Instruments whose quotes an armed workflow needs, for the owner's quote interest: a
    /// one-symbol workflow's instrument, and every open position of the account when a
    /// workflow on any symbol has a check that reads the quote.
    pub(super) fn armed_workflow_instruments(&self) -> BTreeSet<InstrumentId> {
        let mut instruments = BTreeSet::new();
        for entry in self.workflows.values() {
            let definition = &entry.record.definition;
            if entry.record.state != WorkflowState::Armed {
                continue;
            }
            match definition.scope.instrument() {
                Some(instrument_id) => {
                    instruments.insert(instrument_id.clone());
                }
                None if definition.checks().any(WorkflowCheck::reads_quote) => {
                    instruments.extend(
                        open_positions(&self.state.positions, &definition.account_id)
                            .map(|position| position.instrument_id.clone()),
                    );
                }
                None => {}
            }
        }
        instruments
    }

    /// The next timed trigger, clock condition read or expiry of an armed workflow.
    pub(crate) fn next_workflow_deadline_unix_nanos(&self) -> Option<i64> {
        self.workflows
            .values()
            .filter(|entry| entry.record.state == WorkflowState::Armed)
            .flat_map(|entry| {
                entry
                    .next_due_unix_nanos
                    .into_iter()
                    .chain(entry.next_check_unix_nanos)
                    .chain(entry.record.definition.limits.expires_unix_nanos)
            })
            .min()
    }

    /// Fires every armed workflow with a price trigger whose market moved across its level
    /// since the previous quote; several crossings in one batch fire it once. Also keeps the
    /// best exit price of each armed workflow's open position. Runs after a market batch,
    /// outside quote planning.
    pub(super) fn evaluate_price_workflows(&mut self) {
        let mut fired = Vec::new();
        for (workflow_id, entry) in &mut self.workflows {
            if entry.record.state != WorkflowState::Armed {
                continue;
            }
            let definition = &entry.record.definition;
            let watched = open_positions(&self.state.positions, &definition.account_id)
                .filter(|position| {
                    definition
                        .scope
                        .instrument()
                        .is_none_or(|instrument_id| instrument_id == &position.instrument_id)
                })
                .collect::<Vec<_>>();
            entry.best_exits.retain(|instrument_id, _| {
                watched
                    .iter()
                    .any(|position| &position.instrument_id == instrument_id)
            });
            for position in watched {
                let Some(quote) = self.quote_book.observation(&position.instrument_id) else {
                    continue;
                };
                let key = (position.account_id.clone(), position.instrument_id.clone());
                let mark = position_mark(&self.state.trade_cycles, &key);
                let kept = entry.best_exits.get(&position.instrument_id).copied();
                if let Some(best) = best_exit(kept, mark, position, quote.bid, quote.ask) {
                    entry
                        .best_exits
                        .insert(position.instrument_id.clone(), best);
                }
            }
            let Some(quote) = definition
                .scope
                .instrument()
                .and_then(|instrument_id| self.quote_book.observation(instrument_id))
            else {
                continue;
            };
            let mut crossed = false;
            for (trigger, last) in entry
                .record
                .definition
                .triggers
                .iter()
                .zip(&mut entry.at_level)
            {
                let WorkflowTrigger::PriceCrosses { direction, level } = *trigger else {
                    continue;
                };
                let Some(at_level) = price_holds(direction, level, quote.bid, quote.ask) else {
                    continue;
                };
                crossed |= *last == Some(false) && at_level;
                *last = Some(at_level);
            }
            if crossed {
                fired.push((workflow_id.clone(), definition.all_targets()));
            }
        }
        for (workflow_id, targets) in fired {
            self.fire_workflow(&workflow_id, targets);
        }
    }

    /// Expires armed workflows past their expiry and fires every due time trigger.
    pub(super) fn run_workflow_timer(&mut self) {
        let now = self.command_now;
        let expired = self
            .workflows
            .values()
            .filter(|entry| {
                entry.record.state == WorkflowState::Armed
                    && is_expired(&entry.record.definition, now)
            })
            .map(|entry| entry.record.clone())
            .collect::<Vec<_>>();
        for mut record in expired {
            record.state = WorkflowState::Expired;
            if let Err(error) = self.replace_workflow_record(record) {
                aeris_observability::diagnostic!("workflow expiry persistence failed: {error}");
            }
        }
        let mut fired = Vec::new();
        for (workflow_id, entry) in &mut self.workflows {
            if entry.record.state != WorkflowState::Armed {
                continue;
            }
            let Some(due) = entry.next_due_unix_nanos.filter(|due| *due <= now) else {
                continue;
            };
            entry.next_due_unix_nanos = entry.record.definition.next_timed_trigger_after(now);
            if now.saturating_sub(due) <= TIME_TRIGGER_GRACE_NANOS {
                fired.push((workflow_id.clone(), entry.record.definition.all_targets()));
            }
        }
        for (workflow_id, targets) in fired {
            self.fire_workflow(&workflow_id, targets);
        }
    }

    /// Fires every armed workflow with a position trigger whose position opened or closed,
    /// or with a condition trigger that became true, since the previous owner step; one
    /// workflow both triggers fire in a step fires once, on the symbols of both. Runs after
    /// every command, market batch and timer.
    pub(super) fn evaluate_step_workflows(&mut self) {
        for _ in 0..STEP_EVALUATION_PASSES {
            let mut fired = BTreeMap::<String, WorkflowTargets>::new();
            for (workflow_id, targets) in self
                .moved_position_workflows()
                .into_iter()
                .chain(self.met_condition_workflows())
            {
                let merged = match fired.remove(&workflow_id) {
                    Some(earlier) => earlier.union(targets),
                    None => targets,
                };
                fired.insert(workflow_id, merged);
            }
            if fired.is_empty() {
                return;
            }
            for (workflow_id, targets) in fired {
                self.fire_workflow(&workflow_id, targets);
            }
        }
    }

    /// The armed workflows with a position trigger whose position opened or closed since
    /// the previous read, with the symbols that moved.
    fn moved_position_workflows(&mut self) -> Vec<(String, WorkflowTargets)> {
        let mut fired = Vec::new();
        for (workflow_id, entry) in &mut self.workflows {
            if entry.record.state != WorkflowState::Armed {
                continue;
            }
            let definition = &entry.record.definition;
            let marks = workflow_position_marks(&self.state.trade_cycles, definition);
            let Some(previous) = entry.position_marks.replace(marks.clone()) else {
                continue;
            };
            let mut moved = BTreeSet::new();
            for (instrument_id, mark) in marks {
                let before = previous
                    .get(&instrument_id)
                    .copied()
                    .unwrap_or(PositionMark::UNTRADED);
                let open = self
                    .state
                    .positions
                    .get(&(definition.account_id.clone(), instrument_id.clone()))
                    .is_some_and(|position| position.net_quantity.units() != 0);
                let closed = mark.cycle_id != before.cycle_id;
                let opened = (mark.opened_unix_nanos.is_some()
                    && mark.opened_unix_nanos != before.opened_unix_nanos)
                    || (closed && open);
                let triggered = definition.triggers.iter().any(|trigger| match trigger {
                    WorkflowTrigger::PositionOpened => opened,
                    WorkflowTrigger::PositionClosed => closed,
                    WorkflowTrigger::PriceCrosses { .. }
                    | WorkflowTrigger::TimeOfDay(_)
                    | WorkflowTrigger::Every { .. }
                    | WorkflowTrigger::ConditionMet(_) => false,
                });
                if triggered {
                    moved.insert(instrument_id);
                }
            }
            if !moved.is_empty() {
                fired.push((workflow_id.clone(), WorkflowTargets::Only(moved)));
            }
        }
        fired
    }

    /// The armed workflows with a condition trigger read false at the previous step and
    /// true now, with the symbols that became true. Reads only armed workflows that have a
    /// condition trigger; one that cannot read a symbol now keeps that symbol's last answer.
    fn met_condition_workflows(&mut self) -> Vec<(String, WorkflowTargets)> {
        let now = self.command_now;
        let watched = self
            .workflows
            .iter()
            .filter(|(_, entry)| {
                entry.record.state == WorkflowState::Armed
                    && entry
                        .record
                        .definition
                        .trigger_conditions()
                        .next()
                        .is_some()
            })
            .map(|(workflow_id, _)| workflow_id.clone())
            .collect::<Vec<_>>();
        let mut fired = Vec::new();
        for workflow_id in watched {
            let Some(entry) = self.workflows.get(&workflow_id) else {
                continue;
            };
            let day_realized = self.condition_day_realized(entry);
            let day_pnl = day_realized.map(|realized| {
                realized.and_then(|realized| {
                    self.day_pnl(&entry.record.definition.account_id, realized.realized)
                })
            });
            let readings = self.read_trigger_conditions(entry, day_pnl);
            let any_symbol = entry.record.definition.scope == WorkflowScope::AnySymbol;
            let all_targets = entry.record.definition.all_targets();
            let Some(entry) = self.workflows.get_mut(&workflow_id) else {
                continue;
            };
            entry.next_check_unix_nanos = next_condition_check(&entry.record, now);
            if let Some(Ok(realized)) = day_realized {
                entry.day_realized = Some(realized);
            }
            let previous = entry.condition_marks.take();
            let mut marks = BTreeMap::new();
            let mut became = BTreeSet::new();
            for (key, held) in readings {
                let before = previous.as_ref().and_then(|previous| {
                    previous
                        .get(&key)
                        .copied()
                        .or_else(|| any_symbol.then_some(false))
                });
                let Ok(held) = held else {
                    marks.extend(before.map(|before| (key, before)));
                    continue;
                };
                if held && before == Some(false) {
                    became.insert(key.1.clone());
                }
                marks.insert(key, held);
            }
            entry.condition_marks = Some(marks);
            if became.is_empty() {
                continue;
            }
            let targets = if !any_symbol || became.contains(&None) {
                all_targets
            } else {
                WorkflowTargets::Only(became.into_iter().flatten().collect())
            };
            fired.push((workflow_id, targets));
        }
        fired
    }

    /// The day's realized P&L when one of the workflow's condition triggers reads the day's
    /// P&L: the kept value while the owner revision and the session have not moved since,
    /// otherwise summed afresh.
    fn condition_day_realized(
        &self,
        entry: &WorkflowEntry,
    ) -> Option<Result<DayRealized, WorkflowBlocker>> {
        let definition = &entry.record.definition;
        let reads_day = definition
            .trigger_conditions()
            .flat_map(WorkflowCondition::checks)
            .any(|check| matches!(check, WorkflowCheck::DayProfit { .. }));
        if !reads_day {
            return None;
        }
        let session_start = self.day_session_start(&definition.account_id);
        Some(
            entry
                .day_realized
                .filter(|kept| {
                    kept.revision == self.state.revision
                        && kept.session_start_unix_nanos == session_start
                })
                .map_or_else(|| self.day_realized(&definition.account_id), Ok),
        )
    }

    /// Reads every condition trigger of the workflow for each symbol it is read for: the
    /// one instrument; on any symbol each open position when a check reads the position,
    /// otherwise (or while flat everywhere) the whole account once.
    fn read_trigger_conditions(
        &self,
        entry: &WorkflowEntry,
        day_pnl: Option<Result<FixedPoint, WorkflowBlocker>>,
    ) -> Vec<(ConditionKey, Result<bool, WorkflowBlocker>)> {
        let definition = &entry.record.definition;
        let account_id = &definition.account_id;
        let mut readings = Vec::new();
        for (index, trigger) in definition.triggers.iter().enumerate() {
            let WorkflowTrigger::ConditionMet(condition) = trigger else {
                continue;
            };
            let symbols = match &definition.scope {
                WorkflowScope::Instrument(instrument_id) => vec![Some(instrument_id.clone())],
                WorkflowScope::AnySymbol
                    if condition.checks().any(WorkflowCheck::reads_position) =>
                {
                    let open = open_positions(&self.state.positions, account_id)
                        .map(|position| Some(position.instrument_id.clone()))
                        .collect::<Vec<_>>();
                    if open.is_empty() { vec![None] } else { open }
                }
                WorkflowScope::AnySymbol => vec![None],
            };
            for symbol in symbols {
                let reading =
                    self.check_reading(account_id, symbol.as_ref(), &entry.best_exits, day_pnl);
                let held = condition.holds(|check| reading.holds(check));
                readings.push(((index, symbol), held));
            }
        }
        readings
    }

    fn current_workflow(
        &self,
        workflow_id: &str,
        revision: u32,
    ) -> Result<&WorkflowRecord, WorkflowError> {
        let record = &self
            .workflows
            .get(workflow_id)
            .ok_or(WorkflowError::UnknownWorkflow)?
            .record;
        if record.definition.revision != revision {
            return Err(WorkflowError::StaleRevision {
                current: record.definition.revision,
            });
        }
        Ok(record)
    }

    /// Persists a state change of a known workflow; arming starts its trigger afresh.
    fn replace_workflow_record(&mut self, record: WorkflowRecord) -> Result<(), WorkflowError> {
        self.store.put_workflow(&record, None)?;
        let now = self.command_now;
        let entry = self
            .workflows
            .get_mut(&record.definition.workflow_id)
            .ok_or(WorkflowError::UnknownWorkflow)?;
        entry.at_level = [None; MAXIMUM_WORKFLOW_TRIGGERS];
        entry.position_marks = None;
        entry.condition_marks = None;
        entry.next_due_unix_nanos = next_due(&record, now);
        entry.next_check_unix_nanos = next_condition_check(&record, now);
        entry.record = record;
        self.bump_revision().map_err(WorkflowError::Owner)
    }

    fn workflow_view(&self, workflow_id: &str) -> Result<WorkflowView, WorkflowError> {
        let entry = self
            .workflows
            .get(workflow_id)
            .ok_or(WorkflowError::UnknownWorkflow)?;
        let mut record = entry.record.clone();
        record.roll_session(
            self.workflow_session_boundary(&record.definition.account_id),
            self.command_now,
        );
        let blockers = if record.state == WorkflowState::Armed {
            self.workflow_blockers(&record.definition)
        } else {
            Vec::new()
        };
        Ok(WorkflowView {
            layout: record.layout,
            state: record.state,
            session_firings: record.session_firings,
            total_firings: record.total_firings,
            last_attempt_unix_nanos: record.last_attempt_unix_nanos,
            next_due_unix_nanos: entry.next_due_unix_nanos,
            blockers,
            recent_firings: entry.firings.iter().rev().cloned().collect(),
            definition: record.definition,
        })
    }

    fn workflow_session_boundary(&self, account_id: &TradingAccountId) -> RiskSessionBoundary {
        self.state
            .risk_sessions
            .get(account_id)
            .map_or_else(RiskSessionBoundary::default, |session| session.boundary)
    }

    /// The blockers of every branch: what an armed workflow publishes and arming checks.
    /// A check of the day's P&L reads a broker account's open P&L from its venue.
    fn workflow_blockers(&self, definition: &WorkflowDefinition) -> Vec<WorkflowBlocker> {
        let enters = definition
            .actions()
            .any(|action| matches!(action, WorkflowAction::Enter { .. }));
        let venue = definition.actions().any(WorkflowAction::needs_venue)
            || definition
                .checks()
                .any(|check| matches!(check, WorkflowCheck::DayProfit { .. }));
        let prices = definition.checks().any(WorkflowCheck::reads_quote);
        self.blockers_for(definition, enters, venue, prices)
    }

    /// The blockers of the one branch a firing takes.
    fn path_blockers(
        &self,
        definition: &WorkflowDefinition,
        path: &[WorkflowPathStep],
    ) -> Vec<WorkflowBlocker> {
        let enters = path.iter().any(|step| {
            matches!(
                step,
                WorkflowPathStep::Action(_, WorkflowAction::Enter { .. }, _)
            )
        });
        let venue = path.iter().any(
            |step| matches!(step, WorkflowPathStep::Action(_, action, _) if action.needs_venue()),
        );
        self.blockers_for(definition, enters, venue, false)
    }

    /// `venue`: whether something sends to (or reads from) the account's venue, which an
    /// unreachable broker account cannot.
    fn blockers_for(
        &self,
        definition: &WorkflowDefinition,
        enters: bool,
        venue: bool,
        reads_price: bool,
    ) -> Vec<WorkflowBlocker> {
        let account_id = &definition.account_id;
        let mut blockers = Vec::new();
        if !self.state.accounts.contains_key(account_id) {
            blockers.push(WorkflowBlocker::AccountMissing);
        }
        let instrument_id = definition.scope.instrument();
        if instrument_id
            .is_some_and(|instrument_id| !self.state.instruments.contains_key(instrument_id))
        {
            blockers.push(WorkflowBlocker::InstrumentTermsMissing);
        }
        if enters && self.state.risk_locks.contains_key(account_id) {
            blockers.push(WorkflowBlocker::AccountLocked);
        }
        let reachable = match self.route(account_id) {
            Ok(VenueRoute::Simulated) => true,
            Ok(VenueRoute::Broker) => self.connected_broker_accounts.contains(account_id),
            Err(_) => !self.state.accounts.contains_key(account_id),
        };
        if venue && !reachable {
            blockers.push(WorkflowBlocker::BrokerUnreachable);
        }
        let needs_quote = enters
            || reads_price
            || definition
                .triggers
                .iter()
                .any(|trigger| matches!(trigger, WorkflowTrigger::PriceCrosses { .. }));
        // On any symbol, each position's quote is read when its check is.
        if needs_quote
            && instrument_id
                .is_some_and(|instrument_id| self.quote_book.observation(instrument_id).is_none())
        {
            blockers.push(WorkflowBlocker::QuoteUnavailable);
        }
        blockers
    }

    /// The branch a firing on `targets` now takes, every check read from the owner's state
    /// before any action runs. Fails with what stops a check on the path from being read.
    fn resolve_workflow_path(
        &self,
        definition: &WorkflowDefinition,
        targets: WorkflowTargets,
        kept_best: &BTreeMap<InstrumentId, BestExit>,
    ) -> Result<Vec<WorkflowPathStep>, WorkflowBlocker> {
        definition.resolve_path(targets, |condition, targets| {
            self.decide_condition(definition, condition, targets, kept_best)
        })
    }

    /// One symbol's If reads that symbol. On any symbol, an If with a position check is
    /// read for each open position the run acts on and holds when any passes, narrowing the
    /// targets to those that passed; with no such position (or no position check) it is
    /// read once as flat, leaving the targets as they are.
    fn decide_condition(
        &self,
        definition: &WorkflowDefinition,
        condition: &WorkflowCondition,
        targets: &mut WorkflowTargets,
        kept_best: &BTreeMap<InstrumentId, BestExit>,
    ) -> Result<bool, WorkflowBlocker> {
        let account_id = &definition.account_id;
        let day_pnl = condition
            .checks()
            .any(|check| matches!(check, WorkflowCheck::DayProfit { .. }))
            .then(|| {
                self.day_realized(account_id)
                    .and_then(|realized| self.day_pnl(account_id, realized.realized))
            });
        let read = |instrument_id: Option<&InstrumentId>| {
            let reading = self.check_reading(account_id, instrument_id, kept_best, day_pnl);
            condition.holds(|check| reading.holds(check))
        };
        if let WorkflowScope::Instrument(instrument_id) = &definition.scope {
            return read(Some(instrument_id));
        }
        let candidates = if condition.checks().any(WorkflowCheck::reads_position) {
            open_positions(&self.state.positions, account_id)
                .map(|position| &position.instrument_id)
                .filter(|instrument_id| targets.includes(instrument_id))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if candidates.is_empty() {
            return read(None);
        }
        let mut passed = BTreeSet::new();
        for instrument_id in candidates {
            if read(Some(instrument_id))? {
                passed.insert(instrument_id.clone());
            }
        }
        if passed.is_empty() {
            return Ok(false);
        }
        *targets = WorkflowTargets::Only(passed);
        Ok(true)
    }

    /// What a check reads about the account's position and quote in one symbol, or about
    /// the account alone, flat, without one, with the day's P&L when a check needs it.
    fn check_reading<'a>(
        &'a self,
        account_id: &TradingAccountId,
        instrument_id: Option<&InstrumentId>,
        kept_best: &BTreeMap<InstrumentId, BestExit>,
        day_pnl: Option<Result<FixedPoint, WorkflowBlocker>>,
    ) -> CheckReading<'a> {
        let Some(instrument_id) = instrument_id else {
            return CheckReading {
                now: self.command_now,
                position: None,
                opened_unix_nanos: None,
                quote: None,
                best_exit: None,
                instrument: None,
                day_pnl,
            };
        };
        let key = (account_id.clone(), instrument_id.clone());
        let position = self
            .state
            .positions
            .get(&key)
            .filter(|position| position.net_quantity.units() != 0);
        let mark = position_mark(&self.state.trade_cycles, &key);
        let quote = self
            .quote_book
            .observation(instrument_id)
            .map(|quote| (quote.bid, quote.ask));
        CheckReading {
            now: self.command_now,
            position,
            opened_unix_nanos: mark.opened_unix_nanos,
            quote,
            best_exit: position.zip(quote).and_then(|(position, (bid, ask))| {
                let kept = kept_best.get(instrument_id).copied();
                best_exit(kept, mark, position, bid, ask).map(|best| best.price)
            }),
            instrument: self.state.instruments.get(instrument_id),
            day_pnl,
        }
    }

    /// The start of the account's current risk session: its configured boundary, or UTC
    /// midnight for an account without a risk session.
    fn day_session_start(&self, account_id: &TradingAccountId) -> i64 {
        self.workflow_session_boundary(account_id)
            .session_start_containing(self.command_now)
    }

    /// The realized P&L, before fees, of the account's fills executed since its current
    /// risk session started. Summed from the owner's own retained fills, so an account
    /// without a risk profile needs no stored baseline.
    fn day_realized(&self, account_id: &TradingAccountId) -> Result<DayRealized, WorkflowBlocker> {
        let account = self
            .state
            .accounts
            .get(account_id)
            .ok_or(WorkflowBlocker::AccountMissing)?;
        let session_start_unix_nanos = self.day_session_start(account_id);
        let realized = self
            .fills_realized_since(account_id, session_start_unix_nanos, account.currency_scale)
            .map_err(|_| WorkflowBlocker::PositionUnknown)?;
        Ok(DayRealized {
            revision: self.state.revision,
            session_start_unix_nanos,
            realized,
        })
    }

    /// The day's realized P&L plus the open P&L of every position of the account. A broker
    /// account's open P&L is the venue's, unknown while it is unreachable.
    fn day_pnl(
        &self,
        account_id: &TradingAccountId,
        realized: FixedPoint,
    ) -> Result<FixedPoint, WorkflowBlocker> {
        let scale = realized.scale();
        let open = match self.route(account_id) {
            Ok(VenueRoute::Simulated) => {
                let zero =
                    FixedPoint::try_new(0, scale).map_err(|_| WorkflowBlocker::PositionUnknown)?;
                self.state
                    .positions
                    .values()
                    .filter(|position| &position.account_id == account_id)
                    .try_fold(zero, |total, position| {
                        position
                            .unrealized_pnl
                            .exact_rescale(scale)
                            .and_then(|open| total.checked_add(open))
                    })
                    .map_err(|_| WorkflowBlocker::PositionUnknown)?
            }
            Ok(VenueRoute::Broker) => self
                .broker_unrealized(account_id, scale)
                .map_err(|_| WorkflowBlocker::PositionUnknown)?
                .ok_or(WorkflowBlocker::BrokerUnreachable)?,
            Err(_) => return Err(WorkflowBlocker::AccountMissing),
        };
        realized
            .checked_add(open)
            .map_err(|_| WorkflowBlocker::PositionUnknown)
    }

    fn fire_workflow(&mut self, workflow_id: &str, targets: WorkflowTargets) {
        if let Err(error) = self.try_fire_workflow(workflow_id, targets) {
            aeris_observability::diagnostic!("workflow firing failed: {error}");
        }
    }

    /// Decides the branch, then records the firing durably before any action runs, so a
    /// stop in between leaves it `Interrupted` and a restart never repeats it; each action's
    /// client order id derives from the firing and its node for the same reason. A branch
    /// that reaches no action is not a firing: nothing is recorded or counted.
    fn try_fire_workflow(
        &mut self,
        workflow_id: &str,
        targets: WorkflowTargets,
    ) -> Result<(), WorkflowError> {
        let now = self.command_now;
        let mut record = self
            .workflows
            .get(workflow_id)
            .ok_or(WorkflowError::UnknownWorkflow)?
            .record
            .clone();
        let definition = record.definition.clone();
        record.roll_session(self.workflow_session_boundary(&definition.account_id), now);
        if record.cooling_down(now) || record.session_cap_reached() {
            return Ok(());
        }
        let kept_best = self
            .workflows
            .get(workflow_id)
            .map(|entry| entry.best_exits.clone())
            .unwrap_or_default();
        let path = self.resolve_workflow_path(&definition, targets, &kept_best);
        let acts = |path: &[WorkflowPathStep]| {
            path.iter()
                .any(|step| matches!(step, WorkflowPathStep::Action(..)))
        };
        if path.as_deref().is_ok_and(|path| !acts(path)) {
            return Ok(());
        }
        let sequence = record
            .last_sequence
            .checked_add(1)
            .ok_or_else(|| WorkflowError::Owner("workflow firing sequence overflowed".into()))?;
        record.last_sequence = sequence;
        record.last_attempt_unix_nanos = Some(now);
        let (path, blockers) = match path {
            Ok(path) => {
                let blockers = self.path_blockers(&definition, &path);
                (path, blockers)
            }
            Err(blocker) => (Vec::new(), vec![blocker]),
        };
        let mut firing = WorkflowFiring {
            sequence,
            revision: definition.revision,
            fired_unix_nanos: now,
            outcome: WorkflowFiringOutcome::Interrupted,
        };
        if !blockers.is_empty() {
            firing.outcome = WorkflowFiringOutcome::Blocked(blockers);
            return self.commit_workflow_firing(record, firing);
        }
        record.session_firings = record.session_firings.saturating_add(1);
        record.total_firings = record.total_firings.saturating_add(1);
        if definition.limits.repeat == WorkflowRepeat::Once {
            record.state = WorkflowState::Completed;
        }
        self.commit_workflow_firing(record.clone(), firing.clone())?;
        let mut results = Vec::with_capacity(path.len());
        for step in path {
            let (node, outcome) = match step {
                WorkflowPathStep::Branch(node, held) => (node, WorkflowStepOutcome::Branch(held)),
                WorkflowPathStep::Action(node, action, targets) => (
                    node,
                    WorkflowStepOutcome::Action(self.run_workflow_action(
                        &definition,
                        (sequence, node),
                        action,
                        &targets,
                    )),
                ),
            };
            results.push(WorkflowStepResult { node, outcome });
        }
        firing.outcome = WorkflowFiringOutcome::Executed(results);
        self.commit_workflow_firing(record, firing)
    }

    /// Writes the record with one firing, replacing an earlier write of the same firing.
    fn commit_workflow_firing(
        &mut self,
        record: WorkflowRecord,
        firing: WorkflowFiring,
    ) -> Result<(), WorkflowError> {
        self.store.put_workflow(&record, Some(&firing))?;
        let entry = self
            .workflows
            .get_mut(&record.definition.workflow_id)
            .ok_or(WorkflowError::UnknownWorkflow)?;
        if record.state != WorkflowState::Armed {
            entry.next_due_unix_nanos = None;
            entry.next_check_unix_nanos = None;
        }
        entry.record = record;
        if entry
            .firings
            .back()
            .is_some_and(|last| last.sequence == firing.sequence)
        {
            entry.firings.pop_back();
        }
        entry.firings.push_back(firing);
        while entry.firings.len() > WORKFLOW_FIRING_HISTORY {
            entry.firings.pop_front();
        }
        self.bump_revision().map_err(WorkflowError::Owner)
    }

    /// Runs one action of firing `sequence` at `node` on the symbols the run acts on.
    fn run_workflow_action(
        &mut self,
        definition: &WorkflowDefinition,
        (sequence, node): (u64, WorkflowNodeId),
        action: WorkflowAction,
        targets: &WorkflowTargets,
    ) -> WorkflowActionResult {
        let account_id = &definition.account_id;
        match action {
            WorkflowAction::Enter {
                side,
                quantity,
                protection,
            } => match workflow_client_order_id(
                &definition.workflow_id,
                definition.revision,
                sequence,
                node,
            ) {
                Ok(client_order_id) => self.enter_workflow_position(
                    definition,
                    client_order_id,
                    side,
                    quantity,
                    protection,
                ),
                Err(error) => WorkflowActionResult::Refused {
                    detail: error.to_string(),
                },
            },
            WorkflowAction::Flatten => self.flatten_workflow_targets(account_id, targets),
            WorkflowAction::CancelOrders => self.cancel_workflow_target_orders(account_id, targets),
            WorkflowAction::LockAccount => match self.lock_account(
                account_id.clone(),
                RiskLockKind::Manual,
                format!("Locked by workflow {}", definition.name),
                self.command_now,
            ) {
                Ok(()) => WorkflowActionResult::Locked,
                Err(detail) => WorkflowActionResult::Refused { detail },
            },
            // The committed firing is the notice; the desktop announces it.
            WorkflowAction::Notify => WorkflowActionResult::Notified,
        }
    }

    /// Closes the account's position in each target, or in every symbol.
    fn flatten_workflow_targets(
        &mut self,
        account_id: &TradingAccountId,
        targets: &WorkflowTargets,
    ) -> WorkflowActionResult {
        let scopes = match targets {
            WorkflowTargets::Every => vec![None],
            WorkflowTargets::Only(instruments) => instruments.iter().cloned().map(Some).collect(),
        };
        let mut refused = None;
        for instrument in scopes {
            let detail = match self.flatten_scope(&FlattenScope {
                account: Some(account_id.clone()),
                instrument,
            }) {
                Ok(outcome) => match outcome.incomplete {
                    Some(detail) => Some(detail),
                    None if !outcome.unpriced.is_empty() => {
                        Some(aeris_trading::RejectReason::QuoteUnavailable.to_string())
                    }
                    None => None,
                },
                Err(detail) => Some(detail),
            };
            if refused.is_none() {
                refused = detail;
            }
        }
        refused.map_or(WorkflowActionResult::Flattened, |detail| {
            WorkflowActionResult::Refused { detail }
        })
    }

    /// Cancels the account's working orders in each target, or in every symbol.
    fn cancel_workflow_target_orders(
        &mut self,
        account_id: &TradingAccountId,
        targets: &WorkflowTargets,
    ) -> WorkflowActionResult {
        let scopes = match targets {
            WorkflowTargets::Every => vec![None],
            WorkflowTargets::Only(instruments) => instruments.iter().map(Some).collect(),
        };
        let mut orders = 0;
        for instrument_id in scopes {
            match self.cancel_open_orders(Some(account_id), instrument_id) {
                Ok((cancelled, None)) => orders += cancelled.len(),
                Ok((_, Some(detail))) | Err(detail) => {
                    return WorkflowActionResult::Refused { detail };
                }
            }
        }
        WorkflowActionResult::Cancelled { orders }
    }

    /// A market entry stamped from the owner's verified quote, bracketed when the workflow
    /// names a stop and a target.
    fn enter_workflow_position(
        &mut self,
        definition: &WorkflowDefinition,
        client_order_id: ClientOrderId,
        side: OrderSide,
        quantity: FixedPoint,
        protection: Option<WorkflowProtection>,
    ) -> WorkflowActionResult {
        // Validation keeps entries out of workflows on any symbol.
        let Some(instrument_id) = definition.scope.instrument() else {
            return WorkflowActionResult::Refused {
                detail: WorkflowError::Invalid(WorkflowInvalid::Scope).to_string(),
            };
        };
        let quote = match self.market_observation(instrument_id) {
            Ok(quote) => quote,
            Err(detail) => return WorkflowActionResult::Refused { detail },
        };
        let entry = PlaceOrder {
            client_order_id: client_order_id.clone(),
            account_id: definition.account_id.clone(),
            instrument_id: instrument_id.clone(),
            side,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::Day,
            quantity,
            limit_price: None,
            stop_price: None,
            submitted_unix_nanos: self.command_now,
            provenance: quote.provenance,
        };
        let placed = match protection {
            None => self.place_order(&entry).map(|_| ()),
            Some(protection) => self
                .place_bracket_with_template(
                    &entry,
                    &BracketStrategyTemplate {
                        template_id: format!("workflow/{}", definition.workflow_id),
                        revision: definition.revision,
                        name: definition.workflow_id.clone(),
                        stop_offset_ticks: protection.stop_ticks,
                        targets: vec![BracketTarget {
                            offset_ticks: protection.target_ticks,
                            quantity_percent: 100,
                        }],
                        trailing_stop: None,
                        break_even: None,
                        enabled: true,
                    },
                )
                .map(|_| ()),
        };
        match placed {
            Ok(()) => WorkflowActionResult::Placed { client_order_id },
            Err(detail) => WorkflowActionResult::Refused { detail },
        }
    }
}

fn next_due(record: &WorkflowRecord, now: i64) -> Option<i64> {
    (record.state == WorkflowState::Armed)
        .then(|| record.definition.next_timed_trigger_after(now))
        .flatten()
}

/// The next whole minute after `now` while an armed workflow has a condition trigger whose
/// answer can change with the time alone.
fn next_condition_check(record: &WorkflowRecord, now: i64) -> Option<i64> {
    let reads_clock = record
        .definition
        .trigger_conditions()
        .flat_map(WorkflowCondition::checks)
        .any(WorkflowCheck::reads_clock);
    (record.state == WorkflowState::Armed && reads_clock).then(|| {
        now.div_euclid(NANOS_PER_MINUTE)
            .saturating_add(1)
            .saturating_mul(NANOS_PER_MINUTE)
    })
}

/// `Above` holds while the bid is at or above `level`, `Below` while the ask is at or
/// below it; `None` when the quote is at another scale.
fn price_holds(
    direction: PriceDirection,
    level: FixedPoint,
    bid: FixedPoint,
    ask: FixedPoint,
) -> Option<bool> {
    let touch = match direction {
        PriceDirection::Above => bid,
        PriceDirection::Below => ask,
    };
    (touch.scale() == level.scale()).then(|| match direction {
        PriceDirection::Above => touch.units() >= level.units(),
        PriceDirection::Below => touch.units() <= level.units(),
    })
}

/// What one run's checks read, taken once before any action of the run.
struct CheckReading<'a> {
    now: i64,
    /// The account's open position in the workflow instrument; `None` while flat.
    position: Option<&'a Position>,
    opened_unix_nanos: Option<i64>,
    /// The current verified bid and ask.
    quote: Option<(FixedPoint, FixedPoint)>,
    best_exit: Option<FixedPoint>,
    instrument: Option<&'a TradingInstrument>,
    /// The account's P&L today; `None` when no check of the condition reads it.
    day_pnl: Option<Result<FixedPoint, WorkflowBlocker>>,
}

impl CheckReading<'_> {
    fn holds(&self, check: WorkflowCheck) -> Result<bool, WorkflowBlocker> {
        let position_units = self
            .position
            .map_or(0, |position| position.net_quantity.units());
        match check {
            WorkflowCheck::Position(requirement) => Ok(requirement.holds(position_units)),
            WorkflowCheck::Between {
                zone,
                start_minute,
                end_minute,
            } => Ok(time_window_holds(zone, start_minute, end_minute, self.now)),
            WorkflowCheck::Weekday { zone, days } => Ok(days.contains(zone.weekday(self.now))),
            WorkflowCheck::Price { direction, level } => {
                let (bid, ask) = self.quote.ok_or(WorkflowBlocker::QuoteUnavailable)?;
                price_holds(direction, level, bid, ask).ok_or(WorkflowBlocker::QuoteUnavailable)
            }
            WorkflowCheck::PriceRange { low, high } => {
                let (bid, ask) = self.quote.ok_or(WorkflowBlocker::QuoteUnavailable)?;
                let above = price_holds(PriceDirection::Above, low, bid, ask);
                let below = price_holds(PriceDirection::Below, high, bid, ask);
                above
                    .zip(below)
                    .map(|(above, below)| above && below)
                    .ok_or(WorkflowBlocker::QuoteUnavailable)
            }
            WorkflowCheck::TimeInTrade { compare, minutes } => {
                if self.position.is_none() {
                    return Ok(false);
                }
                let opened = self
                    .opened_unix_nanos
                    .ok_or(WorkflowBlocker::PositionUnknown)?;
                Ok(compare.holds(
                    self.now.saturating_sub(opened),
                    i64::from(minutes).saturating_mul(NANOS_PER_MINUTE),
                ))
            }
            WorkflowCheck::OpenProfitTicks { compare, ticks } => {
                let Some(position) = self.position else {
                    return Ok(false);
                };
                let exit = self.exit_price(position)?;
                let entry = position
                    .average_entry_price
                    .ok_or(WorkflowBlocker::PositionUnknown)?;
                let (moved, scale) =
                    signed_move(entry, exit, position.net_quantity.units().signum())
                        .ok_or(WorkflowBlocker::QuoteUnavailable)?;
                let threshold = self.tick_distance(i64::from(ticks), exit, scale)?;
                Ok(compare.holds(moved, threshold))
            }
            WorkflowCheck::OpenProfit { compare, amount } => {
                let Some(position) = self.position else {
                    return Ok(false);
                };
                money_holds(compare, position.unrealized_pnl, amount)
            }
            WorkflowCheck::DayProfit { compare, amount } => {
                let pnl = self.day_pnl.ok_or(WorkflowBlocker::PositionUnknown)??;
                money_holds(compare, pnl, amount)
            }
            WorkflowCheck::Pullback { ticks } => {
                let Some(position) = self.position else {
                    return Ok(false);
                };
                let exit = self.exit_price(position)?;
                let best = self.best_exit.unwrap_or(exit);
                let (pulled, scale) =
                    signed_move(exit, best, position.net_quantity.units().signum())
                        .ok_or(WorkflowBlocker::QuoteUnavailable)?;
                Ok(pulled >= self.tick_distance(i64::from(ticks), exit, scale)?)
            }
        }
    }

    /// The price the open position would exit at: the bid when long, the ask when short.
    fn exit_price(&self, position: &Position) -> Result<FixedPoint, WorkflowBlocker> {
        let (bid, ask) = self.quote.ok_or(WorkflowBlocker::QuoteUnavailable)?;
        Ok(if position.net_quantity.units() > 0 {
            bid
        } else {
            ask
        })
    }

    /// `ticks` of the instrument's tick at `at`, in units of `scale` decimals.
    fn tick_distance(
        &self,
        ticks: i64,
        at: FixedPoint,
        scale: u8,
    ) -> Result<i128, WorkflowBlocker> {
        let instrument = self
            .instrument
            .ok_or(WorkflowBlocker::InstrumentTermsMissing)?;
        let tick = instrument_tick_units(instrument, at)
            .map_err(|_| WorkflowBlocker::InstrumentTermsMissing)?;
        lift(
            i128::from(ticks).saturating_mul(i128::from(tick)),
            at.scale(),
            scale,
        )
        .ok_or(WorkflowBlocker::QuoteUnavailable)
    }
}

/// Compares a money value with an amount at their common scale.
fn money_holds(
    compare: WorkflowCompare,
    value: FixedPoint,
    amount: FixedPoint,
) -> Result<bool, WorkflowBlocker> {
    let scale = value.scale().max(amount.scale());
    lift(i128::from(value.units()), value.scale(), scale)
        .zip(lift(i128::from(amount.units()), amount.scale(), scale))
        .map(|(value, amount)| compare.holds(value, amount))
        .ok_or(WorkflowBlocker::PositionUnknown)
}

/// The account's position mark in one instrument; before its first fill, cycle 0 and no
/// open time.
fn position_mark(
    cycles: &BTreeMap<(TradingAccountId, InstrumentId), TradeCycle>,
    key: &(TradingAccountId, InstrumentId),
) -> PositionMark {
    cycles
        .get(key)
        .map_or(PositionMark::UNTRADED, |cycle| PositionMark {
            cycle_id: cycle.cycle_id,
            opened_unix_nanos: cycle.opened_unix_nanos,
        })
}

/// The position mark of each workflow symbol: the one instrument, or every symbol the
/// account has traded.
fn workflow_position_marks(
    cycles: &BTreeMap<(TradingAccountId, InstrumentId), TradeCycle>,
    definition: &WorkflowDefinition,
) -> BTreeMap<InstrumentId, PositionMark> {
    let account_id = &definition.account_id;
    match definition.scope.instrument() {
        Some(instrument_id) => BTreeMap::from([(
            instrument_id.clone(),
            position_mark(cycles, &(account_id.clone(), instrument_id.clone())),
        )]),
        None => cycles
            .iter()
            .filter(|((account, _), _)| account == account_id)
            .map(|((_, instrument_id), cycle)| {
                (
                    instrument_id.clone(),
                    PositionMark {
                        cycle_id: cycle.cycle_id,
                        opened_unix_nanos: cycle.opened_unix_nanos,
                    },
                )
            })
            .collect(),
    }
}

/// The account's open positions.
fn open_positions<'a>(
    positions: &'a BTreeMap<(TradingAccountId, InstrumentId), Position>,
    account_id: &'a TradingAccountId,
) -> impl Iterator<Item = &'a Position> {
    positions
        .iter()
        .filter(move |((account, _), position)| {
            account == account_id && position.net_quantity.units() != 0
        })
        .map(|(_, position)| position)
}

/// The most favorable of the kept best exit of this same position, its average entry and
/// the current exit price.
fn best_exit(
    kept: Option<BestExit>,
    mark: PositionMark,
    position: &Position,
    bid: FixedPoint,
    ask: FixedPoint,
) -> Option<BestExit> {
    let long = position.net_quantity.units() > 0;
    let exit = if long { bid } else { ask };
    let favorable = if long {
        Ordering::Greater
    } else {
        Ordering::Less
    };
    kept.filter(|kept| kept.mark == mark)
        .map(|kept| kept.price)
        .into_iter()
        .chain(position.average_entry_price)
        .chain([exit])
        .reduce(|best, price| {
            if compare_fixed(price, best) == Some(favorable) {
                price
            } else {
                best
            }
        })
        .map(|price| BestExit { mark, price })
}

/// `to - from` toward the position's side (`sign` is its quantity's sign), in units of
/// their common scale, with that scale.
fn signed_move(from: FixedPoint, to: FixedPoint, sign: i64) -> Option<(i128, u8)> {
    let scale = from.scale().max(to.scale());
    let delta = lift(i128::from(to.units()), to.scale(), scale)?.checked_sub(lift(
        i128::from(from.units()),
        from.scale(),
        scale,
    )?)?;
    Some((delta.checked_mul(i128::from(sign))?, scale))
}

fn compare_fixed(left: FixedPoint, right: FixedPoint) -> Option<Ordering> {
    let scale = left.scale().max(right.scale());
    Some(
        lift(i128::from(left.units()), left.scale(), scale)?.cmp(&lift(
            i128::from(right.units()),
            right.scale(),
            scale,
        )?),
    )
}

/// `units` at `from` decimals expressed at `to` decimals; `None` when `to` has fewer
/// decimals than `from` or the value overflows.
fn lift(units: i128, from: u8, to: u8) -> Option<i128> {
    10_i128
        .checked_pow(u32::from(to.checked_sub(from)?))?
        .checked_mul(units)
}

fn is_expired(definition: &WorkflowDefinition, now: i64) -> bool {
    definition
        .limits
        .expires_unix_nanos
        .is_some_and(|expiry| expiry <= now)
}

/// Levels and quantities must be at the instrument's own `(price, quantity)` scales, and
/// money amounts at the account currency's. A workflow on any symbol has neither levels
/// nor quantities.
fn check_scales(
    definition: &WorkflowDefinition,
    instrument_scales: Option<(u8, u8)>,
    currency_scale: u8,
) -> Result<(), WorkflowError> {
    if definition.checks().any(|check| {
        matches!(
            check,
            WorkflowCheck::OpenProfit { amount, .. } | WorkflowCheck::DayProfit { amount, .. }
                if amount.scale() != currency_scale
        )
    }) {
        return Err(WorkflowError::Invalid(WorkflowInvalid::Amount));
    }
    let Some((price_scale, quantity_scale)) = instrument_scales else {
        return Ok(());
    };
    let trigger_off_scale = definition.triggers.iter().any(|trigger| {
        matches!(trigger, WorkflowTrigger::PriceCrosses { level, .. } if level.scale() != price_scale)
    });
    let condition_off_scale = definition.checks().any(|check| match check {
        WorkflowCheck::Price { level, .. } => level.scale() != price_scale,
        WorkflowCheck::PriceRange { low, high } => {
            low.scale() != price_scale || high.scale() != price_scale
        }
        WorkflowCheck::Position(_)
        | WorkflowCheck::Between { .. }
        | WorkflowCheck::Weekday { .. }
        | WorkflowCheck::TimeInTrade { .. }
        | WorkflowCheck::OpenProfitTicks { .. }
        | WorkflowCheck::OpenProfit { .. }
        | WorkflowCheck::Pullback { .. }
        | WorkflowCheck::DayProfit { .. } => false,
    });
    if trigger_off_scale || condition_off_scale {
        return Err(WorkflowError::Invalid(WorkflowInvalid::Level));
    }
    if definition.actions().any(|action| {
        matches!(action, WorkflowAction::Enter { quantity, .. } if quantity.scale() != quantity_scale)
    }) {
        return Err(WorkflowError::Invalid(WorkflowInvalid::Quantity));
    }
    Ok(())
}
