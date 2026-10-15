use super::{
    Fill, FillPolicyUpdate, FixedPoint, Position, TradingAccount, TradingCore, subtract_fixed,
};
use crate::risk::RiskRuleState;

/// The single durable per-instrument cycle. `pnl_known` distinguishes a legacy
/// open trade with no reliable baseline from a trade opened after V21.
#[derive(Clone)]
pub(crate) struct TradeCycle {
    pub account_id: crate::TradingAccountId,
    pub instrument_id: aeris_instruments::InstrumentId,
    pub cycle_id: i64,
    pub peak_quantity: FixedPoint,
    pub realized_pnl: FixedPoint,
    /// Winner/discipline accumulator since the latest risk-profile registration.
    pub risk_realized_pnl: FixedPoint,
    pub pnl_known: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct FillAccounting {
    pub cycle_id: i64,
    pub realized_gross: Option<FixedPoint>,
    pub cycle_realized: Option<FixedPoint>,
    pub cycle_closed: bool,
}

impl TradingCore {
    /// Compute position, per-fill accounting, cycle, and dependent policy from
    /// the same fill before any of them are committed or published.
    pub(crate) fn apply_fill(
        &self,
        fill: &Fill,
        previous: Option<&Position>,
        account: &TradingAccount,
        instrument: &crate::TradingInstrument,
    ) -> Result<(Position, FillPolicyUpdate), String> {
        let position = crate::next_position(previous, fill, account, instrument)?;
        let zero = FixedPoint::try_new(0, account.currency_scale).map_err(|e| e.to_string())?;
        let zero_quantity =
            FixedPoint::try_new(0, fill.quantity.scale()).map_err(|e| e.to_string())?;
        let key = (fill.account_id.clone(), fill.instrument_id.clone());
        let previous_quantity = previous.map_or(0, |p| p.net_quantity.units());
        let next_quantity = position.net_quantity.units();
        let realized = subtract_fixed(
            position.realized_pnl,
            previous.map_or(zero, |p| p.realized_pnl),
        )?;
        let reduces = previous_quantity != 0 && fill.side.sign() != previous_quantity.signum();
        let completed =
            reduces && (next_quantity == 0 || previous_quantity.signum() != next_quantity.signum());
        let mut cycle = self
            .state
            .trade_cycles
            .get(&key)
            .cloned()
            .unwrap_or(TradeCycle {
                account_id: fill.account_id.clone(),
                instrument_id: fill.instrument_id.clone(),
                cycle_id: 0,
                peak_quantity: zero_quantity,
                realized_pnl: zero,
                risk_realized_pnl: zero,
                pnl_known: previous_quantity == 0,
            });
        if previous_quantity == 0 {
            cycle.realized_pnl = zero;
            cycle.risk_realized_pnl = zero;
            cycle.peak_quantity = zero_quantity;
            cycle.pnl_known = true;
        }
        cycle.realized_pnl = cycle
            .realized_pnl
            .checked_add(
                realized
                    .exact_rescale(cycle.realized_pnl.scale())
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        cycle.risk_realized_pnl = cycle
            .risk_realized_pnl
            .checked_add(
                realized
                    .exact_rescale(cycle.risk_realized_pnl.scale())
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        update_peak(&mut cycle, fill, previous_quantity, next_quantity)?;
        let fill_cycle_id = cycle.cycle_id;
        let completed_pnl = (completed && cycle.pnl_known).then_some(cycle.realized_pnl);
        let completed_risk = completed.then_some((cycle.risk_realized_pnl, cycle.peak_quantity));
        let mut rule_state = self
            .state
            .risk_profiles
            .get(&fill.account_id)
            .map(|_| {
                self.state
                    .risk_rule_states
                    .get(&fill.account_id)
                    .cloned()
                    .ok_or_else(|| "risk rule state is unavailable".to_string())
            })
            .transpose()?;
        let discipline_state = self.next_discipline_state(fill, completed_risk)?;
        if completed {
            update_winner(rule_state.as_mut(), cycle.risk_realized_pnl)?;
            cycle.cycle_id = cycle
                .cycle_id
                .checked_add(1)
                .ok_or_else(|| "trade cycle id overflowed".to_string())?;
            cycle.realized_pnl = zero;
            cycle.risk_realized_pnl = zero;
            cycle.peak_quantity = zero_quantity;
            cycle.pnl_known = next_quantity != 0;
        }
        let accounting = FillAccounting {
            cycle_id: fill_cycle_id,
            realized_gross: reduces.then_some(realized),
            cycle_realized: completed_pnl,
            cycle_closed: completed,
        };
        Ok((
            position,
            FillPolicyUpdate {
                rule_state,
                trade_cycle: cycle,
                discipline_state,
                accounting,
            },
        ))
    }
}

fn update_peak(
    cycle: &mut TradeCycle,
    fill: &Fill,
    previous_quantity: i64,
    next_quantity: i64,
) -> Result<(), String> {
    let peak = previous_quantity
        .unsigned_abs()
        .max(next_quantity.unsigned_abs())
        .max(fill.quantity.units().unsigned_abs());
    let peak = FixedPoint::try_new(
        i64::try_from(peak).map_err(|_| "discipline quantity overflowed".to_string())?,
        fill.quantity.scale(),
    )
    .map_err(|e| e.to_string())?;
    if peak.units()
        > cycle
            .peak_quantity
            .exact_rescale(peak.scale())
            .map_err(|e| e.to_string())?
            .units()
    {
        cycle.peak_quantity = peak;
    }
    Ok(())
}

fn update_winner(state: Option<&mut RiskRuleState>, realized: FixedPoint) -> Result<(), String> {
    if realized.units() <= 0 {
        return Ok(());
    }
    let Some(state) = state else {
        return Ok(());
    };
    let winner = realized
        .exact_rescale(state.total_winning_pnl.scale())
        .map_err(|e| e.to_string())?;
    state.total_winning_pnl = state
        .total_winning_pnl
        .checked_add(winner)
        .map_err(|e| e.to_string())?;
    let winner = winner
        .exact_rescale(state.largest_winning_trade_pnl.scale())
        .map_err(|e| e.to_string())?;
    if winner.units() > state.largest_winning_trade_pnl.units() {
        state.largest_winning_trade_pnl = winner;
    }
    Ok(())
}
