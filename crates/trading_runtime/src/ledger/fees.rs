//! Simulated fee evaluation and fee-schedule installation.

use super::{AccountEnvironment, Fill, FixedPoint, TradingAccountId, TradingCore};
use crate::TradingInstrument;
use aeris_trading::{FeeModel, FeeSchedule, Liquidity, fixed_point_from_i128_rounded};

/// The exact non-negative fee of one simulated fill under the account's schedule, in the
/// account currency. Every model evaluates in one i128 expression and rounds once at the
/// account currency scale, so the charged fee matches the ledger entry exactly.
pub(crate) fn fee_for_fill(
    schedule: Option<&FeeSchedule>,
    instrument: &TradingInstrument,
    fill: &Fill,
    liquidity: Liquidity,
    currency_scale: u8,
) -> Result<FixedPoint, String> {
    let zero = FixedPoint::try_new(0, currency_scale).map_err(|error| error.to_string())?;
    let Some(schedule) = schedule else {
        return Ok(zero);
    };
    match schedule.model_for(instrument.contract.instrument_class, &fill.instrument_id) {
        FeeModel::None => Ok(zero),
        FeeModel::PerContract { per_side } => per_unit_charge(per_side, fill, currency_scale),
        FeeModel::PerUnit {
            per_unit,
            minimum,
            maximum_bps,
        } => {
            let mut fee = per_unit_charge(per_unit, fill, currency_scale)?;
            if let Some(minimum) = minimum {
                let minimum = fixed_point_from_i128_rounded(
                    i128::from(minimum.units()),
                    minimum.scale(),
                    currency_scale,
                )
                .map_err(|error| error.to_string())?;
                if minimum.units() > fee.units() {
                    fee = minimum;
                }
            }
            if let Some(bps) = maximum_bps {
                let cap = notional_fee(fill, instrument, bps, currency_scale)?;
                if cap.units() < fee.units() {
                    fee = cap;
                }
            }
            Ok(fee)
        }
        FeeModel::NotionalBps {
            maker_bps,
            taker_bps,
        } => {
            let bps = match liquidity {
                Liquidity::Maker => maker_bps,
                Liquidity::Taker => taker_bps,
            };
            notional_fee(fill, instrument, bps, currency_scale)
        }
    }
}

/// rate times quantity, rounded once into the account currency.
fn per_unit_charge(
    rate: FixedPoint,
    fill: &Fill,
    currency_scale: u8,
) -> Result<FixedPoint, String> {
    let raw = i128::from(rate.units()) * i128::from(fill.quantity.units());
    let raw_scale = rate.scale() + fill.quantity.scale();
    fixed_point_from_i128_rounded(raw, raw_scale, currency_scale).map_err(|error| error.to_string())
}

/// Basis points of the fill notional (price times quantity times point value), rounded once
/// into the account currency.
fn notional_fee(
    fill: &Fill,
    instrument: &TradingInstrument,
    bps: u32,
    currency_scale: u8,
) -> Result<FixedPoint, String> {
    let point_value = instrument
        .contract
        .point_value
        .ok_or_else(|| "a notional fee model needs the contract's point value".to_string())?;
    let raw = i128::from(fill.price.units())
        * i128::from(fill.quantity.units())
        * i128::from(point_value.units())
        * i128::from(bps);
    let raw_scale = fill.price.scale() + fill.quantity.scale() + point_value.scale() + 4;
    fixed_point_from_i128_rounded(raw, raw_scale, currency_scale).map_err(|error| error.to_string())
}

impl TradingCore {
    /// Validates and durably installs one simulated account's fee schedule.
    pub(crate) fn set_fee_schedule(
        &mut self,
        account_id: &TradingAccountId,
        schedule: FeeSchedule,
    ) -> Result<(), String> {
        let account = self
            .state
            .accounts
            .get(account_id)
            .ok_or_else(|| "fee schedule account is not registered".to_string())?;
        if account.environment != AccountEnvironment::Simulated {
            return Err("fee schedules apply to simulated accounts only".to_string());
        }
        schedule.validate().map_err(|error| error.to_string())?;
        for instrument_id in schedule.by_instrument.keys() {
            if !self.state.instruments.contains_key(instrument_id) {
                return Err(format!(
                    "fee schedule instrument {} is not registered",
                    instrument_id.as_str()
                ));
            }
        }
        self.store.put_fee_schedule(account_id, &schedule)?;
        self.state
            .fee_schedules
            .insert(account_id.clone(), schedule);
        self.bump_revision()
    }
}
