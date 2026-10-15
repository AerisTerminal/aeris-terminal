//! Round-trip projection of the bounded fill history. One simulated trade spans a net position
//! cycle: from the fill that opens a flat position to the fill that flattens or reverses it, with
//! every scale-in and partial exit in between. A reversing fill closes one trade and opens the
//! next. One broker trade spans one broker-identified position.
//!
//! Every exit fill, partial or final, is its own closed round trip: the quantity it closed, at
//! the trade's average entry, with that fill's realized P&L. A trade still open is one more
//! round trip that is not closed.

use super::weighted_average_entry_price;
use aeris_instruments::InstrumentId;
use aeris_trading::{Fill, FillId, FixedPoint, OrderSide, Position, TradingAccountId};
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

type PositionKey = (TradingAccountId, InstrumentId);

/// Volume-weighted summary of the fills on one side of a round trip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TradeLeg {
    pub first_unix_nanos: i64,
    pub last_unix_nanos: i64,
    /// Averaged with the same rounding policy as a position's average entry price.
    pub average_price: FixedPoint,
    pub quantity: FixedPoint,
}

impl TradeLeg {
    fn new(fill: &Fill, quantity: FixedPoint) -> Self {
        Self {
            first_unix_nanos: fill.execution_unix_nanos,
            last_unix_nanos: fill.execution_unix_nanos,
            average_price: fill.price,
            quantity,
        }
    }

    fn add(&mut self, fill: &Fill, quantity: FixedPoint) -> Result<(), String> {
        self.average_price = weighted_average_entry_price(
            self.average_price,
            self.quantity.units().unsigned_abs(),
            fill.price,
            quantity.units().unsigned_abs(),
        )?;
        self.quantity = self
            .quantity
            .checked_add(quantity)
            .map_err(|error| error.to_string())?;
        self.last_unix_nanos = fill.execution_unix_nanos;
        Ok(())
    }
}

/// One executed trade, projected from the trading owner's fills: either the part of a trade one
/// exit fill closed, or a trade that is still open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeRoundTrip {
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    /// Opening side: buy for a long trade, sell for a short one.
    pub side: OrderSide,
    /// The fills that opened or added to the trade before this exit; a closed round trip carries
    /// the quantity its exit closed. `None` when the trade opened before the retained fill
    /// history, so its entry price is unknown.
    pub entry: Option<TradeLeg>,
    /// The exit fill of a closed round trip, or every exit so far of an open trade.
    pub exit: Option<TradeLeg>,
    pub closed: bool,
    /// Realized P&L of a closed round trip's exit. `None` while open, or when the owner has no
    /// record of it.
    pub final_pnl: Option<FixedPoint>,
    /// Realized exits in this cycle plus the current unrealized position P&L. `None` for a
    /// closed trade or when the owner's cycle baseline is unknown.
    pub open_pnl: Option<FixedPoint>,
}

impl TradeRoundTrip {
    /// The owner's known P&L for this trade: live while open, final after close.
    #[must_use]
    pub fn current_pnl(&self) -> Option<FixedPoint> {
        if self.closed {
            self.final_pnl
        } else {
            self.open_pnl
        }
    }

    /// Time of the trade's most recent fill.
    #[must_use]
    pub fn last_fill_unix_nanos(&self) -> i64 {
        let entry = self.entry.map_or(i64::MIN, |leg| leg.last_unix_nanos);
        let exit = self.exit.map_or(i64::MIN, |leg| leg.last_unix_nanos);
        entry.max(exit)
    }
}

struct OpenTrade {
    side: OrderSide,
    entry: Option<TradeLeg>,
    entry_known: bool,
    exit: Option<TradeLeg>,
    /// Sum of the exits' recorded realized P&L, `None` once any exit lacks a record.
    exit_realized_pnl: Option<FixedPoint>,
    exits_recorded: bool,
}

impl OpenTrade {
    fn opened(fill: &Fill, quantity: FixedPoint) -> Self {
        Self {
            side: fill.side,
            entry: Some(TradeLeg::new(fill, quantity)),
            entry_known: true,
            exit: None,
            exit_realized_pnl: None,
            exits_recorded: true,
        }
    }

    /// A trade whose opening fill is older than the retained history.
    fn already_open(net_quantity_before: i64) -> Self {
        Self {
            side: if net_quantity_before > 0 {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            },
            entry: None,
            entry_known: false,
            exit: None,
            exit_realized_pnl: None,
            exits_recorded: true,
        }
    }

    fn add_entry(&mut self, fill: &Fill, quantity: FixedPoint) -> Result<(), String> {
        add_to_leg(&mut self.entry, fill, quantity)
    }

    fn add_exit(
        &mut self,
        fill: &Fill,
        quantity: FixedPoint,
        realized_pnl: Option<FixedPoint>,
    ) -> Result<(), String> {
        add_to_leg(&mut self.exit, fill, quantity)?;
        match (realized_pnl, self.exit_realized_pnl) {
            (None, _) => self.exits_recorded = false,
            (Some(pnl), None) => self.exit_realized_pnl = Some(pnl),
            (Some(pnl), Some(sum)) => {
                self.exit_realized_pnl =
                    Some(sum.checked_add(pnl).map_err(|error| error.to_string())?);
            }
        }
        Ok(())
    }

    /// The closed round trip of `fill`, an exit closing `closed` of this trade, recorded before
    /// the exit is added to the trade.
    fn closing(
        &self,
        key: &PositionKey,
        fill: &Fill,
        closed: FixedPoint,
        realized_pnl: Option<FixedPoint>,
    ) -> TradeRoundTrip {
        TradeRoundTrip {
            account_id: key.0.clone(),
            instrument_id: key.1.clone(),
            side: self.side,
            entry: self
                .entry
                .filter(|_| self.entry_known)
                .map(|entry| TradeLeg {
                    quantity: closed,
                    ..entry
                }),
            exit: Some(TradeLeg::new(fill, closed)),
            closed: true,
            final_pnl: realized_pnl,
            open_pnl: None,
        }
    }

    fn still_open(self, key: &PositionKey, open_pnl: Option<FixedPoint>) -> TradeRoundTrip {
        TradeRoundTrip {
            account_id: key.0.clone(),
            instrument_id: key.1.clone(),
            side: self.side,
            entry: self.entry.filter(|_| self.entry_known),
            exit: self.exit,
            closed: false,
            final_pnl: None,
            open_pnl,
        }
    }

    fn open_pnl(
        &self,
        key: &PositionKey,
        positions: &BTreeMap<PositionKey, Position>,
        trade_cycles: &BTreeMap<PositionKey, crate::core::ledger::TradeCycle>,
    ) -> Result<Option<FixedPoint>, String> {
        let Some(position) = positions
            .get(key)
            .filter(|position| position.net_quantity.units().signum() == self.side.sign())
        else {
            return Ok(None);
        };
        let realized = match trade_cycles.get(key).filter(|cycle| cycle.pnl_known) {
            Some(cycle) => cycle.realized_pnl,
            None if self.entry_known && self.exits_recorded => self.exit_realized_pnl.unwrap_or(
                FixedPoint::try_new(0, position.unrealized_pnl.scale())
                    .map_err(|error| error.to_string())?,
            ),
            None => return Ok(None),
        };
        let realized = realized
            .exact_rescale(position.unrealized_pnl.scale())
            .map_err(|error| error.to_string())?;
        position
            .unrealized_pnl
            .checked_add(realized)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

fn add_to_leg(leg: &mut Option<TradeLeg>, fill: &Fill, quantity: FixedPoint) -> Result<(), String> {
    if let Some(leg) = leg {
        return leg.add(fill, quantity);
    }
    *leg = Some(TradeLeg::new(fill, quantity));
    Ok(())
}

/// Projects round trips, newest activity first, from fills ordered newest first.
///
/// Net quantity before each fill is reconstructed backward from the current position, so a
/// trade that opened before the retained history is still grouped correctly, with an unknown
/// entry rather than a misleading partial one.
pub(super) fn project_round_trips<'a>(
    fills_newest_first: impl DoubleEndedIterator<Item = &'a Fill> + Clone,
    positions: &BTreeMap<PositionKey, Position>,
    fill_accounting: &BTreeMap<FillId, crate::core::ledger::FillAccounting>,
    trade_cycles: &BTreeMap<PositionKey, crate::core::ledger::TradeCycle>,
) -> Result<Vec<TradeRoundTrip>, String> {
    let mut net_after = BTreeMap::<PositionKey, FixedPoint>::new();
    let mut quantity_before = Vec::new();
    for fill in fills_newest_first.clone() {
        let key = (fill.account_id.clone(), fill.instrument_id.clone());
        let after = match net_after.get(&key) {
            Some(after) => *after,
            None => positions.get(&key).map_or_else(
                || FixedPoint::try_new(0, fill.quantity.scale()).map_err(|e| e.to_string()),
                |position| Ok(position.net_quantity),
            )?,
        };
        let signed = signed_quantity(fill, after.scale())?;
        let before = FixedPoint::try_new(
            after
                .units()
                .checked_sub(signed.units())
                .ok_or_else(|| "round-trip quantity overflowed".to_string())?,
            after.scale(),
        )
        .map_err(|error| error.to_string())?;
        net_after.insert(key, before);
        quantity_before.push((before, signed));
    }

    let mut open = BTreeMap::<PositionKey, OpenTrade>::new();
    let mut round_trips = Vec::new();
    for (fill, (before, signed)) in fills_newest_first
        .rev()
        .zip(quantity_before.into_iter().rev())
    {
        let key = (fill.account_id.clone(), fill.instrument_id.clone());
        let scale = before.scale();
        let before = before.units();
        let after = before
            .checked_add(signed.units())
            .ok_or_else(|| "round-trip quantity overflowed".to_string())?;
        if before == 0 {
            open.insert(key, OpenTrade::opened(fill, absolute(signed)?));
            continue;
        }
        let trade = open
            .entry(key.clone())
            .or_insert_with(|| OpenTrade::already_open(before));
        if before.signum() == signed.units().signum() {
            trade.add_entry(fill, absolute(signed)?)?;
            continue;
        }
        let closed_units = before.unsigned_abs().min(signed.units().unsigned_abs());
        let closed = quantity(closed_units, scale)?;
        let closes_trade = after == 0 || after.signum() != before.signum();
        let realized_pnl = fill_accounting
            .get(&fill.id)
            .and_then(|row| row.realized_gross);
        // Fills recorded before per-fill P&L carry only the completed trade's P&L, which is
        // this exit's own when it is the only exit of a fully retained trade.
        let realized_pnl = realized_pnl.or_else(|| {
            (closes_trade && trade.entry_known && trade.exit.is_none())
                .then(|| {
                    fill_accounting
                        .get(&fill.id)
                        .and_then(|row| row.cycle_realized)
                })
                .flatten()
        });
        round_trips.push(trade.closing(&key, fill, closed, realized_pnl));
        trade.add_exit(fill, closed, realized_pnl)?;
        if !closes_trade {
            continue;
        }
        open.remove(&key);
        if after != 0 {
            open.insert(
                key,
                OpenTrade::opened(fill, quantity(after.unsigned_abs(), scale)?),
            );
        }
    }
    for (key, trade) in open {
        let open_pnl = trade.open_pnl(&key, positions, trade_cycles)?;
        round_trips.push(trade.still_open(&key, open_pnl));
    }
    round_trips.sort_by_key(|trip| std::cmp::Reverse(trip.last_fill_unix_nanos()));
    Ok(round_trips)
}

/// Projects the closing fills of broker trades, newest activity first, from broker fills ordered
/// newest first.
///
/// A broker trade is one broker position: hedged positions are identified by the broker, not
/// netted per instrument. Its first retained fill sets the trade side unless the opening fill
/// is older than the retained history, in which case only the exits are known. A partial close
/// of a position the broker still holds is listed as soon as it fills; the remaining position
/// is shown with open positions.
pub(super) fn project_closed_broker_round_trips<'a>(
    fills_newest_first: impl DoubleEndedIterator<Item = &'a Fill>,
    fill_positions: &BTreeMap<FillId, String>,
    fill_accounting: &BTreeMap<FillId, crate::core::ledger::FillAccounting>,
) -> Result<Vec<TradeRoundTrip>, String> {
    let mut trades = BTreeMap::<(TradingAccountId, String), OpenTrade>::new();
    let mut round_trips = Vec::new();
    for fill in fills_newest_first.rev() {
        let Some(position_id) = fill_positions.get(&fill.id) else {
            continue;
        };
        let realized_pnl = fill_accounting
            .get(&fill.id)
            .and_then(|row| row.realized_gross);
        let trade = match trades.entry((fill.account_id.clone(), position_id.clone())) {
            Entry::Occupied(trade) => trade.into_mut(),
            // A closing fill opens nothing; seen first, the opening fill was not retained.
            Entry::Vacant(slot) if realized_pnl.is_some() => {
                slot.insert(OpenTrade::already_open(-fill.side.sign()))
            }
            Entry::Vacant(slot) => {
                slot.insert(OpenTrade::opened(fill, fill.quantity));
                continue;
            }
        };
        if fill.side == trade.side {
            trade.add_entry(fill, fill.quantity)?;
            continue;
        }
        let key = (fill.account_id.clone(), fill.instrument_id.clone());
        round_trips.push(trade.closing(&key, fill, fill.quantity, realized_pnl));
    }
    round_trips.sort_by_key(|trip| std::cmp::Reverse(trip.last_fill_unix_nanos()));
    Ok(round_trips)
}

fn signed_quantity(fill: &Fill, scale: u8) -> Result<FixedPoint, String> {
    let quantity = fill
        .quantity
        .exact_rescale(scale)
        .map_err(|error| format!("fill quantity does not match its position scale: {error}"))?;
    FixedPoint::try_new(
        quantity
            .units()
            .checked_mul(fill.side.sign())
            .ok_or_else(|| "round-trip quantity overflowed".to_string())?,
        scale,
    )
    .map_err(|error| error.to_string())
}

fn absolute(quantity: FixedPoint) -> Result<FixedPoint, String> {
    self::quantity(quantity.units().unsigned_abs(), quantity.scale())
}

fn quantity(units: u64, scale: u8) -> Result<FixedPoint, String> {
    FixedPoint::try_new(
        i64::try_from(units).map_err(|_| "round-trip quantity overflowed".to_string())?,
        scale,
    )
    .map_err(|error| error.to_string())
}
