use super::{
    ClientOrderId, EconomicEventRiskAction, EconomicEventRiskOutcome, EconomicEventRiskTrigger,
    EmergencyOutcome, Fill, FillId, FillMemoryUpdate, FixedPoint, FlattenOutcome, FlattenScope,
    GroupStatus, InstrumentId, Liquidity, Order, OrderId, OrderSide, OrderStatus, OrderType,
    PlaceOrder, SimulatedMarketObservation, TimeInForce, TradingAccountId, TradingCore,
    UnpricedPosition, VenueRoute, flatten_acceptance_event, flatten_fill_event,
};
use crate::book::lifecycle::CancellationCause;
use crate::store::OrderStep;

impl TradingCore {
    /// The kill switch is one durable step per invocation: it locks every in-scope tradable
    /// account, cancels their non-protective working orders with the kill-switch cause and
    /// flattens every priced position at the owner's verified quotes, retiring the
    /// flattened positions' protection in the same transaction. Positions without a
    /// verified quote keep their protection and are reported in `unpriced`. Broker cancels
    /// and closes are venue requests that follow the committed step; what could not be
    /// requested is reported in `incomplete` while the locks stay in place.
    pub(super) fn kill_switch(
        &mut self,
        account_id: Option<&TradingAccountId>,
        reason: &str,
        locked_at_unix_nanos: i64,
    ) -> Result<EmergencyOutcome, String> {
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
        // A targeted command on a data-only live account is refused before any change; a
        // global one skips such accounts, which cannot trade, and protects the rest.
        if let Some(account_id) = account_id {
            self.route(account_id)?;
        }
        let tradable = accounts
            .into_iter()
            .filter(|account| self.route(account).is_ok())
            .collect::<Vec<_>>();
        let scope = FlattenScope {
            account: account_id.cloned(),
            instrument: None,
        };
        let reason = reason.to_string();
        let mut outcome = self.plan_atomic(|core| {
            let mut staged = EmergencyOutcome::default();
            for account in &tradable {
                core.lock_account(
                    account.clone(),
                    crate::RiskLockKind::KillSwitch,
                    reason.clone(),
                    locked_at_unix_nanos,
                )?;
                staged.accounts_locked += 1;
            }
            let planned = core.stage_simulated_flatten(&scope, CancellationCause::KillSwitch)?;
            staged.cancelled = planned.cancelled;
            staged.fills = planned.fills;
            staged.unpriced = planned.unpriced;
            Ok(staged)
        })?;
        for account in &tradable {
            if !matches!(self.route(account), Ok(VenueRoute::CtraderDemo)) {
                continue;
            }
            match self.flatten_broker_account(account, None) {
                Ok(closed) => {
                    outcome
                        .pending_close_requests
                        .extend(closed.pending_close_requests);
                    if let Some(failure) = closed.incomplete {
                        append_incomplete(
                            &mut outcome.incomplete,
                            format!("accounts are locked; {failure}"),
                        );
                    }
                }
                Err(error) => append_incomplete(
                    &mut outcome.incomplete,
                    format!("accounts are locked; cTrader positions were not closed: {error}"),
                ),
            }
        }
        Ok(outcome)
    }

    /// Locks the accounts and cancels their non-protective working orders in one durable
    /// step; protection of still-open positions keeps working. Broker orders are cancelled
    /// by request after the committed step. This is the economic-event lock action: no
    /// position is closed.
    fn lock_and_disarm(
        &mut self,
        account_id: &TradingAccountId,
        reason: &str,
        locked_at_unix_nanos: i64,
    ) -> Result<(), String> {
        self.route(account_id)?;
        let scope = FlattenScope {
            account: Some(account_id.clone()),
            instrument: None,
        };
        let reason = reason.to_string();
        self.plan_atomic(|core| {
            core.lock_account(
                account_id.clone(),
                crate::RiskLockKind::KillSwitch,
                reason.clone(),
                locked_at_unix_nanos,
            )?;
            core.stage_scope_cancellations(&scope, CancellationCause::KillSwitch)?;
            Ok(())
        })?;
        if matches!(self.route(account_id), Ok(VenueRoute::CtraderDemo)) {
            let (_, failure) = self.cancel_open_orders(Some(account_id), None)?;
            if let Some(failure) = failure {
                return Err(format!("accounts are locked; {failure}"));
            }
        }
        Ok(())
    }

    /// Runs `plan` on a speculative projection of the state and commits every staged change
    /// in one transaction. A failed plan or a failed commit leaves memory and disk at the
    /// last committed state.
    pub(super) fn plan_atomic<T>(
        &mut self,
        plan: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.pending_equity_samples.clear();
        let projection = self.state.clone();
        let original = std::mem::replace(&mut self.state, projection);
        self.quote_planning = true;
        let result = plan(self);
        self.quote_planning = false;
        let planned = std::mem::replace(&mut self.state, original);
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.pending_equity_samples.clear();
                return Err(error);
            }
        };
        if let Err(error) =
            self.store
                .commit_quote_step(&self.state, &planned, &self.pending_equity_samples)
        {
            self.pending_equity_samples.clear();
            return Err(error);
        }
        self.pending_equity_samples.clear();
        self.state = planned;
        self.enforce_memory_retention();
        Ok(value)
    }

    /// Flattens every in-scope simulated position at the owner's verified quotes, then
    /// requests the in-scope broker closes. The simulated step commits once; a broker venue
    /// that cannot be reached is reported without blocking anything else.
    pub(super) fn flatten_scope(&mut self, scope: &FlattenScope) -> Result<FlattenOutcome, String> {
        if self.state.accounts.is_empty() {
            return Err("flatten requires at least one registered account".to_string());
        }
        // A targeted command on an unknown or data-only live account is refused before any
        // change; a global one skips such accounts, which cannot trade, and handles the rest.
        if let Some(account_id) = &scope.account {
            self.route(account_id)?;
        }
        let mut outcome = FlattenOutcome::default();
        let simulated = self
            .state
            .accounts
            .keys()
            .filter(|account| {
                scope.account.as_ref().is_none_or(|scope| scope == *account)
                    && matches!(self.route(account), Ok(VenueRoute::Simulated))
            })
            .cloned()
            .collect::<Vec<_>>();
        if !simulated.is_empty() {
            let planned = self.plan_atomic(|core| {
                core.stage_simulated_flatten(scope, CancellationCause::Flatten)
            })?;
            outcome.fills = planned.fills;
            outcome.unpriced = planned.unpriced;
        }
        let broker = self
            .state
            .accounts
            .keys()
            .filter(|account| {
                scope.account.as_ref().is_none_or(|scope| scope == *account)
                    && matches!(self.route(account), Ok(VenueRoute::CtraderDemo))
            })
            .cloned()
            .collect::<Vec<_>>();
        for account_id in broker {
            match self.flatten_broker_account(&account_id, scope.instrument.as_ref()) {
                Ok(closed) => {
                    outcome
                        .pending_close_requests
                        .extend(closed.pending_close_requests);
                    if let Some(failure) = closed.incomplete {
                        append_incomplete(&mut outcome.incomplete, failure);
                    }
                }
                Err(error) => append_incomplete(
                    &mut outcome.incomplete,
                    format!("cTrader positions were not closed: {error}"),
                ),
            }
        }
        Ok(outcome)
    }

    /// Reverses one simulated position at the owner's verified quote. The close and the
    /// admitted opening order commit in one step, so a risk rejection or a failed commit
    /// leaves the original position untouched.
    pub(super) fn reverse_position(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
    ) -> Result<FlattenOutcome, String> {
        if !self.state.accounts.contains_key(account_id) {
            return Err("reverse account is not registered".to_string());
        }
        match self.route(account_id)? {
            VenueRoute::Simulated => {}
            VenueRoute::CtraderDemo => {
                let observation = self.market_observation(instrument_id)?;
                return self.reverse_broker_position(
                    account_id,
                    instrument_id,
                    &observation.provenance,
                );
            }
        }
        let observation = self.verified_observation(instrument_id)?;
        self.plan_atomic(|core| core.stage_reverse(account_id, instrument_id, &observation))
    }

    /// The owner's own verified quote for an administrative close: a current Live quote
    /// inside the freshness window on an open session.
    fn verified_observation(
        &self,
        instrument_id: &InstrumentId,
    ) -> Result<SimulatedMarketObservation, String> {
        let instrument = self
            .state
            .instruments
            .get(instrument_id)
            .ok_or_else(|| "flatten instrument is not registered".to_string())?
            .clone();
        self.check_practice_market(&instrument)
            .map_err(|reason| reason.to_string())?;
        self.quote_book
            .observation(instrument_id)
            .ok_or_else(|| "a current live market quote is unavailable".to_string())
    }

    /// Cancels the scope's non-protective working orders with `cause` and stages one
    /// closing fill per priced in-scope position on the speculative state. Positions
    /// without a verified quote are collected in the unpriced report, never silently
    /// skipped, and keep their protection.
    fn stage_simulated_flatten(
        &mut self,
        scope: &FlattenScope,
        cause: CancellationCause,
    ) -> Result<StagedFlatten, String> {
        let cancelled = self.stage_scope_cancellations(scope, cause)?;
        let keys = self
            .state
            .positions
            .iter()
            .filter(|((account_id, instrument_id), position)| {
                scope
                    .account
                    .as_ref()
                    .is_none_or(|scope| scope == account_id)
                    && scope
                        .instrument
                        .as_ref()
                        .is_none_or(|scope| scope == instrument_id)
                    && position.net_quantity.units() != 0
                    && matches!(self.route(account_id), Ok(VenueRoute::Simulated))
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut fills = Vec::new();
        let mut unpriced = Vec::new();
        let mut priced: Vec<(InstrumentId, SimulatedMarketObservation)> = Vec::new();
        for key in keys {
            let Ok(observation) = self.verified_observation(&key.1) else {
                let position = self
                    .state
                    .positions
                    .get(&key)
                    .ok_or("flatten position disappeared")?;
                unpriced.push(UnpricedPosition {
                    account_id: key.0.clone(),
                    instrument_id: key.1.clone(),
                    net_quantity: position.net_quantity,
                });
                continue;
            };
            if let Some(fill) = self.stage_flatten_fill(&key, &observation)? {
                fills.push(fill);
                if !priced.iter().any(|(id, _)| id == &key.1) {
                    priced.push((key.1.clone(), observation));
                }
            }
        }
        // Protection of a flattened position retires with the fill, in the same step.
        for (instrument_id, observation) in priced {
            let instrument = self
                .state
                .instruments
                .get(&instrument_id)
                .ok_or_else(|| "flatten instrument is not registered".to_string())?
                .clone();
            let instrument_fills = fills
                .iter()
                .filter(|fill| fill.instrument_id == instrument_id)
                .cloned()
                .collect::<Vec<_>>();
            self.bump_revision()?;
            self.reconcile_order_groups(&instrument_fills, &observation, &instrument)?;
            self.align_position_protection(&instrument_fills, &observation)?;
        }
        Ok(StagedFlatten {
            cancelled,
            fills,
            unpriced,
        })
    }

    /// Cancels every working order in the scope that is not an active group's protection
    /// leg; a pending bracket's entry takes its held legs and group with it. Protection of
    /// a position that stays open (unpriced) remains working. Returns the cancelled orders.
    fn stage_scope_cancellations(
        &mut self,
        scope: &FlattenScope,
        cause: CancellationCause,
    ) -> Result<Vec<Order>, String> {
        let protected = self
            .state
            .order_groups
            .values()
            .filter(|group| group.status == GroupStatus::Active)
            .flat_map(|group| group.legs.iter().map(|leg| leg.order_id.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        let candidates = self
            .state
            .orders
            .open_orders()
            .filter(|order| {
                scope
                    .account
                    .as_ref()
                    .is_none_or(|scope| scope == &order.account_id)
                    && scope
                        .instrument
                        .as_ref()
                        .is_none_or(|scope| scope == &order.instrument_id)
                    && matches!(self.route(&order.account_id), Ok(VenueRoute::Simulated))
                    && !protected.contains(&order.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut step = OrderStep::new(self.state.next_sequence);
        let mut cancelled = Vec::new();
        for order in candidates {
            if step
                .updated
                .iter()
                .any(|(updated, _)| updated.id == order.id)
            {
                continue;
            }
            cancelled.push(self.stage_cancellation(&mut step, order, true, cause)?);
        }
        self.commit_order_step(step, Vec::new())?;
        Ok(cancelled)
    }

    /// Stages one closing fill for a position at the verified quote. The wrapping
    /// `plan_atomic` commits the order, fill, ledger and equity sample in one transaction.
    fn stage_flatten_fill(
        &mut self,
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
            .get(&key.1)
            .ok_or_else(|| "flatten instrument is not registered".to_string())?
            .clone();
        let account = self
            .state
            .accounts
            .get(&key.0)
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
        let order = flatten_order(
            &key.0,
            observation,
            sequence,
            side,
            quantity,
            self.command_now,
        )?;
        let accepted = flatten_acceptance_event(&order, sequence)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(accepted);
        self.state.orders.insert(order.id.clone(), order.clone());
        let fill = Fill {
            id: FillId::try_new(format!("sim-flatten-fill-{}", sequence + 1))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            account_id: key.0.clone(),
            instrument_id: key.1.clone(),
            side,
            price,
            quantity,
            execution_unix_nanos: provenance.observed_unix_nanos,
            provenance: provenance.clone(),
        };
        fill.validate().map_err(|error| error.to_string())?;
        let event = flatten_fill_event(&order, &fill, sequence + 1, provenance)?;
        let (position, cost_basis, policy_update) = self.apply_fill(
            &fill,
            Some(&current),
            &account,
            &instrument,
            Liquidity::Taker,
        )?;
        let filled = crate::book::lifecycle::fill_simulated(order, self.command_now)
            .map_err(|error| error.to_string())?;
        self.apply_fill_to_memory(
            filled,
            event,
            &fill,
            FillMemoryUpdate {
                position,
                cost_basis,
                position_key: key.clone(),
                policy_update,
                next_sequence: sequence + 2,
            },
        );
        Ok(Some(fill))
    }

    /// The reverse is one admitted operation: the position closes, the opposite market
    /// order is admitted against the post-close state, and both fills commit together.
    fn stage_reverse(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        observation: &SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        let key = (account_id.clone(), instrument_id.clone());
        let position = self
            .state
            .positions
            .get(&key)
            .filter(|position| position.net_quantity.units() != 0)
            .cloned()
            .ok_or_else(|| "reverse requires an open position".to_string())?;
        let mut fills = Vec::new();
        if let Some(fill) = self.stage_flatten_fill(&key, observation)? {
            fills.push(fill);
        }
        let instrument = self
            .state
            .instruments
            .get(instrument_id)
            .ok_or_else(|| "reverse instrument is not registered".to_string())?
            .clone();
        self.bump_revision()?;
        self.reconcile_order_groups(&fills, observation, &instrument)?;
        self.align_position_protection(&fills, observation)?;
        // The opening leg is admitted after the close, so the risk limits evaluate the
        // position the reverse would leave behind; a rejection abandons the whole step.
        let side = if position.net_quantity.units() > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let quantity = FixedPoint::try_new(
            i64::try_from(position.net_quantity.units().unsigned_abs())
                .map_err(|_| "reverse quantity overflowed".to_string())?,
            position.net_quantity.scale(),
        )
        .map_err(|error| error.to_string())?;
        let command = PlaceOrder {
            client_order_id: ClientOrderId::try_new(format!(
                "sim-reverse-{}",
                self.state.next_sequence
            ))
            .map_err(|error| error.to_string())?,
            account_id: account_id.clone(),
            instrument_id: instrument_id.clone(),
            side,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            quantity,
            limit_price: None,
            stop_price: None,
            submitted_unix_nanos: observation.provenance.observed_unix_nanos,
            provenance: observation.provenance.clone(),
        };
        let mut step = OrderStep::new(self.state.next_sequence);
        let order = self.stage_single_order(&mut step, command, true)?;
        self.commit_order_step(step, Vec::new())?;
        let touch = if side == OrderSide::Sell {
            observation.bid
        } else {
            observation.ask
        };
        let opening = self.execute_market_fills(
            vec![(order.id.clone(), touch)],
            observation,
            &instrument,
            true,
        )?;
        self.bump_revision()?;
        self.reconcile_order_groups(&opening, observation, &instrument)?;
        self.align_position_protection(&opening, observation)?;
        fills.extend(opening);
        Ok(FlattenOutcome {
            fills,
            ..FlattenOutcome::default()
        })
    }

    pub(super) fn apply_economic_event_risk(
        &mut self,
        event: &EconomicEventRiskTrigger,
    ) -> Result<EconomicEventRiskOutcome, String> {
        const EVENT_GRACE_NANOS: i64 = 15 * 60 * 1_000_000_000;
        event.validate()?;
        let profiles = self
            .state
            .risk_profiles
            .values()
            .filter_map(|profile| {
                profile
                    .enabled
                    .then_some(profile.economic_event_rule)
                    .flatten()
                    .map(|rule| (profile.clone(), rule))
            })
            .collect::<Vec<_>>();
        for (profile, rule) in &profiles {
            if rule.action == EconomicEventRiskAction::Flatten
                && self.route(&profile.account_id)? != VenueRoute::Simulated
            {
                return Err("economic-event flatten is unavailable for broker accounts".into());
            }
        }
        let mut outcome = EconomicEventRiskOutcome::default();
        for (profile, rule) in profiles {
            if event.importance < rule.minimum_importance {
                continue;
            }
            let lead_nanos = i64::from(rule.lead_seconds)
                .checked_mul(1_000_000_000)
                .ok_or_else(|| "economic event lead time overflowed".to_string())?;
            let window_start = event
                .scheduled_unix_nanos
                .checked_sub(lead_nanos)
                .ok_or_else(|| "economic event lead window overflowed".to_string())?;
            let window_end = event
                .scheduled_unix_nanos
                .checked_add(EVENT_GRACE_NANOS)
                .ok_or_else(|| "economic event grace window overflowed".to_string())?;
            if !(window_start..=window_end).contains(&event.observed_unix_nanos)
                || self.store.economic_event_action_exists(
                    &profile.account_id,
                    &event.event_id,
                    profile.version,
                )?
            {
                continue;
            }
            match rule.action {
                EconomicEventRiskAction::Lock => {
                    // Lock and cancel non-protective orders without closing positions:
                    // protection of the still-open positions keeps working.
                    let reason = format!("Economic event lock: {} ({})", event.title, event.source);
                    self.lock_and_disarm(&profile.account_id, &reason, event.observed_unix_nanos)?;
                    outcome.locked_accounts += 1;
                }
                EconomicEventRiskAction::Flatten => {
                    // The owner's verified quotes price every open instrument; positions
                    // without one are reported rather than blocking the rule.
                    let flattened = self.flatten_scope(&FlattenScope {
                        account: Some(profile.account_id.clone()),
                        instrument: None,
                    })?;
                    outcome.fills.extend(flattened.fills);
                    outcome.unpriced.extend(flattened.unpriced);
                    outcome.flattened_accounts += 1;
                }
            }
            self.store.put_economic_event_action(
                &profile.account_id,
                &event.event_id,
                profile.version,
                rule.action,
                event.observed_unix_nanos,
            )?;
        }
        Ok(outcome)
    }
}

/// The staged result of a simulated flatten: the non-protective orders cancelled with
/// the caller's cause, the closing fills and the positions kept as unpriced.
struct StagedFlatten {
    cancelled: Vec<Order>,
    fills: Vec<Fill>,
    unpriced: Vec<UnpricedPosition>,
}

fn append_incomplete(slot: &mut Option<String>, message: String) {
    match slot {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&message);
        }
        None => *slot = Some(message),
    }
}

fn flatten_order(
    account_id: &TradingAccountId,
    observation: &SimulatedMarketObservation,
    sequence: u64,
    side: OrderSide,
    quantity: FixedPoint,
    command_now: i64,
) -> Result<Order, String> {
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
        filled_quantity: FixedPoint::try_new(0, quantity.scale())
            .map_err(|error| error.to_string())?,
        limit_price: None,
        stop_price: None,
        status: OrderStatus::Working,
        submitted_unix_nanos: observation.provenance.observed_unix_nanos,
        revision: 0,
        accepted_unix_nanos: 0,
        eligible_after_unix_nanos: 0,
        triggered_unix_nanos: None,
        expires_at_unix_nanos: None,
        updated_unix_nanos: 0,
        terminal_unix_nanos: None,
        provenance: observation.provenance.clone(),
    };
    let accepted =
        crate::book::lifecycle::accept(order, command_now).map_err(|error| error.to_string())?;
    accepted.validate().map_err(|error| error.to_string())?;
    Ok(accepted)
}
