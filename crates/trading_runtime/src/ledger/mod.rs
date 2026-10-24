mod equity;
mod fees;
mod position;
pub(crate) use equity::{EQUITY_RING_CAPACITY, EQUITY_SAMPLE_INTERVAL_NANOS};
#[cfg(test)]
pub(crate) use fees::fee_for_fill;
pub(crate) use position::{CostBasis, FillAccounting, TradeCycle};

use super::{
    AccountEnvironment, AccountPnl, DisciplineState, Fill, FillMemoryUpdate, FillPolicyUpdate,
    FixedPoint, InstrumentId, LedgerEntry, LedgerEntryKind, Liquidity, MAXIMUM_SNAPSHOT_ITEMS,
    Order, OrderEvent, Position, PositionPnl, ProtectionRole, RiskLock, RiskMeter,
    SessionAdherenceReview, TradeRoundTrip, TradingAccount, TradingAccountId, TradingCore,
    TradingSnapshot, consistency_projection, negative_loss, nonnegative_difference, pnl_ticks,
    remaining_limit, round_trip, subtract_fixed, tick_value,
};

/// The durable per-position projection folded from pruned fills: the exact state after every
/// fill retention deleted for that account and instrument. Recovery replays the checkpoint
/// plus the retained fill suffix and lands on the pre-retention projection exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PositionCheckpoint {
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub net_quantity: FixedPoint,
    pub average_entry_price: Option<FixedPoint>,
    pub realized_pnl: FixedPoint,
    pub cost_basis: Option<CostBasis>,
    pub last_fill_unix_nanos: i64,
    /// Every fill folded into this checkpoint; the retained suffix continues from here.
    pub fill_count: u64,
}

/// The durable per-account cash balance folded from pruned ledger entries. A ledgered
/// account's balance is this checkpoint plus the exact sum of its remaining entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LedgerCheckpoint {
    pub account_id: TradingAccountId,
    pub balance: FixedPoint,
    /// Every ledger entry folded into this checkpoint.
    pub entry_count: u64,
    pub at_unix_nanos: i64,
}
impl TradingCore {
    pub(super) fn next_discipline_state(&self, fill: &Fill) -> DisciplineState {
        let mut state = self
            .state
            .discipline_states
            .get(&fill.account_id)
            .cloned()
            .unwrap_or_else(|| DisciplineState::new(fill.account_id.clone()));
        if self.is_stop_fill(fill) {
            state.last_stop_fill_unix_nanos = Some(fill.execution_unix_nanos);
        }
        state
    }

    pub(super) fn is_stop_fill(&self, fill: &Fill) -> bool {
        self.state.orders.get(&fill.order_id).is_some_and(|order| {
            self.owned_leg_role(&order.client_order_id) == Some(ProtectionRole::StopLoss)
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
            .ledger_entries
            .extend(update.policy_update.ledger_entries.iter().cloned());
        match update.cost_basis {
            Some(basis) => self
                .state
                .cost_basis
                .insert(update.position_key.clone(), basis),
            None => self.state.cost_basis.remove(&update.position_key),
        };
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
        // One equity sample per fill: the ring updates now, and the sample travels to the
        // durable history with the committing transaction (quote, placement and emergency
        // steps). A speculative plan that rolls back is cleared.
        if let Some(sample) = self.record_equity_sample(&fill.account_id, fill.execution_unix_nanos)
        {
            self.pending_equity_samples.push(sample);
        }
    }

    /// Round trips, open and closed, of every account within the snapshot bound; every exit
    /// fill, partial or final, is one closed round trip.
    pub(super) fn round_trips(&self) -> Result<Vec<TradeRoundTrip>, String> {
        round_trip::project_round_trips(
            self.state.fills.iter().rev().take(MAXIMUM_SNAPSHOT_ITEMS),
            &self.state.positions,
            &self.state.fill_accounting,
            &self.state.trade_cycles,
        )
    }

    pub(crate) fn snapshot(&self) -> Result<TradingSnapshot, String> {
        // Orders are absent from this guard: retention.maximum_orders already bounds them
        // (250,000), and the compatibility snapshot must still succeed beyond 10,000 orders
        // (M5.11). New subscribers should use the bounded working set instead.
        if self.state.accounts.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.instruments.len() > MAXIMUM_SNAPSHOT_ITEMS
            || self.state.positions.len() > MAXIMUM_SNAPSHOT_ITEMS
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
            broker_balances: self.state.broker_balances.clone(),
            connected_broker_accounts: self.connected_broker_accounts.clone(),
            position_pnl: self.position_pnl()?,
            account_pnl: self.account_pnl()?,
            ledger_entries: self.state.ledger_entries.clone(),
            equity_samples: self.equity_sample_rows(),
            fee_schedules: self
                .state
                .fee_schedules
                .iter()
                .map(|(account, schedule)| (account.clone(), schedule.clone()))
                .collect(),
            risk_profiles: self.state.risk_profiles.values().cloned().collect(),
            risk_meters: self.risk_meters()?,
            risk_locks: self.projected_risk_locks(),
            risk_rule_states: self.state.risk_rule_states.values().cloned().collect(),
            risk_sessions: self.state.risk_sessions.values().cloned().collect(),
            discipline_states: self.state.discipline_states.values().cloned().collect(),
            session_plans: self.state.session_plans.values().cloned().collect(),
            session_adherence_reviews: self.session_adherence_reviews()?,
            trade_copiers: self.state.trade_copiers.values().cloned().collect(),
            copy_dispatches: self.copy_dispatches.iter().cloned().collect(),
            strategy_templates: self.state.strategy_templates.values().cloned().collect(),
            managed_brackets: self
                .state
                .order_groups
                .values()
                .filter(|group| {
                    group.kind == crate::core::GroupKind::Bracket && !group.legs.is_empty()
                })
                .map(|group| self.managed_bracket_view(group))
                .collect::<Result<Vec<_>, _>>()?,
            planned_legs: self.owned_planned_legs(),
            protective_orders: self.protection_order_views(),
            recovery_notices: self.recovery_notices.clone(),
            venue_notices: self.broker_sync.notices(),
        })
    }

    /// Every account's in-memory equity ring, flattened into one snapshot history.
    fn equity_sample_rows(&self) -> Vec<aeris_trading::EquitySample> {
        self.state
            .equity_rings
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    /// The current balance of one simulated account: the ledger checkpoint plus the exact
    /// sum of its remaining cash-ledger entries, or the legacy starting-equity math while a
    /// gated pre-group store awaits its conversion backfill.
    pub(super) fn simulated_balance(
        &self,
        account: &TradingAccount,
        realized: FixedPoint,
    ) -> Result<Option<FixedPoint>, String> {
        let checkpoint = self.state.ledger_checkpoints.get(&account.id);
        let ledgered = account.starting_equity.is_none()
            || checkpoint.is_some()
            || self
                .state
                .ledger_entries
                .iter()
                .any(|entry| entry.account_id == account.id);
        if ledgered {
            let mut total = match checkpoint {
                Some(checkpoint) => checkpoint.balance,
                None => FixedPoint::try_new(0, account.currency_scale)
                    .map_err(|error| error.to_string())?,
            };
            for entry in self
                .state
                .ledger_entries
                .iter()
                .filter(|entry| entry.account_id == account.id)
            {
                total = total
                    .checked_add(
                        entry
                            .amount
                            .exact_rescale(account.currency_scale)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
            }
            return Ok(Some(total));
        }
        account
            .starting_equity
            .map(|starting| starting.checked_add(realized))
            .transpose()
            .map_err(|error| error.to_string())
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
                let balance = self.simulated_balance(account, realized)?;
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

    /// A broker account's realized P&L is its positions', like a simulated account's; its
    /// open P&L is the venue's. Its balance is the broker's and its equity is that balance plus
    /// the open P&L. Open P&L, balance and equity need the venue to reach the account now, so a
    /// disconnected account shows none of them rather than a stale value.
    pub(super) fn broker_account_pnl(
        &self,
        account: &TradingAccount,
        realized: FixedPoint,
        unrealized: FixedPoint,
    ) -> Result<AccountPnl, String> {
        let unrealized = self
            .connected_broker_accounts
            .contains(&account.id)
            .then_some(unrealized);
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
        // The map is keyed by account, so its values are already in account order.
        self.state.risk_locks.values().cloned().collect()
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
                        .map(|lock| lock.reason.clone()),
                }))
            })
            .filter_map(Result::transpose)
            .collect()
    }
}
