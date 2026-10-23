use super::{
    Fill, FillPolicyUpdate, FixedPoint, LedgerEntry, LedgerEntryKind, Liquidity, Position,
    TradingAccount, TradingCore, subtract_fixed,
};
use crate::risk::RiskRuleState;

/// The single durable per-instrument cycle. `pnl_known` distinguishes a legacy
/// open trade with no reliable baseline from a trade opened after V21.
#[derive(Clone, Eq, PartialEq)]
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

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct FillAccounting {
    pub cycle_id: i64,
    pub realized_gross: Option<FixedPoint>,
    /// The exact fee the account's schedule charged this fill, in the account currency.
    pub commission: FixedPoint,
    pub cycle_realized: Option<FixedPoint>,
    pub cycle_closed: bool,
}

/// The exact integer cost basis of an open simulated position: the sum of price times
/// quantity units over every open unit. Realized and unrealized P&L derive from it, so the
/// rounded display average never enters money math. A full close always consumes the
/// remaining basis exactly, so every completed round trip realizes its exact total.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CostBasis {
    /// The summed price-times-quantity units of the open quantity; never negative.
    pub units: i128,
    /// The combined decimal scale: instrument price scale plus quantity scale.
    pub scale: u8,
}

impl CostBasis {
    /// The basis opened by one fill.
    pub(crate) fn from_fill(fill: &Fill) -> Result<Self, String> {
        Ok(Self {
            units: fill_weight(fill)?,
            scale: fill
                .price
                .scale()
                .checked_add(fill.quantity.scale())
                .ok_or_else(|| "position cost basis scale overflowed".to_string())?,
        })
    }

    /// Rescales to another combined scale without rounding; mixed-generation fills with
    /// inexactly convertible scales reject rather than corrupt the basis.
    pub(crate) fn rescaled(self, target_scale: u8) -> Result<Self, String> {
        let units = match target_scale.cmp(&self.scale) {
            std::cmp::Ordering::Equal => self.units,
            std::cmp::Ordering::Greater => self
                .units
                .checked_mul(power_of_ten(target_scale - self.scale)?)
                .ok_or_else(|| "position cost basis overflowed".to_string())?,
            std::cmp::Ordering::Less => {
                let divisor = power_of_ten(self.scale - target_scale)?;
                if self.units % divisor != 0 {
                    return Err("position cost basis rescale is inexact".to_string());
                }
                self.units / divisor
            }
        };
        Ok(Self {
            units,
            scale: target_scale,
        })
    }

    /// Adds one same-direction fill's weight.
    pub(crate) fn added(self, fill: &Fill) -> Result<Self, String> {
        let fill = Self::from_fill(fill)?.rescaled(self.scale)?;
        Ok(Self {
            units: self
                .units
                .checked_add(fill.units)
                .ok_or_else(|| "position cost basis overflowed".to_string())?,
            scale: self.scale,
        })
    }

    /// Closes `closed` of `open` quantity units. A full close consumes the entire remaining
    /// basis; a partial close removes the pro-rata share rounded once at basis-unit
    /// granularity, so intermediate shares never drift the final total.
    pub(crate) fn close(self, open: u64, closed: u64) -> Result<(Option<Self>, i128), String> {
        if closed >= open {
            return Ok((None, self.units));
        }
        let share = aeris_trading::round_divide_i128(
            self.units
                .checked_mul(i128::from(closed))
                .ok_or_else(|| "position cost basis overflowed".to_string())?,
            i128::from(open),
        )
        .map_err(|error| error.to_string())?;
        Ok((
            Some(Self {
                units: self.units - share,
                scale: self.scale,
            }),
            share,
        ))
    }
}

/// The price-times-quantity weight of one fill at its own combined scale.
fn fill_weight(fill: &Fill) -> Result<i128, String> {
    i128::from(fill.price.units())
        .checked_mul(i128::from(fill.quantity.units()))
        .ok_or_else(|| "position cost basis overflowed".to_string())
}

fn power_of_ten(exponent: u8) -> Result<i128, String> {
    if exponent > 18 {
        return Err("position cost basis scale overflowed".to_string());
    }
    Ok(10_i128.pow(u32::from(exponent)))
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
        liquidity: Liquidity,
    ) -> Result<(Position, Option<CostBasis>, FillPolicyUpdate), String> {
        let previous_basis = self
            .state
            .cost_basis
            .get(&(fill.account_id.clone(), fill.instrument_id.clone()))
            .copied();
        let (position, next_basis) =
            crate::next_position(previous, previous_basis, fill, account, instrument)?;
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
        let mut rule_state = self.fill_rule_state(&fill.account_id)?;
        let discipline_state = self.next_discipline_state(fill);
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
        let fee = super::fees::fee_for_fill(
            self.state.fee_schedules.get(&fill.account_id),
            instrument,
            fill,
            liquidity,
            account.currency_scale,
        )?;
        let accounting = FillAccounting {
            cycle_id: fill_cycle_id,
            realized_gross: reduces.then_some(realized),
            commission: fee,
            cycle_realized: completed_pnl,
            cycle_closed: completed,
        };
        let ledger_entries = fill_ledger_entries(fill, &accounting)?;
        Ok((
            position,
            next_basis,
            FillPolicyUpdate {
                rule_state,
                trade_cycle: cycle,
                discipline_state,
                accounting,
                ledger_entries,
            },
        ))
    }
}

/// The cash-ledger entries one simulated fill commits: its exact realized P&L first, then
/// its commission, so the replayed chain reads realized P&L before commission per exit.
fn fill_ledger_entries(
    fill: &Fill,
    accounting: &FillAccounting,
) -> Result<Vec<LedgerEntry>, String> {
    let mut entries = Vec::new();
    if let Some(realized) = accounting
        .realized_gross
        .filter(|realized| realized.units() != 0)
    {
        entries.push(LedgerEntry {
            entry_id: format!("pnl-{}", fill.id.as_str()),
            account_id: fill.account_id.clone(),
            kind: LedgerEntryKind::RealizedPnl,
            amount: realized,
            fill_id: Some(fill.id.clone()),
            at_unix_nanos: fill.execution_unix_nanos,
        });
    }
    if accounting.commission.units() != 0 {
        entries.push(LedgerEntry {
            entry_id: format!("fee-{}", fill.id.as_str()),
            account_id: fill.account_id.clone(),
            kind: LedgerEntryKind::Commission,
            amount: FixedPoint::try_new(
                accounting
                    .commission
                    .units()
                    .checked_neg()
                    .ok_or_else(|| "ledger fee amount overflows".to_string())?,
                accounting.commission.scale(),
            )
            .map_err(|error| error.to_string())?,
            fill_id: Some(fill.id.clone()),
            at_unix_nanos: fill.execution_unix_nanos,
        });
    }
    for entry in &entries {
        entry.validate().map_err(|error| error.to_string())?;
    }
    Ok(entries)
}

impl TradingCore {
    /// The risk-rule state a fill may update, when the account has a profile.
    fn fill_rule_state(
        &self,
        account_id: &crate::TradingAccountId,
    ) -> Result<Option<RiskRuleState>, String> {
        self.state
            .risk_profiles
            .get(account_id)
            .map(|_| {
                self.state
                    .risk_rule_states
                    .get(account_id)
                    .cloned()
                    .ok_or_else(|| "risk rule state is unavailable".to_string())
            })
            .transpose()
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
