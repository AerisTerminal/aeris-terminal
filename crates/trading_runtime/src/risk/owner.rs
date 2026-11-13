use super::{
    BROKER_PNL_UNAVAILABLE, BTreeMap, EconomicEventRiskAction, FAST_STOP_REENTRY_NANOS, FixedPoint,
    InstrumentId, MaximumDrawdownRule, OrderSide, PlaceOrder, RiskEvaluation, RiskLock,
    RiskLockKind, RiskProfile, RiskRuleState, RiskSession, RiskSessionBoundary, SessionPlan,
    TradingAccountId, TradingCore, TrailingDrawdownMode, VenueRoute, checked_difference,
    consistency_projection, subtract_fixed,
};
use aeris_trading::{RiskWarning, TradingValidationError};

/// Where the maximum drawdown stands for one account.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MaximumDrawdownStanding {
    pub(super) equity: FixedPoint,
    pub(super) peak: FixedPoint,
    pub(super) floor: FixedPoint,
}

impl TradingCore {
    pub(super) fn register_risk_profile(&mut self, profile: RiskProfile) -> Result<(), String> {
        if profile
            .economic_event_rule
            .is_some_and(|rule| rule.action == EconomicEventRiskAction::Flatten)
            && self.route(&profile.account_id)? != VenueRoute::Simulated
        {
            return Err("economic-event flatten is unavailable for broker accounts".into());
        }
        profile.validate()?;
        let Some(account) = self.state.accounts.get(&profile.account_id) else {
            return Err("risk profile account is not registered".to_string());
        };
        if profile.maximum_drawdown.is_some() {
            if self.route(&profile.account_id)? != VenueRoute::Simulated {
                return Err("a maximum drawdown is available only for practice accounts".into());
            }
            if profile.daily_loss_limit.scale() != account.currency_scale {
                return Err(
                    "a maximum drawdown needs the risk profile in the account currency scale"
                        .into(),
                );
            }
        }
        if self
            .state
            .risk_profiles
            .get(&profile.account_id)
            .is_some_and(|current| {
                current.profile_id == profile.profile_id && profile.version <= current.version
            })
        {
            return Err("risk profile revision must advance".to_string());
        }
        if self.route(&profile.account_id)? != VenueRoute::Simulated
            && profile.session_start_realized_pnl.units() != 0
        {
            return Err(
                "a broker account's session P&L starts from its deals; the baseline must be zero"
                    .into(),
            );
        }
        let zero = FixedPoint::try_new(0, profile.daily_loss_limit.scale())
            .map_err(|error| error.to_string())?;
        // An unreachable broker account starts its peak at zero; the live check raises it.
        let current = self.session_pnl(&profile)?.unwrap_or(zero);
        let equity_peak = profile
            .maximum_drawdown
            .map(|_| {
                self.registered_equity_peak(&profile)
                    .ok_or("account equity is unavailable for the maximum drawdown")
            })
            .transpose()?;
        let rule_state = RiskRuleState {
            account_id: profile.account_id.clone(),
            profile_id: profile.profile_id.clone(),
            profile_version: profile.version,
            peak_session_pnl: if current.units() > 0 { current } else { zero },
            total_winning_pnl: zero,
            largest_winning_trade_pnl: zero,
            equity_peak,
        };
        self.store
            .put_risk_profile_and_state(&profile, &rule_state)?;
        // Matches the store's INSERT OR IGNORE: the first profile seeds the session, a later
        // revision keeps the running session and its baseline.
        self.state
            .risk_sessions
            .entry(profile.account_id.clone())
            .or_insert_with(|| RiskSession::from_profile(&profile));
        self.state
            .risk_rule_states
            .insert(profile.account_id.clone(), rule_state);
        for cycle in self
            .state
            .trade_cycles
            .values_mut()
            .filter(|cycle| cycle.account_id == profile.account_id)
        {
            cycle.risk_realized_pnl = FixedPoint::try_new(0, cycle.risk_realized_pnl.scale())
                .map_err(|error| error.to_string())?;
            cycle.peak_quantity = FixedPoint::try_new(0, cycle.peak_quantity.scale())
                .map_err(|error| error.to_string())?;
        }
        self.state
            .risk_profiles
            .insert(profile.account_id.clone(), profile);
        self.bump_revision()
    }

    /// The equity high-water mark a registration with a maximum drawdown keeps, or `None`
    /// while the account equity is unknown: a revision that keeps the rule keeps its peak, so
    /// editing other rules never resets it; a profile that adds the rule starts the peak at
    /// the account's current equity.
    fn registered_equity_peak(&self, profile: &RiskProfile) -> Option<FixedPoint> {
        self.state
            .risk_profiles
            .get(&profile.account_id)
            .filter(|current| current.maximum_drawdown.is_some())
            .and_then(|_| self.state.risk_rule_states.get(&profile.account_id))
            .and_then(|state| state.equity_peak)
            .or_else(|| self.account_equity(&profile.account_id))
    }

    /// The account's equity against its maximum-drawdown floor, or `None` while equity is
    /// unknown. A missing peak reads as the current equity.
    pub(super) fn maximum_drawdown_standing(
        &self,
        account_id: &TradingAccountId,
        rule: MaximumDrawdownRule,
    ) -> Result<Option<MaximumDrawdownStanding>, TradingValidationError> {
        let Some(equity) = self.account_equity(account_id) else {
            return Ok(None);
        };
        let peak = self
            .state
            .risk_rule_states
            .get(account_id)
            .and_then(|state| state.equity_peak)
            .unwrap_or(equity);
        Ok(Some(MaximumDrawdownStanding {
            equity,
            peak,
            floor: rule.floor(peak)?,
        }))
    }

    pub(super) fn register_session_plan(&mut self, plan: SessionPlan) -> Result<(), String> {
        plan.validate()?;
        if !self.state.accounts.contains_key(&plan.account_id) {
            return Err("session plan account is not registered".to_string());
        }
        if self
            .state
            .session_plans
            .get(&plan.account_id)
            .is_some_and(|current| {
                current.plan_id == plan.plan_id && plan.revision <= current.revision
            })
        {
            return Err("session plan revision must advance".to_string());
        }
        self.store.put_session_plan(&plan)?;
        self.state
            .session_plans
            .insert(plan.account_id.clone(), plan);
        self.bump_revision()
    }

    pub(super) fn lock_account(
        &mut self,
        account_id: TradingAccountId,
        kind: RiskLockKind,
        reason: String,
        locked_at_unix_nanos: i64,
    ) -> Result<(), String> {
        if !self.state.accounts.contains_key(&account_id) {
            return Err("risk lock account is not registered".to_string());
        }
        let profile = self.state.risk_profiles.get(&account_id);
        let lock = RiskLock {
            account_id: account_id.clone(),
            kind,
            reason,
            locked_at_unix_nanos,
            profile_id: profile.map(|profile| profile.profile_id.clone()),
            profile_version: profile.map(|profile| profile.version),
        };
        if !self.quote_planning {
            self.store.put_risk_lock(&lock)?;
        }
        self.state.risk_locks.insert(account_id, lock);
        self.bump_revision()
    }

    pub(super) fn unlock_account(&mut self, account_id: &TradingAccountId) -> Result<(), String> {
        self.store.delete_risk_lock(account_id)?;
        self.state.risk_locks.remove(account_id);
        self.bump_revision()
    }

    pub(super) fn evaluate_order_risk(
        &mut self,
        command: &PlaceOrder,
        attached_stop: bool,
    ) -> Result<RiskEvaluation, String> {
        if self.state.risk_locks.contains_key(&command.account_id) {
            return Err(self.admission_detail(command, aeris_trading::RejectReason::AccountLocked));
        }
        self.check_session_plan(command)?;
        let discipline_warnings = self.check_discipline(command)?;
        let Some(profile) = self.state.risk_profiles.get(&command.account_id).cloned() else {
            return Ok(RiskEvaluation {
                warnings: discipline_warnings,
                projected_contracts: command.quantity,
                current_realized_pnl: FixedPoint::try_new(0, 2)
                    .map_err(|error| error.to_string())?,
            });
        };
        if !profile.enabled {
            return Ok(RiskEvaluation {
                warnings: discipline_warnings,
                projected_contracts: command.quantity,
                current_realized_pnl: FixedPoint::try_new(0, profile.daily_loss_limit.scale())
                    .map_err(|error| error.to_string())?,
            });
        }
        self.check_restriction(command, &profile)?;
        let current_session_pnl = self.session_pnl(&profile)?.ok_or(BROKER_PNL_UNAVAILABLE)?;
        self.check_loss_limits(command, &profile, current_session_pnl)?;
        let projected_contracts = self.projected_contracts(command)?;
        let maximum_contracts = profile
            .max_contracts
            .exact_rescale(command.quantity.scale())
            .map_err(|error| error.to_string())?;
        if projected_contracts > maximum_contracts.units().unsigned_abs() {
            return Err("order exceeds the account maximum-contract rule".to_string());
        }
        let mut warnings =
            self.risk_warnings(command, &profile, current_session_pnl, attached_stop)?;
        warnings.extend(discipline_warnings);
        Ok(RiskEvaluation {
            warnings,
            projected_contracts: FixedPoint::try_new(
                i64::try_from(projected_contracts)
                    .map_err(|_| "projected contract quantity overflowed".to_string())?,
                command.quantity.scale(),
            )
            .map_err(|error| error.to_string())?,
            current_realized_pnl: current_session_pnl,
        })
    }

    pub(super) fn check_session_plan(&mut self, command: &PlaceOrder) -> Result<(), String> {
        if self.strictly_reduces_position(command)? {
            return Ok(());
        }
        let Some(plan) = self.state.session_plans.get(&command.account_id).cloned() else {
            return Ok(());
        };
        if !plan.contains_time(command.submitted_unix_nanos) {
            return Err("order is outside the session plan trading hours".to_string());
        }
        if !plan.is_ready() {
            return Err("session plan checklist or allowed setup is incomplete".to_string());
        }
        let pnl = self.plan_session_pnl(&plan)?;
        if pnl.units() < 0 && pnl.units().unsigned_abs() >= plan.maximum_loss.units().unsigned_abs()
        {
            self.lock_account(
                command.account_id.clone(),
                RiskLockKind::SessionPlan,
                "session plan maximum loss reached".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: session plan maximum loss reached".to_string());
        }
        Ok(())
    }

    pub(super) fn check_discipline(
        &self,
        command: &PlaceOrder,
    ) -> Result<Vec<RiskWarning>, String> {
        if self.strictly_reduces_position(command)? {
            return Ok(Vec::new());
        }
        let fast_reentry = self
            .state
            .discipline_states
            .get(&command.account_id)
            .and_then(|state| state.last_stop_fill_unix_nanos)
            .is_some_and(|last_stop| {
                let elapsed = command.submitted_unix_nanos.saturating_sub(last_stop);
                (0..=FAST_STOP_REENTRY_NANOS).contains(&elapsed)
            });
        Ok(if fast_reentry {
            vec![RiskWarning::FastStopReentry]
        } else {
            Vec::new()
        })
    }

    pub(super) fn strictly_reduces_position(&self, command: &PlaceOrder) -> Result<bool, String> {
        let Some(position) = self
            .state
            .positions
            .get(&(command.account_id.clone(), command.instrument_id.clone()))
        else {
            return Ok(false);
        };
        let net = position
            .net_quantity
            .exact_rescale(command.quantity.scale())
            .map_err(|error| error.to_string())?
            .units();
        Ok(net != 0
            && net.signum() != command.side.sign()
            && command.quantity.units().unsigned_abs() <= net.unsigned_abs())
    }

    pub(super) fn plan_session_pnl(&self, plan: &SessionPlan) -> Result<FixedPoint, String> {
        let current = self
            .state
            .positions
            .values()
            .filter(|position| position.account_id == plan.account_id)
            .try_fold(
                FixedPoint::try_new(0, plan.maximum_loss.scale())
                    .map_err(|error| error.to_string())?,
                |total, position| {
                    let realized = position
                        .realized_pnl
                        .exact_rescale(plan.maximum_loss.scale())
                        .map_err(|error| error.to_string())?;
                    let unrealized = position
                        .unrealized_pnl
                        .exact_rescale(plan.maximum_loss.scale())
                        .map_err(|error| error.to_string())?;
                    total
                        .checked_add(realized)
                        .and_then(|value| value.checked_add(unrealized))
                        .map_err(|error| error.to_string())
                },
            )?;
        subtract_fixed(current, plan.session_start_realized_pnl)
    }

    pub(super) fn check_restriction(
        &mut self,
        command: &PlaceOrder,
        profile: &RiskProfile,
    ) -> Result<(), String> {
        if profile
            .restricted_until_unix_nanos
            .is_some_and(|until| command.submitted_unix_nanos < until)
        {
            self.lock_account(
                command.account_id.clone(),
                RiskLockKind::NewsRestriction,
                "news/session restriction is active".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: news/session restriction is active".to_string());
        }
        Ok(())
    }

    pub(super) fn check_loss_limits(
        &mut self,
        command: &PlaceOrder,
        profile: &RiskProfile,
        current_session_pnl: FixedPoint,
    ) -> Result<(), String> {
        let loss = current_session_pnl.units().unsigned_abs();
        if current_session_pnl.units() < 0
            && loss >= profile.daily_loss_limit.units().unsigned_abs()
        {
            self.lock_account(
                command.account_id.clone(),
                RiskLockKind::DailyLossLimit,
                "daily loss limit reached".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: daily loss limit reached".to_string());
        }
        if let Some(drawdown) = profile.trailing_drawdown {
            let state = self
                .state
                .risk_rule_states
                .get(&profile.account_id)
                .ok_or_else(|| "risk rule state is unavailable".to_string())?;
            let floor = subtract_fixed(state.peak_session_pnl, drawdown)?;
            if current_session_pnl.units() <= floor.units() {
                self.lock_account(
                    command.account_id.clone(),
                    RiskLockKind::TrailingDrawdown,
                    format!(
                        "{} trailing drawdown reached",
                        profile.trailing_mode.as_str()
                    ),
                    command.submitted_unix_nanos,
                )?;
                return Err("account is risk-locked: trailing drawdown reached".to_string());
            }
        }
        if let Some(target) = profile.daily_profit_target
            && current_session_pnl.units() >= target.units()
        {
            self.lock_account(
                command.account_id.clone(),
                RiskLockKind::ProfitTarget,
                "daily profit target reached".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: daily profit target reached".to_string());
        }
        if let Some(rule) = profile.maximum_drawdown
            && let Some(standing) = self
                .maximum_drawdown_standing(&profile.account_id, rule)
                .map_err(|error| error.to_string())?
            && standing.equity.units() <= standing.floor.units()
        {
            self.lock_account(
                command.account_id.clone(),
                RiskLockKind::MaximumDrawdown,
                "maximum drawdown reached".to_string(),
                command.submitted_unix_nanos,
            )?;
            return Err("account is risk-locked: maximum drawdown reached".to_string());
        }
        Ok(())
    }

    pub(super) fn projected_contracts(&self, command: &PlaceOrder) -> Result<u64, String> {
        self.projected_contracts_for_account(
            &command.account_id,
            command.quantity.scale(),
            Some(command),
        )
    }

    pub(super) fn projected_contracts_for_account(
        &self,
        account_id: &TradingAccountId,
        quantity_scale: u8,
        command: Option<&PlaceOrder>,
    ) -> Result<u64, String> {
        let mut scenarios = BTreeMap::<InstrumentId, (i128, i128, i128)>::new();
        for position in self
            .state
            .positions
            .values()
            .filter(|position| &position.account_id == account_id)
        {
            let current = position
                .net_quantity
                .exact_rescale(quantity_scale)
                .map_err(|error| error.to_string())?;
            scenarios
                .entry(position.instrument_id.clone())
                .or_default()
                .0 = i128::from(current.units());
        }
        for order in self.state.orders.open_orders().filter(|order| {
            &order.account_id == account_id
                && order.status.is_open()
                && self.owned_leg_role(&order.client_order_id).is_none()
        }) {
            let quantity = order
                .quantity
                .exact_rescale(quantity_scale)
                .map_err(|error| error.to_string())?;
            let scenario = scenarios.entry(order.instrument_id.clone()).or_default();
            match order.side {
                OrderSide::Buy => scenario.1 += i128::from(quantity.units()),
                OrderSide::Sell => scenario.2 += i128::from(quantity.units()),
            }
        }
        if let Some(command) = command {
            let quantity = command
                .quantity
                .exact_rescale(quantity_scale)
                .map_err(|error| error.to_string())?;
            let scenario = scenarios.entry(command.instrument_id.clone()).or_default();
            match command.side {
                OrderSide::Buy => scenario.1 += i128::from(quantity.units()),
                OrderSide::Sell => scenario.2 += i128::from(quantity.units()),
            }
        }
        let total = scenarios
            .values()
            .try_fold(0_u128, |total, (current, buys, sells)| {
                let long = current
                    .checked_add(*buys)
                    .ok_or_else(|| "risk contract quantity overflowed".to_string())?;
                let short = current
                    .checked_sub(*sells)
                    .ok_or_else(|| "risk contract quantity overflowed".to_string())?;
                total
                    .checked_add(long.unsigned_abs().max(short.unsigned_abs()))
                    .ok_or_else(|| "risk contract quantity overflowed".to_string())
            })?;
        u64::try_from(total).map_err(|_| "risk contract quantity overflowed".to_string())
    }

    pub(super) fn risk_warnings(
        &self,
        command: &PlaceOrder,
        profile: &RiskProfile,
        current_realized: FixedPoint,
        attached_stop: bool,
    ) -> Result<Vec<RiskWarning>, String> {
        let mut warnings = Vec::new();
        if current_realized.units() < 0
            && current_realized.units().unsigned_abs() * 100
                >= profile.daily_loss_limit.units().unsigned_abs() * 80
        {
            warnings.push(RiskWarning::ApproachingDailyLossLimit);
        }
        // A bracket entry's stop leg is attached by its template, and a stop-market entry
        // carries its own trigger; both count as attached stops for the warning.
        if !attached_stop && command.stop_price.is_none() {
            warnings.push(RiskWarning::NoAttachedStop);
        }
        if let Some(limit) = profile.consistency_max_single_trade_percent {
            let state = self
                .state
                .risk_rule_states
                .get(&profile.account_id)
                .ok_or_else(|| "risk rule state is unavailable".to_string())?;
            let (current, _) = consistency_projection(state, limit)?;
            if current > limit {
                warnings.push(RiskWarning::ConsistencyLimitExceeded {
                    current_percent: current,
                    limit_percent: limit,
                });
            }
        }
        Ok(warnings)
    }

    /// The account's durable risk session, derived from the registering profile while a
    /// gated pre-group store still waits for its V27 session rows.
    fn risk_session_for(&self, profile: &RiskProfile) -> RiskSession {
        self.state
            .risk_sessions
            .get(&profile.account_id)
            .cloned()
            .unwrap_or_else(|| RiskSession::from_profile(profile))
    }

    /// Realized P&L since the session start. A broker account sums the realized P&L of its
    /// deals executed since then; its baseline is zero, enforced at registration.
    pub(super) fn realized_since_session(
        &self,
        profile: &RiskProfile,
    ) -> Result<FixedPoint, String> {
        self.realized_since(profile, &self.risk_session_for(profile))
    }

    fn realized_since(
        &self,
        profile: &RiskProfile,
        session: &RiskSession,
    ) -> Result<FixedPoint, String> {
        if self.route(&profile.account_id)? != VenueRoute::Simulated {
            return self.fills_realized_since(
                &profile.account_id,
                session.session_start_unix_nanos,
                profile.daily_loss_limit.scale(),
            );
        }
        let current = self
            .state
            .positions
            .values()
            .filter(|position| position.account_id == profile.account_id)
            .map(|position| position.realized_pnl)
            .try_fold(
                FixedPoint::try_new(0, profile.daily_loss_limit.scale())
                    .map_err(|error| error.to_string())?,
                |total, value| {
                    let value = value
                        .exact_rescale(profile.daily_loss_limit.scale())
                        .map_err(|error| error.to_string())?;
                    total.checked_add(value).map_err(|error| error.to_string())
                },
            )?;
        let baseline = session
            .session_start_realized_pnl
            .exact_rescale(profile.daily_loss_limit.scale())
            .map_err(|error| error.to_string())?;
        let negative_baseline = FixedPoint::try_new(
            baseline
                .units()
                .checked_neg()
                .ok_or_else(|| "risk session baseline overflowed".to_string())?,
            baseline.scale(),
        )
        .map_err(|error| error.to_string())?;
        current
            .checked_add(negative_baseline)
            .map_err(|error| error.to_string())
    }

    /// Session P&L: realized since the session start plus open P&L. `None` while a broker
    /// account is unreachable, because its open P&L is then unknown.
    pub(super) fn session_pnl(&self, profile: &RiskProfile) -> Result<Option<FixedPoint>, String> {
        self.session_pnl_for(profile, &self.risk_session_for(profile))
    }

    fn session_pnl_for(
        &self,
        profile: &RiskProfile,
        session: &RiskSession,
    ) -> Result<Option<FixedPoint>, String> {
        let scale = profile.daily_loss_limit.scale();
        let realized = self.realized_since(profile, session)?;
        if self.route(&profile.account_id)? != VenueRoute::Simulated {
            let Some(unrealized) = self.broker_unrealized(&profile.account_id, scale)? else {
                return Ok(None);
            };
            return realized
                .checked_add(unrealized)
                .map(Some)
                .map_err(|error| error.to_string());
        }
        self.state
            .positions
            .values()
            .filter(|position| position.account_id == profile.account_id)
            .map(|position| position.unrealized_pnl)
            .try_fold(realized, |total, unrealized| {
                let unrealized = unrealized
                    .exact_rescale(scale)
                    .map_err(|error| error.to_string())?;
                total
                    .checked_add(unrealized)
                    .map_err(|error| error.to_string())
            })
            .map(Some)
    }

    /// The realized P&L, before fees, of the account's fills executed at or after `since`:
    /// for a broker account the venue-reported deals.
    pub(super) fn fills_realized_since(
        &self,
        account_id: &TradingAccountId,
        since_unix_nanos: i64,
        scale: u8,
    ) -> Result<FixedPoint, String> {
        let zero = FixedPoint::try_new(0, scale).map_err(|error| error.to_string())?;
        self.state
            .fills
            .iter()
            .filter(|fill| {
                &fill.account_id == account_id && fill.execution_unix_nanos >= since_unix_nanos
            })
            .filter_map(|fill| self.state.fill_accounting.get(&fill.id))
            .filter_map(|accounting| accounting.realized_gross)
            .try_fold(zero, |total, realized| {
                realized
                    .exact_rescale(scale)
                    .and_then(|realized| total.checked_add(realized))
            })
            .map_err(|error| error.to_string())
    }

    /// The venue's open P&L of the account's positions, or `None` while the venue cannot
    /// reach the account.
    pub(super) fn broker_unrealized(
        &self,
        account_id: &TradingAccountId,
        scale: u8,
    ) -> Result<Option<FixedPoint>, String> {
        if !self.connected_broker_accounts.contains(account_id) {
            return Ok(None);
        }
        let zero = FixedPoint::try_new(0, scale).map_err(|error| error.to_string())?;
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
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub(super) fn evaluate_live_risk(&mut self, observed_unix_nanos: i64) -> Result<(), String> {
        let profiles = self
            .state
            .risk_profiles
            .values()
            .filter(|profile| profile.enabled)
            .cloned()
            .collect::<Vec<_>>();
        for profile in profiles {
            if self.state.risk_locks.contains_key(&profile.account_id) {
                continue;
            }
            // An unreachable broker account is not locked on a guess.
            let Some(current) = self.session_pnl(&profile)? else {
                continue;
            };
            let mut breach = session_pnl_breach(&profile, current);
            if breach.is_none()
                && (profile.trailing_drawdown.is_some() || profile.maximum_drawdown.is_some())
            {
                let mut state = self
                    .state
                    .risk_rule_states
                    .get(&profile.account_id)
                    .cloned()
                    .ok_or_else(|| "risk rule state is unavailable".to_string())?;
                let equity = self.account_equity(&profile.account_id);
                breach = trailing_rule_breach(&profile, current, equity, &mut state)
                    .map_err(|error| error.to_string())?;
                if self.state.risk_rule_states.get(&profile.account_id) != Some(&state) {
                    // A speculative quote plan persists the raised peaks with the rest of its
                    // committed facts instead.
                    if !self.quote_planning {
                        self.store.put_risk_rule_state(&state)?;
                    }
                    self.state
                        .risk_rule_states
                        .insert(profile.account_id.clone(), state);
                }
            }
            if let Some((kind, reason)) = breach {
                self.lock_account(profile.account_id, kind, reason, observed_unix_nanos)?;
            }
        }
        let plans = self
            .state
            .session_plans
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for plan in plans {
            if self.state.risk_locks.contains_key(&plan.account_id) {
                continue;
            }
            let pnl = self.plan_session_pnl(&plan)?;
            if pnl.units() < 0
                && pnl.units().unsigned_abs() >= plan.maximum_loss.units().unsigned_abs()
            {
                self.lock_account(
                    plan.account_id,
                    RiskLockKind::SessionPlan,
                    "session plan maximum loss reached".to_string(),
                    observed_unix_nanos,
                )?;
            }
        }
        Ok(())
    }

    /// Changes one account's session boundary. The new boundary applies from the next
    /// rollover; the current session keeps its start and baseline.
    pub(super) fn set_risk_session_boundary(
        &mut self,
        account_id: &TradingAccountId,
        boundary: RiskSessionBoundary,
    ) -> Result<(), String> {
        if !self.state.risk_sessions.contains_key(account_id) {
            return Err("risk session is not registered for the account".to_string());
        }
        self.store.put_risk_session_boundary(account_id, boundary)?;
        if let Some(session) = self.state.risk_sessions.get_mut(account_id) {
            session.boundary = boundary;
        }
        self.bump_revision()
    }

    /// Rolls every due risk session at its boundary: re-baselines the daily meters, resets
    /// the session trailing peak, raises the maximum-drawdown peak to the boundary equity,
    /// releases the daily-meter and profit-target locks, and records the boundary equity
    /// sample, each account in one durable transaction. Called from the owner timer, so a
    /// process closed across a boundary rolls at the first tick after restart.
    pub(super) fn roll_risk_sessions(&mut self, now: i64) -> Result<(), String> {
        let sessions = self
            .state
            .risk_sessions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for session in sessions {
            if session
                .boundary
                .next_after(session.session_start_unix_nanos)
                > now
            {
                continue;
            }
            let rolled = RiskSession {
                account_id: session.account_id.clone(),
                boundary: session.boundary,
                session_start_unix_nanos: session.boundary.session_start_containing(now),
                session_start_realized_pnl: self.session_rollover_baseline(&session)?,
            };
            let profile = self.state.risk_profiles.get(&session.account_id).cloned();
            let rule_state = match (
                &profile,
                self.state.risk_rule_states.get(&session.account_id),
            ) {
                (Some(profile), Some(state)) => {
                    // A broker account whose open P&L is unknown keeps its previous peak.
                    let mut reset = state.clone();
                    if let Some(current) = self.session_pnl_for(profile, &rolled)? {
                        if current.units() > 0 {
                            reset.peak_session_pnl = current;
                        } else {
                            reset.peak_session_pnl = FixedPoint::try_new(0, current.scale())
                                .map_err(|error| error.to_string())?;
                        }
                    }
                    // The maximum drawdown's peak never resets: the boundary equity only
                    // raises it, which is the only move an end-of-day peak makes.
                    if profile.enabled
                        && profile.maximum_drawdown.is_some()
                        && let Some(equity) = self.account_equity(&session.account_id)
                        && reset
                            .equity_peak
                            .is_none_or(|peak| equity.units() > peak.units())
                    {
                        reset.equity_peak = Some(equity);
                    }
                    Some(reset)
                }
                _ => None,
            };
            let released = self
                .state
                .risk_locks
                .get(&session.account_id)
                .filter(|lock| lock.kind.resets_at_session_boundary())
                .map(|lock| vec![lock.account_id.clone()])
                .unwrap_or_default();
            // The boundary equity sample belongs to profiled simulated accounts; it commits
            // inside the same transaction as the session row.
            let sample = if profile.is_some()
                && crate::is_simulated(&self.state.accounts, &session.account_id)
            {
                self.account_equity(&session.account_id)
                    .map(|equity| aeris_trading::EquitySample {
                        account_id: session.account_id.clone(),
                        at_unix_nanos: now,
                        equity,
                    })
            } else {
                None
            };
            self.store.roll_risk_session(
                &rolled,
                rule_state.as_ref(),
                &released,
                sample.as_ref(),
            )?;
            self.state
                .risk_sessions
                .insert(session.account_id.clone(), rolled);
            if let Some(state) = rule_state {
                self.state
                    .risk_rule_states
                    .insert(session.account_id.clone(), state);
            }
            for account_id in &released {
                self.state.risk_locks.remove(account_id);
            }
            if let Some(sample) = sample {
                self.push_equity_sample(sample);
            }
            self.bump_revision()?;
        }
        Ok(())
    }

    /// The realized-P&L baseline of the next session: the all-time realized total for a
    /// simulated account, zero for a broker account (its deals measure from the new start).
    fn session_rollover_baseline(&self, session: &RiskSession) -> Result<FixedPoint, String> {
        let scale = session.session_start_realized_pnl.scale();
        let zero = FixedPoint::try_new(0, scale).map_err(|error| error.to_string())?;
        if self.route(&session.account_id)? != VenueRoute::Simulated {
            return Ok(zero);
        }
        self.state
            .positions
            .values()
            .filter(|position| position.account_id == session.account_id)
            .map(|position| position.realized_pnl)
            .try_fold(zero, |total, value| {
                let value = value
                    .exact_rescale(scale)
                    .map_err(|error| error.to_string())?;
                total.checked_add(value).map_err(|error| error.to_string())
            })
    }
}

/// The daily rule the live session P&L breaches: the loss limit or the profit target.
fn session_pnl_breach(
    profile: &RiskProfile,
    current: FixedPoint,
) -> Option<(RiskLockKind, String)> {
    if current.units() < 0
        && current.units().unsigned_abs() >= profile.daily_loss_limit.units().unsigned_abs()
    {
        return Some((
            RiskLockKind::DailyLossLimit,
            "daily loss limit reached".to_string(),
        ));
    }
    profile
        .daily_profit_target
        .is_some_and(|target| current.units() >= target.units())
        .then(|| {
            (
                RiskLockKind::ProfitTarget,
                "daily profit target reached".to_string(),
            )
        })
}

/// The trailing rule the live session P&L or equity breaches, raising in `state` the
/// intraday peaks those rules follow on the way. Without a known equity the maximum
/// drawdown is not evaluated.
fn trailing_rule_breach(
    profile: &RiskProfile,
    current: FixedPoint,
    equity: Option<FixedPoint>,
    state: &mut RiskRuleState,
) -> Result<Option<(RiskLockKind, String)>, TradingValidationError> {
    if let Some(drawdown) = profile.trailing_drawdown {
        if profile.trailing_mode == TrailingDrawdownMode::Intraday
            && current.units() > state.peak_session_pnl.units()
        {
            state.peak_session_pnl = current;
        }
        let floor = checked_difference(state.peak_session_pnl, drawdown)?;
        if current.units() <= floor.units() {
            return Ok(Some((
                RiskLockKind::TrailingDrawdown,
                format!(
                    "{} trailing drawdown reached",
                    profile.trailing_mode.as_str()
                ),
            )));
        }
    }
    let (Some(rule), Some(equity)) = (profile.maximum_drawdown, equity) else {
        return Ok(None);
    };
    // An end-of-day peak moves only at the session rollover; an intraday peak follows
    // every new equity high.
    let peak = match state.equity_peak {
        Some(peak)
            if rule.mode == TrailingDrawdownMode::EndOfDay || equity.units() <= peak.units() =>
        {
            peak
        }
        _ => equity,
    };
    state.equity_peak = Some(peak);
    Ok((equity.units() <= rule.floor(peak)?.units()).then(|| {
        (
            RiskLockKind::MaximumDrawdown,
            "maximum drawdown reached".to_string(),
        )
    }))
}
