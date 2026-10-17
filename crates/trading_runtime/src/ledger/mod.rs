mod position;
pub(crate) use position::{FillAccounting, TradeCycle};

use super::{
    AccountPnl, DisciplineState, Fill, FillMemoryUpdate, FillPolicyUpdate, FixedPoint,
    MAXIMUM_SNAPSHOT_ITEMS, ManagedBracket, Order, OrderEvent, Position, PositionPnl,
    ProtectiveOrderRole, RAPID_LOSS_COOLDOWN_NANOS, RAPID_LOSS_THRESHOLD, RAPID_LOSS_WINDOW_NANOS,
    RiskLock, RiskMeter, SessionAdherenceReview, TradeRoundTrip, TradingAccount, TradingCore,
    TradingSnapshot, consistency_projection, is_simulated, negative_loss, nonnegative_difference,
    pnl_ticks, remaining_limit, round_trip, subtract_fixed, tick_value,
};

impl TradingCore {
    pub(super) fn next_discipline_state(
        &self,
        fill: &Fill,
        completed_trade: Option<(FixedPoint, FixedPoint)>,
    ) -> Result<DisciplineState, String> {
        let mut state = self
            .state
            .discipline_states
            .get(&fill.account_id)
            .cloned()
            .unwrap_or_else(|| DisciplineState::new(fill.account_id.clone()));
        if self.is_stop_fill(fill) {
            state.last_stop_fill_unix_nanos = Some(fill.execution_unix_nanos);
        }
        let Some((pnl, peak_quantity)) = completed_trade else {
            return Ok(state);
        };
        if pnl.units() >= 0 {
            state.rapid_loss_count = 0;
            state.loss_window_started_unix_nanos = None;
            state.post_loss_quantity_cap = None;
            return Ok(state);
        }
        let inside_window = state.loss_window_started_unix_nanos.is_some_and(|started| {
            fill.execution_unix_nanos.saturating_sub(started) <= RAPID_LOSS_WINDOW_NANOS
        });
        if inside_window {
            state.rapid_loss_count = state.rapid_loss_count.saturating_add(1);
        } else {
            state.rapid_loss_count = 1;
            state.loss_window_started_unix_nanos = Some(fill.execution_unix_nanos);
        }
        state.last_loss_unix_nanos = Some(fill.execution_unix_nanos);
        state.post_loss_quantity_cap = Some(peak_quantity);
        if state.rapid_loss_count >= RAPID_LOSS_THRESHOLD {
            state.cooldown_until_unix_nanos = Some(
                fill.execution_unix_nanos
                    .checked_add(RAPID_LOSS_COOLDOWN_NANOS)
                    .ok_or_else(|| "tilt cooldown timestamp overflowed".to_string())?,
            );
        }
        Ok(state)
    }

    pub(super) fn is_stop_fill(&self, fill: &Fill) -> bool {
        self.state.orders.get(&fill.order_id).is_some_and(|order| {
            self.protection_role(&order.client_order_id) == Some(ProtectiveOrderRole::StopLoss)
        })
    }

    pub(super) fn apply_fill_to_memory(
        &mut self,
        order: Order,
        event: OrderEvent,
        fill: &Fill,
        update: FillMemoryUpdate,
    ) {
        self.state.next_sequence = update.next_sequence;
        self.state.orders.insert(order.id.clone(), order);
        self.state.order_events.push_back(event);
        self.state.fills.push_back(fill.clone());
        self.state
            .fill_accounting
            .insert(fill.id.clone(), update.policy_update.accounting);
        self.state
            .positions
            .insert(update.position_key, update.position);
        if let Some(rule_state) = update.policy_update.rule_state {
            self.state
                .risk_rule_states
                .insert(fill.account_id.clone(), rule_state);
        }
        self.state.trade_cycles.insert(
            (fill.account_id.clone(), fill.instrument_id.clone()),
            update.policy_update.trade_cycle,
        );
        self.state.discipline_states.insert(
            fill.account_id.clone(),
            update.policy_update.discipline_state,
        );
    }

    /// The newest simulated-account fills, within the snapshot bound.
    pub(super) fn simulated_fills_newest_first(&self) -> Vec<&Fill> {
        self.state
            .fills
            .iter()
            .rev()
            .filter(|fill| is_simulated(&self.state.accounts, &fill.account_id))
            .take(MAXIMUM_SNAPSHOT_ITEMS)
            .collect()
    }

    /// Simulated round trips, open and closed, merged with closed broker round trips; every exit
    /// fill, partial or final, is one closed round trip.
    pub(super) fn round_trips(&self) -> Result<Vec<TradeRoundTrip>, String> {
        let broker_fills = self
            .state
            .fills
            .iter()
            .rev()
            .filter(|fill| self.state.broker_fill_positions.contains_key(&fill.id))
            .take(MAXIMUM_SNAPSHOT_ITEMS)
            .collect::<Vec<_>>();
        let mut round_trips = round_trip::project_round_trips(
            self.simulated_fills_newest_first().into_iter(),
            &self.state.positions,
            &self.state.fill_accounting,
            &self.state.trade_cycles,
        )?;
        round_trips.extend(round_trip::project_closed_broker_round_trips(
            broker_fills.into_iter(),
            &self.state.broker_fill_positions,
            &self.state.fill_accounting,
        )?);
        round_trips.sort_by_key(|trip| std::cmp::Reverse(trip.last_fill_unix_nanos()));
        Ok(round_trips)
    }

    pub(crate) fn snapshot(&self) -> Result<TradingSnapshot, String> {
        if self.state.accounts.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.instruments.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.orders.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.positions.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.broker_positions.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.protective_orders.len() > MAXIMUM_SNAPSHOT_ITEMS
        {
            return Err("trading snapshot item limit exceeded".to_string());
        }
        Ok(TradingSnapshot {
            revision: self.state.revision,
            market_observation_error: self.market_observation_error.clone(),
            accounts: self.state.accounts.values().cloned().collect(),
            instruments: self.state.instruments.values().cloned().collect(),
            orders: self.state.orders.values().cloned().collect(),
            order_events: self
                .state
                .order_events
                .iter()
                .rev()
                .take(MAXIMUM_SNAPSHOT_ITEMS)
                .cloned()
                .collect(),
            fills: self
                .state
                .fills
                .iter()
                .rev()
                .take(MAXIMUM_SNAPSHOT_ITEMS)
                .cloned()
                .collect(),
            completed_trade_pnl: self
                .state
                .fills
                .iter()
                .rev()
                .take(MAXIMUM_SNAPSHOT_ITEMS)
                .filter_map(|fill| {
                    self.state
                        .fill_accounting
                        .get(&fill.id)
                        .and_then(|row| row.cycle_realized.map(|pnl| (fill.id.clone(), pnl)))
                })
                .collect(),
            fill_realized_pnl: self
                .state
                .fills
                .iter()
                .rev()
                .take(MAXIMUM_SNAPSHOT_ITEMS)
                .filter_map(|fill| {
                    self.state
                        .fill_accounting
                        .get(&fill.id)
                        .and_then(|row| row.realized_gross.map(|pnl| (fill.id.clone(), pnl)))
                })
                .collect(),
            round_trips: self.round_trips()?,
            positions: self.state.positions.values().cloned().collect(),
            broker_positions: self.state.broker_positions.values().cloned().collect(),
            broker_balances: self.state.broker_balances.clone(),
            connected_broker_accounts: self.connected_broker_accounts.clone(),
            position_pnl: self.position_pnl()?,
            account_pnl: self.account_pnl()?,
            risk_profiles: self.state.risk_profiles.values().cloned().collect(),
            risk_meters: self.risk_meters()?,
            risk_locks: self.projected_risk_locks(),
            risk_rule_states: self.state.risk_rule_states.values().cloned().collect(),
            discipline_states: self.state.discipline_states.values().cloned().collect(),
            session_plans: self.state.session_plans.values().cloned().collect(),
            session_adherence_reviews: self.session_adherence_reviews()?,
            trade_copiers: self.state.trade_copiers.values().cloned().collect(),
            copy_dispatches: self.copy_dispatches.iter().cloned().collect(),
            strategy_templates: self.state.strategy_templates.values().cloned().collect(),
            managed_brackets: {
                // Group-owned brackets project into the legacy reader view until M6.11
                // replaces it; pending groups carry no child state, activated ones carry
                // their real child ids and entry price.
                let mut brackets: Vec<ManagedBracket> =
                    self.state.managed_brackets.values().cloned().collect();
                for group in self.state.order_groups.values() {
                    if group.kind == crate::core::GroupKind::Bracket && !group.legs.is_empty() {
                        brackets.push(self.managed_bracket_view(group)?);
                    }
                }
                brackets
            },
            planned_bracket_legs: self.planned_bracket_legs(),
            protective_orders: self.state.protective_orders.values().cloned().collect(),
        })
    }

    pub(super) fn account_pnl(&self) -> Result<Vec<AccountPnl>, String> {
        self.state
            .accounts
            .values()
            .map(|account| -> Result<AccountPnl, String> {
                let zero = FixedPoint::try_new(0, account.currency_scale)
                    .map_err(|error| error.to_string())?;
                let (realized, unrealized) = self
                    .state
                    .positions
                    .values()
                    .filter(|position| position.account_id == account.id)
                    .try_fold(
                        (zero, zero),
                        |(realized, unrealized), position| -> Result<_, String> {
                            let realized_value = position
                                .realized_pnl
                                .exact_rescale(account.currency_scale)
                                .map_err(|error| error.to_string())?;
                            let unrealized_value = position
                                .unrealized_pnl
                                .exact_rescale(account.currency_scale)
                                .map_err(|error| error.to_string())?;
                            Ok((
                                realized
                                    .checked_add(realized_value)
                                    .map_err(|error| error.to_string())?,
                                unrealized
                                    .checked_add(unrealized_value)
                                    .map_err(|error| error.to_string())?,
                            ))
                        },
                    )?;
                if account.broker_ref.is_some() {
                    return self.broker_account_pnl(account, realized, unrealized);
                }
                let balance = account
                    .starting_equity
                    .map(|starting| starting.checked_add(realized))
                    .transpose()
                    .map_err(|error| error.to_string())?;
                Ok(AccountPnl {
                    account_id: account.id.clone(),
                    currency: account.currency.clone(),
                    realized,
                    unrealized: Some(unrealized),
                    balance,
                    equity: balance
                        .map(|balance| balance.checked_add(unrealized))
                        .transpose()
                        .map_err(|error| error.to_string())?,
                })
            })
            .collect()
    }

    /// A broker account adds the broker-reported realized P&L of every deal Aeris recorded and
    /// the broker's net unrealized P&L of its open positions. Its balance is the broker's and its
    /// equity is that balance plus the open P&L. Open P&L, balance and equity need the venue to
    /// reach the account now, so a disconnected account shows none of them rather than a stale
    /// value.
    pub(super) fn broker_account_pnl(
        &self,
        account: &TradingAccount,
        realized: FixedPoint,
        unrealized: FixedPoint,
    ) -> Result<AccountPnl, String> {
        let realized = realized
            .checked_add(self.broker_realized_since(
                &account.id,
                i64::MIN,
                account.currency_scale,
            )?)
            .map_err(|error| error.to_string())?;
        let unrealized = self
            .broker_unrealized(&account.id, account.currency_scale)?
            .map(|broker| unrealized.checked_add(broker))
            .transpose()
            .map_err(|error| error.to_string())?;
        let balance = self
            .state
            .broker_balances
            .get(&account.id)
            .filter(|_| unrealized.is_some())
            .map(|balance| balance.exact_rescale(account.currency_scale))
            .transpose()
            .map_err(|error| error.to_string())?;
        let equity = balance
            .zip(unrealized)
            .map(|(balance, unrealized)| balance.checked_add(unrealized))
            .transpose()
            .map_err(|error| error.to_string())?;
        Ok(AccountPnl {
            account_id: account.id.clone(),
            currency: account.currency.clone(),
            realized,
            unrealized,
            balance,
            equity,
        })
    }

    pub(super) fn session_adherence_reviews(&self) -> Result<Vec<SessionAdherenceReview>, String> {
        self.state
            .session_plans
            .values()
            .map(|plan| {
                let fills = self
                    .state
                    .fills
                    .iter()
                    .filter(|fill| fill.account_id == plan.account_id)
                    .collect::<Vec<_>>();
                let pnl = self.plan_session_pnl(plan)?;
                Ok(SessionAdherenceReview {
                    account_id: plan.account_id.clone(),
                    plan_id: plan.plan_id.clone(),
                    plan_revision: plan.revision,
                    checklist_completed: plan
                        .checklist
                        .iter()
                        .filter(|item| item.completed)
                        .count(),
                    checklist_total: plan.checklist.len(),
                    active_setup: plan.active_setup.clone(),
                    retained_fill_count: fills.len(),
                    fills_outside_planned_hours: fills
                        .iter()
                        .filter(|fill| !plan.contains_time(fill.execution_unix_nanos))
                        .count(),
                    maximum_loss_respected: pnl.units() > -plan.maximum_loss.units(),
                })
            })
            .collect()
    }

    pub(super) fn projected_risk_locks(&self) -> Vec<RiskLock> {
        let mut locks = self.state.risk_locks.values().cloned().collect::<Vec<_>>();
        for state in self.state.discipline_states.values() {
            let Some(until) = state.cooldown_until_unix_nanos else {
                continue;
            };
            if locks.iter().any(|lock| lock.account_id == state.account_id) {
                continue;
            }
            let profile = self.state.risk_profiles.get(&state.account_id);
            locks.push(RiskLock {
                account_id: state.account_id.clone(),
                reason: format!("rapid-loss cooldown active until {until}"),
                locked_at_unix_nanos: state.last_loss_unix_nanos.unwrap_or(1),
                profile_id: profile.map(|profile| profile.profile_id.clone()),
                profile_version: profile.map(|profile| profile.version),
            });
        }
        locks.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        locks
    }

    pub(super) fn position_pnl(&self) -> Result<Vec<PositionPnl>, String> {
        self.state
            .positions
            .values()
            .map(|position| {
                let account = self
                    .state
                    .accounts
                    .get(&position.account_id)
                    .ok_or_else(|| "position account is not registered".to_string())?;
                let instrument = self
                    .state
                    .instruments
                    .get(&position.instrument_id)
                    .ok_or_else(|| "position instrument is not registered".to_string())?;
                let point_value = instrument
                    .contract
                    .point_value
                    .and_then(|value| FixedPoint::try_new(value.units(), value.scale()).ok());
                let ticks = tick_value(instrument, account.currency_scale).ok();
                Ok(PositionPnl {
                    realized_ticks: ticks
                        .and_then(|tick_value| pnl_ticks(position.realized_pnl, tick_value)),
                    unrealized_ticks: ticks
                        .and_then(|tick_value| pnl_ticks(position.unrealized_pnl, tick_value)),
                    point_value,
                    currency_scale: account.currency_scale,
                    position: position.clone(),
                })
            })
            .collect()
    }

    /// One meter per risk profile; an unreachable broker account has no meter until its open
    /// P&L is known again.
    pub(super) fn risk_meters(&self) -> Result<Vec<RiskMeter>, String> {
        self.state
            .risk_profiles
            .values()
            .map(|profile| -> Result<Option<RiskMeter>, String> {
                let current_realized_pnl = self.realized_since_session(profile)?;
                let Some(current_session_pnl) = self.session_pnl(profile)? else {
                    return Ok(None);
                };
                let loss_used = negative_loss(current_session_pnl)?;
                let daily_loss_remaining = remaining_limit(profile.daily_loss_limit, loss_used)?;
                let trailing_drawdown_remaining = profile
                    .trailing_drawdown
                    .map(|drawdown| {
                        let state = self
                            .state
                            .risk_rule_states
                            .get(&profile.account_id)
                            .ok_or_else(|| "risk rule state is unavailable".to_string())?;
                        let floor = subtract_fixed(state.peak_session_pnl, drawdown)?;
                        nonnegative_difference(current_session_pnl, floor)
                    })
                    .transpose()?;
                let projected_contracts = FixedPoint::try_new(
                    i64::try_from(self.projected_contracts_for_account(
                        &profile.account_id,
                        profile.max_contracts.scale(),
                        None,
                    )?)
                    .map_err(|_| "risk contract quantity overflowed".to_string())?,
                    profile.max_contracts.scale(),
                )
                .map_err(|error| error.to_string())?;
                let contracts_remaining =
                    remaining_limit(profile.max_contracts, projected_contracts)?;
                let (consistency_current_percent, consistency_additional_profit_required) = profile
                    .consistency_max_single_trade_percent
                    .map(|limit| {
                        let state = self
                            .state
                            .risk_rule_states
                            .get(&profile.account_id)
                            .ok_or_else(|| "risk rule state is unavailable".to_string())?;
                        let (current, additional) = consistency_projection(state, limit)?;
                        Ok::<_, String>((Some(current), Some(additional)))
                    })
                    .transpose()?
                    .unwrap_or((None, None));
                Ok(Some(RiskMeter {
                    account_id: profile.account_id.clone(),
                    profile_id: profile.profile_id.clone(),
                    profile_version: profile.version,
                    enabled: profile.enabled,
                    current_realized_pnl,
                    daily_loss_remaining,
                    trailing_drawdown_remaining,
                    contracts_remaining,
                    consistency_max_single_trade_percent: profile
                        .consistency_max_single_trade_percent,
                    consistency_current_percent,
                    consistency_additional_profit_required,
                    restricted_until_unix_nanos: profile.restricted_until_unix_nanos,
                    lock_reason: self
                        .state
                        .risk_locks
                        .get(&profile.account_id)
                        .map(|lock| lock.reason.clone())
                        .or_else(|| {
                            self.state
                                .discipline_states
                                .get(&profile.account_id)
                                .and_then(|state| state.cooldown_until_unix_nanos)
                                .map(|until| format!("rapid-loss cooldown active until {until}"))
                        }),
                }))
            })
            .filter_map(Result::transpose)
            .collect()
    }
}
