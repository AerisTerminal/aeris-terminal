//! The trading owner's side of workflows: registration, arming, evaluation against its own
//! verified quotes and clock, and the actions of each firing, which go through the same
//! admission, risk and venue paths as a manual command.

use super::{
    BracketStrategyTemplate, BracketTarget, FlattenScope, PlaceOrder, RiskLockKind, TradingCore,
    TradingService, VenueRoute, respond,
};
use crate::risk::session::RiskSessionBoundary;
use crate::service::{Command, WorkflowCommand};
use crate::workflow::{
    MAXIMUM_WORKFLOWS, PriceDirection, WORKFLOW_FIRING_HISTORY, WorkflowAction,
    WorkflowActionResult, WorkflowBlocker, WorkflowCondition, WorkflowDefinition, WorkflowError,
    WorkflowFiring, WorkflowFiringOutcome, WorkflowInvalid, WorkflowLayout, WorkflowNodeId,
    WorkflowPathStep, WorkflowProtection, WorkflowRecord, WorkflowRepeat, WorkflowState,
    WorkflowStepOutcome, WorkflowStepResult, WorkflowTrigger, WorkflowView, time_window_holds,
    workflow_client_order_id,
};
use aeris_instruments::InstrumentId;
use aeris_trading::{
    ClientOrderId, FixedPoint, OrderSide, OrderType, TimeInForce, TradingAccountId,
};
use std::collections::VecDeque;

/// A time trigger reached more than this late (the computer slept or the owner stalled) is
/// skipped instead of firing away from its moment.
const TIME_TRIGGER_GRACE_NANOS: i64 = 60_000_000_000;

/// One workflow as the owner holds it between steps.
pub(crate) struct WorkflowEntry {
    record: WorkflowRecord,
    /// The newest firings, oldest first.
    firings: VecDeque<WorkflowFiring>,
    /// Whether the last quote since arming was at or past the trigger level; `None` until
    /// the first quote, which never fires.
    at_level: Option<bool>,
    next_due_unix_nanos: Option<i64>,
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
            self.workflows.insert(
                record.definition.workflow_id.clone(),
                WorkflowEntry {
                    record,
                    firings: firings.into(),
                    at_level: None,
                    next_due_unix_nanos,
                },
            );
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
        if !self.state.accounts.contains_key(&definition.account_id) {
            return Err(WorkflowError::UnknownAccount);
        }
        let instrument = self
            .state
            .instruments
            .get(&definition.instrument_id)
            .ok_or(WorkflowError::UnknownInstrument)?;
        check_scales(
            &definition,
            instrument.price_scale,
            instrument.quantity_scale,
        )?;
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
        self.workflows.insert(
            workflow_id.clone(),
            WorkflowEntry {
                record,
                firings,
                at_level: None,
                next_due_unix_nanos: None,
            },
        );
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

    /// Every workflow's instrument, so the working set carries the terms that name and
    /// price it.
    pub(super) fn workflow_instruments(&self) -> impl Iterator<Item = &InstrumentId> {
        self.workflows
            .values()
            .map(|entry| &entry.record.definition.instrument_id)
    }

    /// Instruments whose quotes an armed workflow needs, for the owner's quote interest.
    pub(super) fn armed_workflow_instruments(&self) -> impl Iterator<Item = &InstrumentId> {
        self.workflows
            .values()
            .filter(|entry| entry.record.state == WorkflowState::Armed)
            .map(|entry| &entry.record.definition.instrument_id)
    }

    /// The next time-of-day trigger or expiry of an armed workflow.
    pub(crate) fn next_workflow_deadline_unix_nanos(&self) -> Option<i64> {
        self.workflows
            .values()
            .filter(|entry| entry.record.state == WorkflowState::Armed)
            .flat_map(|entry| {
                entry
                    .next_due_unix_nanos
                    .into_iter()
                    .chain(entry.record.definition.limits.expires_unix_nanos)
            })
            .min()
    }

    /// Fires every armed price workflow whose market moved across its level since the
    /// previous quote. Runs after a market batch, outside quote planning.
    pub(super) fn evaluate_price_workflows(&mut self) {
        let mut fired = Vec::new();
        for (workflow_id, entry) in &mut self.workflows {
            if entry.record.state != WorkflowState::Armed {
                continue;
            }
            let WorkflowTrigger::PriceCrosses { direction, level } =
                entry.record.definition.trigger
            else {
                continue;
            };
            let Some(quote) = self
                .quote_book
                .observation(&entry.record.definition.instrument_id)
            else {
                continue;
            };
            let touch = match direction {
                PriceDirection::Above => quote.bid,
                PriceDirection::Below => quote.ask,
            };
            if touch.scale() != level.scale() {
                continue;
            }
            let at_level = match direction {
                PriceDirection::Above => touch.units() >= level.units(),
                PriceDirection::Below => touch.units() <= level.units(),
            };
            if entry.at_level == Some(false) && at_level {
                fired.push(workflow_id.clone());
            }
            entry.at_level = Some(at_level);
        }
        for workflow_id in fired {
            self.fire_workflow(&workflow_id);
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
            let WorkflowTrigger::TimeOfDay(time) = entry.record.definition.trigger else {
                continue;
            };
            let Some(due) = entry.next_due_unix_nanos.filter(|due| *due <= now) else {
                continue;
            };
            entry.next_due_unix_nanos = Some(time.next_after(now));
            if now.saturating_sub(due) <= TIME_TRIGGER_GRACE_NANOS {
                fired.push(workflow_id.clone());
            }
        }
        for workflow_id in fired {
            self.fire_workflow(&workflow_id);
        }
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
        entry.at_level = None;
        entry.next_due_unix_nanos = next_due(&record, now);
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
    fn workflow_blockers(&self, definition: &WorkflowDefinition) -> Vec<WorkflowBlocker> {
        let enters = definition
            .actions()
            .any(|action| matches!(action, WorkflowAction::Enter { .. }));
        let prices = definition
            .conditions()
            .any(|condition| matches!(condition, WorkflowCondition::Price { .. }));
        self.blockers_for(definition, enters, prices)
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
                WorkflowPathStep::Action(_, WorkflowAction::Enter { .. })
            )
        });
        self.blockers_for(definition, enters, false)
    }

    fn blockers_for(
        &self,
        definition: &WorkflowDefinition,
        enters: bool,
        reads_price: bool,
    ) -> Vec<WorkflowBlocker> {
        let account_id = &definition.account_id;
        let mut blockers = Vec::new();
        if !self.state.accounts.contains_key(account_id) {
            blockers.push(WorkflowBlocker::AccountMissing);
        }
        if !self
            .state
            .instruments
            .contains_key(&definition.instrument_id)
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
        if !reachable {
            blockers.push(WorkflowBlocker::BrokerUnreachable);
        }
        let needs_quote = enters
            || reads_price
            || matches!(definition.trigger, WorkflowTrigger::PriceCrosses { .. });
        if needs_quote
            && self
                .quote_book
                .observation(&definition.instrument_id)
                .is_none()
        {
            blockers.push(WorkflowBlocker::QuoteUnavailable);
        }
        blockers
    }

    /// The branch a firing now takes, every condition read from the owner's state before
    /// any action runs. `None` when a price condition has no current quote at its scale.
    fn resolve_workflow_path(
        &self,
        definition: &WorkflowDefinition,
    ) -> Option<Vec<WorkflowPathStep>> {
        let position_units = self
            .state
            .positions
            .get(&(
                definition.account_id.clone(),
                definition.instrument_id.clone(),
            ))
            .map_or(0, |position| position.net_quantity.units());
        let quote = self.quote_book.observation(&definition.instrument_id);
        definition.resolve_path(|condition| match condition {
            WorkflowCondition::Position(requirement) => Some(requirement.holds(position_units)),
            WorkflowCondition::Between {
                zone,
                start_minute,
                end_minute,
            } => Some(time_window_holds(
                zone,
                start_minute,
                end_minute,
                self.command_now,
            )),
            WorkflowCondition::Price { direction, level } => {
                let quote = quote.as_ref()?;
                let touch = match direction {
                    PriceDirection::Above => quote.bid,
                    PriceDirection::Below => quote.ask,
                };
                (touch.scale() == level.scale()).then(|| match direction {
                    PriceDirection::Above => touch.units() >= level.units(),
                    PriceDirection::Below => touch.units() <= level.units(),
                })
            }
        })
    }

    fn fire_workflow(&mut self, workflow_id: &str) {
        if let Err(error) = self.try_fire_workflow(workflow_id) {
            aeris_observability::diagnostic!("workflow firing failed: {error}");
        }
    }

    /// Decides the branch, then records the firing durably before any action runs, so a
    /// stop in between leaves it `Interrupted` and a restart never repeats it; each action's
    /// client order id derives from the firing and its node for the same reason. A branch
    /// that reaches no action is not a firing: nothing is recorded or counted.
    fn try_fire_workflow(&mut self, workflow_id: &str) -> Result<(), WorkflowError> {
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
        let path = self.resolve_workflow_path(&definition);
        let acts = |path: &[WorkflowPathStep]| {
            path.iter()
                .any(|step| matches!(step, WorkflowPathStep::Action(..)))
        };
        if path.as_deref().is_some_and(|path| !acts(path)) {
            return Ok(());
        }
        let sequence = record
            .last_sequence
            .checked_add(1)
            .ok_or_else(|| WorkflowError::Owner("workflow firing sequence overflowed".into()))?;
        record.last_sequence = sequence;
        record.last_attempt_unix_nanos = Some(now);
        let (path, blockers) = match path {
            Some(path) => {
                let blockers = self.path_blockers(&definition, &path);
                (path, blockers)
            }
            None => (Vec::new(), vec![WorkflowBlocker::QuoteUnavailable]),
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
                WorkflowPathStep::Action(node, action) => (
                    node,
                    WorkflowStepOutcome::Action(self.run_workflow_action(
                        &definition,
                        sequence,
                        node,
                        action,
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

    fn run_workflow_action(
        &mut self,
        definition: &WorkflowDefinition,
        sequence: u64,
        node: WorkflowNodeId,
        action: WorkflowAction,
    ) -> WorkflowActionResult {
        let account_id = &definition.account_id;
        let instrument_id = &definition.instrument_id;
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
            WorkflowAction::Flatten => match self.flatten_scope(&FlattenScope {
                account: Some(account_id.clone()),
                instrument: Some(instrument_id.clone()),
            }) {
                Ok(outcome) => match outcome.incomplete {
                    Some(detail) => WorkflowActionResult::Refused { detail },
                    None if !outcome.unpriced.is_empty() => WorkflowActionResult::Refused {
                        detail: aeris_trading::RejectReason::QuoteUnavailable.to_string(),
                    },
                    None => WorkflowActionResult::Flattened,
                },
                Err(detail) => WorkflowActionResult::Refused { detail },
            },
            WorkflowAction::CancelOrders => {
                match self.cancel_open_orders(Some(account_id), Some(instrument_id)) {
                    Ok((cancelled, None)) => WorkflowActionResult::Cancelled {
                        orders: cancelled.len(),
                    },
                    Ok((_, Some(detail))) | Err(detail) => WorkflowActionResult::Refused { detail },
                }
            }
            WorkflowAction::LockAccount => match self.lock_account(
                account_id.clone(),
                RiskLockKind::Manual,
                format!("Locked by workflow {}", definition.name),
                self.command_now,
            ) {
                Ok(()) => WorkflowActionResult::Locked,
                Err(detail) => WorkflowActionResult::Refused { detail },
            },
        }
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
        let quote = match self.market_observation(&definition.instrument_id) {
            Ok(quote) => quote,
            Err(detail) => return WorkflowActionResult::Refused { detail },
        };
        let entry = PlaceOrder {
            client_order_id: client_order_id.clone(),
            account_id: definition.account_id.clone(),
            instrument_id: definition.instrument_id.clone(),
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
    match (record.state, record.definition.trigger) {
        (WorkflowState::Armed, WorkflowTrigger::TimeOfDay(time)) => Some(time.next_after(now)),
        _ => None,
    }
}

fn is_expired(definition: &WorkflowDefinition, now: i64) -> bool {
    definition
        .limits
        .expires_unix_nanos
        .is_some_and(|expiry| expiry <= now)
}

/// Levels and quantities must be at the instrument's own scales.
fn check_scales(
    definition: &WorkflowDefinition,
    price_scale: u8,
    quantity_scale: u8,
) -> Result<(), WorkflowError> {
    let trigger_off_scale = matches!(
        definition.trigger,
        WorkflowTrigger::PriceCrosses { level, .. } if level.scale() != price_scale
    );
    let condition_off_scale = definition.conditions().any(|condition| {
        matches!(condition, WorkflowCondition::Price { level, .. } if level.scale() != price_scale)
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
