//! Trading on a bar replay. A replaying chart shows past bars, so its order entry trades one
//! isolated practice session ([`BarReplayTrading`], owned logic in `trading_runtime`) that
//! fills against the bars the replay reveals. Nothing reaches an account, a venue, the trading
//! owner or its store, and the session ends with the replay. The pane opens the session at the
//! first replay order, feeds it each bar the replay reveals and projects its orders, position
//! and fills onto the chart in place of the account's. It survives timeframe switches on the
//! same instrument; stepping back before its newest trade or changing the instrument resets it.

use super::*;
use aeris_chart_integration::{ChartReplayBar, ChartReplayPhase};
use aeris_localization::panels;
use aeris_trading_runtime::{
    BarReplayTrading, BarReplayTradingError, ReplayBar, ReplayEntry, ReplayOrder,
    ReplayOrderRequest, ReplayOrderRole, ReplayOrderType,
};

/// Every replay session opens with 100,000.00 in the instrument's currency.
const STARTING_BALANCE_UNITS: i64 = 10_000_000;
const STARTING_BALANCE_SCALE: u8 = 2;
const NANOS_PER_SECOND: f64 = 1_000_000_000.0;
const NANOS_PER_MICRO: i64 = 1_000;
const ORDER_ID_PREFIX: &str = "replay-order:";
const FILL_ID_PREFIX: &str = "replay-fill:";
const BRACKET_ID_PREFIX: &str = "replay-bracket:";
const POSITION_ID_PREFIX: &str = "replay-position:";

/// Why a replay trading action did not run, worded for the order-entry status line.
#[derive(Clone, Debug, PartialEq)]
enum ReplayTradingRefusal {
    Intent(ChartIntentRejection),
    /// The order ticket could not price its order.
    Ticket(&'static str),
    /// The trading owner has published no verified terms for the chart's instrument.
    NoTerms,
    Session(BarReplayTradingError),
}

impl ReplayTradingRefusal {
    fn message(&self) -> &'static str {
        match self {
            Self::Intent(reason) => reason.message(),
            Self::Ticket(reason) => reason,
            Self::NoTerms => tr!(panels::REPLAY_TRADING_NO_TERMS),
            Self::Session(error) => match error {
                BarReplayTradingError::MissingPointValue => {
                    tr!(panels::REPLAY_TRADING_NO_POINT_VALUE)
                }
                BarReplayTradingError::NoBar => tr!(panels::REPLAY_TRADING_NO_BAR),
                BarReplayTradingError::InvalidPrice => tr!(workspace::INTENT_INVALID_PRICE),
                BarReplayTradingError::InvalidQuantity => tr!(workspace::INTENT_INVALID_QUANTITY),
                BarReplayTradingError::ProtectionOnWrongSide => {
                    tr!(panels::REPLAY_TRADING_WRONG_SIDE)
                }
                BarReplayTradingError::WorkingOrderLimit => {
                    tr!(panels::REPLAY_TRADING_ORDER_LIMIT)
                }
                BarReplayTradingError::UnknownOrder => tr!(panels::REPLAY_TRADING_UNKNOWN_ORDER),
                BarReplayTradingError::NoPosition => tr!(panels::REPLAY_TRADING_NO_POSITION),
                BarReplayTradingError::Accounting(_) => tr!(panels::REPLAY_TRADING_FAILED),
            },
        }
    }
}

impl From<BarReplayTradingError> for ReplayTradingRefusal {
    fn from(error: BarReplayTradingError) -> Self {
        Self::Session(error)
    }
}

impl From<ChartIntentRejection> for ReplayTradingRefusal {
    fn from(reason: ChartIntentRejection) -> Self {
        Self::Intent(reason)
    }
}

/// The newest revealed close and the instrument's tick, in price units at the replay
/// session's price scale.
#[derive(Clone, Copy, Debug)]
pub(super) struct ReplayTouch {
    pub(super) close_units: i64,
    pub(super) tick_units: Option<i64>,
}

/// An order the order ticket, a trading shortcut or the chart trade bar sends to the replay.
#[derive(Clone, Copy)]
pub(super) struct ReplayTicketOrder {
    pub(super) side: aeris_trading::OrderSide,
    pub(super) quantity: aeris_desktop::trading::OrderQuantity,
    /// The ticket's own stop and target in ticks from the entry, or from the close for a
    /// market order; a selected order template replaces it.
    pub(super) bracket: Option<aeris_desktop::trading::TicketBracket>,
}

/// What the order ticket shows while its chart replays; empty before the first replay order.
#[derive(Clone, Debug, Default)]
pub(super) struct ReplayTicket {
    pub(super) currency: Option<String>,
    pub(super) net_quantity: Option<aeris_trading::FixedPoint>,
    pub(super) open_pnl: Option<aeris_trading::FixedPoint>,
    pub(super) realized_pnl: Option<aeris_trading::FixedPoint>,
    pub(super) working_orders: usize,
}

impl WorkspaceSurface {
    /// Brings the replay's trading session to the replay clock: it ends with the replay,
    /// resets when replay moved before its newest trade or onto another instrument, and
    /// otherwise trades the bars revealed since it last looked. The chart then shows the
    /// session in place of the account, or the account again once replay ended.
    pub(super) fn sync_bar_replay_trading(&mut self, cx: &mut Context<Self>) {
        let state = self
            .chart
            .as_ref()
            .map(|chart| chart.read(cx).bar_replay_state());
        let Some(state) = state.filter(|state| state.phase != ChartReplayPhase::Off) else {
            if self.bar_replay.trading.take().is_some() {
                self.refresh_pane_trading_projection(cx);
            }
            return;
        };
        let instrument_changed = self.bar_replay.trading.as_ref().is_some_and(|session| {
            self.product.as_ref().is_none_or(|product| {
                session.instrument().instrument_id.as_str() != product.instrument_id
            })
        });
        let stepped_back = state.clock_micros.is_some_and(|clock| {
            self.bar_replay
                .trading
                .as_ref()
                .is_some_and(|session| session.traded_beyond(clock.saturating_mul(NANOS_PER_MICRO)))
        });
        if instrument_changed || stepped_back {
            self.reset_bar_replay_trading(cx);
        } else if state.clock_micros.is_some() {
            self.advance_bar_replay_trading(cx);
        }
        self.refresh_pane_trading_projection(cx);
    }

    fn reset_bar_replay_trading(&mut self, cx: &mut Context<Self>) {
        if self
            .bar_replay
            .trading
            .take()
            .is_some_and(|session| session.has_activity())
        {
            bar_replay::post_bar_replay_notice(
                "bar_replay_trading_reset",
                tr!(panels::REPLAY_TRADES_RESET_TITLE),
                tr!(panels::REPLAY_TRADES_RESET_DETAIL),
                cx,
            );
        }
    }

    /// Feeds the session every bar the replay revealed since its newest one; a new session
    /// starts at the newest revealed bar.
    fn advance_bar_replay_trading(&mut self, cx: &App) {
        let (Some(chart), Some(session)) = (self.chart.as_ref(), self.bar_replay.trading.as_mut())
        else {
            return;
        };
        let from = session
            .last_bar()
            .and_then(|bar| bar.open_unix_nanos.to_f64())
            .map(|nanos| nanos / NANOS_PER_SECOND);
        let price_scale = u32::from(session.instrument().price_scale);
        let bars = chart
            .read(cx)
            .bar_replay_revealed_bars(from)
            .iter()
            .map(|bar| replay_bar(bar, price_scale, self.interval))
            .collect::<Option<Vec<_>>>();
        let result = bars
            .ok_or(BarReplayTradingError::InvalidPrice)
            .and_then(|bars| session.advance(bars));
        if let Err(error) = result {
            diagnostic!("Bar replay trading could not follow the replay: {error}");
        }
    }

    /// The replay session, opened on the chart instrument's verified terms at the first order.
    fn bar_replay_trading_session(
        &mut self,
        cx: &App,
    ) -> Result<&mut BarReplayTrading, ReplayTradingRefusal> {
        if self.bar_replay.trading.is_none() {
            let product = self
                .product
                .as_ref()
                .ok_or(ChartIntentRejection::NoInstrument)?;
            let instrument = self
                .lifecycle
                .trading_store()
                .and_then(|store| {
                    store.read(cx).current().and_then(|set| {
                        set.instruments
                            .iter()
                            .find(|terms| terms.instrument_id.as_str() == product.instrument_id)
                            .cloned()
                    })
                })
                .ok_or(ReplayTradingRefusal::NoTerms)?;
            let balance =
                aeris_trading::FixedPoint::try_new(STARTING_BALANCE_UNITS, STARTING_BALANCE_SCALE)
                    .map_err(|error| BarReplayTradingError::Accounting(error.to_string()))?;
            self.bar_replay.trading = Some(BarReplayTrading::new(instrument, balance)?);
            self.advance_bar_replay_trading(cx);
        }
        self.bar_replay
            .trading
            .as_mut()
            .ok_or(ReplayTradingRefusal::Session(BarReplayTradingError::NoBar))
    }

    /// A chart trading action on a replaying chart, run against the replay session.
    pub(super) fn dispatch_bar_replay_intent(
        &mut self,
        chart: &Entity<AerisChartView>,
        intent: &ChartTradingIntent,
        cx: &mut Context<Self>,
    ) {
        let result = self.apply_bar_replay_intent(intent, cx);
        let accepted = self.record_bar_replay_result(result, cx);
        chart.update(cx, |chart, _| {
            chart.resolve_trading_intent(intent.sequence, accepted);
        });
    }

    /// Shows a replay trading result on the pane's status line and re-projects the chart.
    /// Returns whether the action ran.
    fn record_bar_replay_result(
        &mut self,
        result: Result<&'static str, ReplayTradingRefusal>,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = result.map(str::to_string).map_err(|refusal| {
            let message = refusal.message();
            diagnostic!("Bar replay refused a trading action: {refusal:?}");
            message.to_string()
        });
        let accepted = result.is_ok();
        self.lifecycle
            .trading_feedback()
            .record_for_pane(&self.trading_pane_token, result);
        self.refresh_pane_trading_projection(cx);
        cx.notify();
        accepted
    }

    fn apply_bar_replay_intent(
        &mut self,
        intent: &ChartTradingIntent,
        cx: &App,
    ) -> Result<&'static str, ReplayTradingRefusal> {
        let session = self.bar_replay_trading_session(cx)?;
        let price_scale = u32::from(session.instrument().price_scale);
        let price = |value: Option<f64>| {
            value
                .and_then(|value| chart_price_fixed_point(value, price_scale))
                .ok_or(ChartIntentRejection::InvalidPrice)
        };
        match intent.action {
            ChartTradingIntentAction::PlaceBracketOrder => {
                let quantity_scale = u32::from(session.instrument().quantity_scale);
                session.place(ReplayOrderRequest {
                    side: order_side(intent.side.ok_or(ChartIntentRejection::Incomplete)?),
                    entry: ReplayEntry::At(price(intent.price)?),
                    quantity: intent
                        .quantity
                        .and_then(|quantity| chart_quantity_fixed_point(quantity, quantity_scale))
                        .ok_or(ChartIntentRejection::InvalidQuantity)?,
                    stop_loss: Some(price(intent.stop_loss_price)?),
                    take_profit: Some(price(intent.take_profit_price)?),
                })?;
                Ok(tr!(panels::REPLAY_ORDER_PLACED))
            }
            ChartTradingIntentAction::CreateStopLoss
            | ChartTradingIntentAction::CreateTakeProfit => {
                let position_id = replay_position_id(session);
                if position_id.is_none() || intent.position_id != position_id {
                    return Err(ChartIntentRejection::NotOnChart.into());
                }
                let role = if intent.action == ChartTradingIntentAction::CreateStopLoss {
                    ReplayOrderRole::StopLoss
                } else {
                    ReplayOrderRole::TakeProfit
                };
                session.protect(role, price(intent.price)?)?;
                Ok(tr!(panels::REPLAY_PROTECTION_PLACED))
            }
            ChartTradingIntentAction::ModifyOrder => {
                session.modify(replay_order_id(intent)?, price(intent.price)?)?;
                Ok(tr!(panels::REPLAY_ORDER_MOVED))
            }
            ChartTradingIntentAction::CancelOrder => {
                session.cancel(replay_order_id(intent)?)?;
                Ok(tr!(panels::REPLAY_ORDER_CANCELLED))
            }
            ChartTradingIntentAction::ClosePosition => {
                if intent.position_id != replay_position_id(session) {
                    return Err(ChartIntentRejection::NotOnChart.into());
                }
                session.close_position()?;
                Ok(tr!(panels::REPLAY_POSITION_CLOSED))
            }
        }
    }

    /// A Buy or Sell at the market on a replaying chart (the chart trade bar or a trading
    /// shortcut): it fills at the newest revealed close, protected by the selected order
    /// template's bracket.
    pub(super) fn place_bar_replay_market_order(
        &mut self,
        side: aeris_trading::OrderSide,
        quantity: aeris_desktop::trading::OrderQuantity,
        cx: &mut Context<Self>,
    ) {
        let order = ReplayTicketOrder {
            side,
            quantity,
            bracket: None,
        };
        self.place_bar_replay_ticket_order(order, |_| Ok(None), cx);
    }

    /// An order on a replaying chart. `price` turns the newest revealed close into the entry
    /// price in price units, or `None` for a market order.
    pub(super) fn place_bar_replay_ticket_order(
        &mut self,
        order: ReplayTicketOrder,
        price: impl FnOnce(ReplayTouch) -> Result<Option<i64>, &'static str>,
        cx: &mut Context<Self>,
    ) {
        let result = self.apply_bar_replay_ticket_order(order, price, cx);
        self.record_bar_replay_result(result, cx);
    }

    fn apply_bar_replay_ticket_order(
        &mut self,
        order: ReplayTicketOrder,
        price: impl FnOnce(ReplayTouch) -> Result<Option<i64>, &'static str>,
        cx: &App,
    ) -> Result<&'static str, ReplayTradingRefusal> {
        let bracket = self
            .order_template_choice(cx)
            .replay_bracket(order.bracket)
            .map_err(ReplayTradingRefusal::Ticket)?;
        let session = self.bar_replay_trading_session(cx)?;
        let price_scale = session.instrument().price_scale;
        let tick_units = tick_units(session.instrument());
        let quantity = order
            .quantity
            .fixed_point(session.instrument().quantity_scale)
            .ok_or(ChartIntentRejection::InvalidQuantity)?;
        let close_units = session
            .last_bar()
            .ok_or(BarReplayTradingError::NoBar)?
            .close
            .units();
        let entry_units = price(ReplayTouch {
            close_units,
            tick_units,
        })
        .map_err(ReplayTradingRefusal::Ticket)?;
        let fixed = |units: Option<i64>| {
            units
                .and_then(|units| aeris_trading::FixedPoint::try_new(units, price_scale).ok())
                .ok_or(BarReplayTradingError::InvalidPrice)
        };
        let (stop_loss, take_profit) = match bracket {
            None => (None, None),
            Some(bracket) => {
                let tick = tick_units.ok_or(ReplayTradingRefusal::Ticket(tr!(
                    panels::TICKET_PRICE_INCREMENT_UNKNOWN
                )))?;
                let reference = entry_units.unwrap_or(close_units);
                let away = |ticks: u32, below: bool| {
                    let distance = i64::from(ticks).checked_mul(tick)?;
                    if below {
                        reference.checked_sub(distance)
                    } else {
                        reference.checked_add(distance)
                    }
                };
                let long = order.side == aeris_trading::OrderSide::Buy;
                (
                    Some(fixed(away(bracket.stop_ticks, long))?),
                    Some(fixed(away(bracket.target_ticks, !long))?),
                )
            }
        };
        let entry = match entry_units {
            Some(units) => ReplayEntry::At(fixed(Some(units))?),
            None => ReplayEntry::Market,
        };
        session.place(ReplayOrderRequest {
            side: order.side,
            entry,
            quantity,
            stop_loss,
            take_profit,
        })?;
        Ok(tr!(panels::REPLAY_ORDER_PLACED))
    }

    /// Cancels every working replay order.
    pub(super) fn cancel_bar_replay_orders(&mut self, cx: &mut Context<Self>) {
        let result = match self.bar_replay.trading.as_mut() {
            Some(session) => cancel_every_replay_order(session),
            None => Ok(()),
        }
        .map(|()| tr!(panels::REPLAY_ORDERS_CANCELLED))
        .map_err(ReplayTradingRefusal::from);
        self.record_bar_replay_result(result, cx);
    }

    /// Cancels every working replay order and closes the replay position at the newest close.
    pub(super) fn flatten_bar_replay(&mut self, cx: &mut Context<Self>) {
        let result = match self.bar_replay.trading.as_mut() {
            Some(session) => cancel_every_replay_order(session).and_then(|()| {
                if session.position().is_some() {
                    session.close_position()
                } else {
                    Ok(())
                }
            }),
            None => Ok(()),
        }
        .map(|()| tr!(panels::REPLAY_FLATTENED))
        .map_err(ReplayTradingRefusal::from);
        self.record_bar_replay_result(result, cx);
    }

    /// What the order ticket shows while the chart replays; `None` otherwise.
    pub(super) fn bar_replay_ticket(&self, cx: &App) -> Option<ReplayTicket> {
        if !self.bar_replay_on(cx) {
            return None;
        }
        let Some(session) = self.bar_replay.trading.as_ref() else {
            return Some(ReplayTicket::default());
        };
        let position = session.position();
        Some(ReplayTicket {
            currency: Some(session.currency().to_string()),
            net_quantity: position.map(|position| position.net_quantity),
            open_pnl: Some(session.unrealized_pnl()),
            realized_pnl: Some(session.realized_pnl()),
            working_orders: session.orders().len(),
        })
    }

    /// Re-projects the pane's trading onto its chart: the replay session while the chart
    /// replays, the selected account otherwise.
    fn refresh_pane_trading_projection(&mut self, cx: &mut Context<Self>) {
        if let Some(store) = self.lifecycle.trading_store() {
            workspace_layout::apply_trading_snapshot(self, &store, cx);
        }
    }

    /// The replay session on the chart, while the chart replays. An empty projection before
    /// the first replay order keeps the account's orders off a chart showing the past.
    pub(super) fn bar_replay_chart_trading(&self, cx: &App) -> Option<ChartTradingSnapshot> {
        if !self.bar_replay_on(cx) {
            return None;
        }
        let Some(session) = self.bar_replay.trading.as_ref() else {
            return Some(ChartTradingSnapshot::default());
        };
        let instrument = self
            .product
            .as_ref()
            .and_then(|product| chart_instrument_metadata(product, session.currency()))
            .unwrap_or_default();
        let position_id = replay_position_id(session);
        let orders = session
            .orders()
            .iter()
            .filter_map(|order| replay_chart_order(order, session.orders(), position_id.clone()))
            .collect();
        let positions = session
            .position()
            .zip(position_id)
            .and_then(|(position, id)| {
                Some(ChartTradingPosition {
                    id,
                    account_id: None,
                    pane_index: 0,
                    price_scale: ChartTradingPriceScale::Right,
                    side: if position.net_quantity.units() > 0 {
                        ChartPositionSide::Long
                    } else {
                        ChartPositionSide::Short
                    },
                    average_price: chart_price(Some(position.average_entry_price))?,
                    quantity: chart_quantity(position.net_quantity)?,
                    display_pnl: chart_price(Some(position.unrealized_pnl)),
                    currency: Some(session.currency().to_string()),
                    annotations: Vec::new(),
                })
            })
            .into_iter()
            .collect();
        let executions = session
            .fills()
            .filter_map(|fill| {
                Some(ChartTradingExecution {
                    id: ChartExecutionId::new(format!("{FILL_ID_PREFIX}{}", fill.id)).ok()?,
                    account_id: None,
                    pane_index: 0,
                    price_scale: ChartTradingPriceScale::Right,
                    side: chart_order_side(fill.side),
                    kind: ChartExecutionKind::PartialFill,
                    time: chart_time_seconds_from_unix_nanos(fill.executed_unix_nanos),
                    price: chart_price(Some(fill.price))?,
                    quantity: chart_quantity(fill.quantity)?,
                    order_id: chart_replay_order_id(fill.order_id),
                    position_id: None,
                    marker_shape: ChartExecutionMarkerShape::default(),
                    size_by_quantity: false,
                })
            })
            .collect();
        Some(ChartTradingSnapshot {
            instrument,
            positions,
            orders,
            executions,
            round_trips: Vec::new(),
        })
    }

    /// The replay session's balance for the replay panel, once the session exists.
    pub(super) fn bar_replay_balance_label(&self) -> Option<String> {
        self.bar_replay.trading.as_ref().map(|session| {
            tr!(
                panels::REPLAY_BALANCE,
                amount = order_ticket::format_money(session.currency(), session.balance())
            )
        })
    }
}

fn cancel_every_replay_order(session: &mut BarReplayTrading) -> Result<(), BarReplayTradingError> {
    // Cancelling an entry also cancels the protections waiting for it, so each pass takes
    // whatever order is first; the session bounds how many there are.
    while let Some(order_id) = session.orders().first().map(|order| order.id) {
        session.cancel(order_id)?;
    }
    Ok(())
}

/// The instrument's tick in price units at its price scale, when its terms name one that
/// fits that scale.
fn tick_units(instrument: &aeris_trading_runtime::TradingInstrument) -> Option<i64> {
    let tick = instrument.contract.tick_size.as_ref()?;
    aeris_trading::FixedPoint::try_new(tick.units(), tick.scale())
        .ok()?
        .exact_rescale(instrument.price_scale)
        .ok()
        .map(aeris_trading::FixedPoint::units)
        .filter(|units| *units > 0)
}

fn replay_chart_order(
    order: &ReplayOrder,
    orders: &[ReplayOrder],
    position_id: Option<ChartPositionId>,
) -> Option<ChartWorkingOrder> {
    let price = chart_price(Some(order.price))?;
    let carries_legs = order.role == ReplayOrderRole::Entry
        && orders.iter().any(|leg| leg.entry_id == Some(order.id));
    let bracket_id = order
        .entry_id
        .or(carries_legs.then_some(order.id))
        .and_then(|entry| ChartTradingGroupId::new(format!("{BRACKET_ID_PREFIX}{entry}")).ok());
    let protects_position = order.role != ReplayOrderRole::Entry && order.entry_id.is_none();
    Some(ChartWorkingOrder {
        id: chart_replay_order_id(order.id)?,
        account_id: None,
        pane_index: 0,
        price_scale: ChartTradingPriceScale::Right,
        side: chart_order_side(order.side),
        kind: match order.order_type {
            ReplayOrderType::Limit => ChartOrderKind::Limit,
            ReplayOrderType::Stop => ChartOrderKind::Stop,
        },
        role: match order.role {
            ReplayOrderRole::Entry => ChartOrderRole::Working,
            ReplayOrderRole::StopLoss => ChartOrderRole::StopLoss,
            ReplayOrderRole::TakeProfit => ChartOrderRole::TakeProfit,
        },
        status: ChartOrderStatus::Working,
        price,
        stop_price: (order.order_type == ReplayOrderType::Stop).then_some(price),
        trailing_trigger_price: None,
        break_even_trigger_price: None,
        quantity: chart_quantity(order.quantity)?,
        filled_quantity: 0.0,
        position_id: position_id.filter(|_| protects_position),
        parent_order_id: order.entry_id.and_then(chart_replay_order_id),
        bracket_id: bracket_id.clone(),
        oco_group_id: bracket_id,
        revision: 0,
        annotations: Vec::new(),
    })
}

fn chart_replay_order_id(id: u64) -> Option<ChartOrderId> {
    ChartOrderId::new(format!("{ORDER_ID_PREFIX}{id}")).ok()
}

fn replay_order_id(intent: &ChartTradingIntent) -> Result<u64, ChartIntentRejection> {
    intent
        .order_id
        .as_ref()
        .and_then(|id| id.as_str().strip_prefix(ORDER_ID_PREFIX)?.parse().ok())
        .ok_or(ChartIntentRejection::NotOnChart)
}

fn replay_position_id(session: &BarReplayTrading) -> Option<ChartPositionId> {
    session.position()?;
    ChartPositionId::new(format!(
        "{POSITION_ID_PREFIX}{}",
        session.instrument().instrument_id.as_str()
    ))
    .ok()
}

const fn order_side(side: ChartOrderSide) -> aeris_trading::OrderSide {
    match side {
        ChartOrderSide::Buy => aeris_trading::OrderSide::Buy,
        ChartOrderSide::Sell => aeris_trading::OrderSide::Sell,
    }
}

/// A revealed chart bar at the instrument's price scale. The newest bar the chart holds has no
/// next bar yet, so it ends one interval after it opens; an interval without a fixed length
/// (ticks, months) ends it just after its open, so a later bar is never mistaken for time the
/// session already traded.
fn replay_bar(
    bar: &ChartReplayBar,
    price_scale: u32,
    interval: ChartInterval,
) -> Option<ReplayBar> {
    let unix_nanos = |seconds: f64| (seconds * NANOS_PER_SECOND).round().to_i64();
    let open_unix_nanos = unix_nanos(bar.open_time)?;
    let end_unix_nanos = match bar.end_time {
        Some(end) => unix_nanos(end)?,
        None => match interval.aggregation() {
            ChartAggregation::FixedSeconds(seconds) => {
                open_unix_nanos.checked_add(i64::from(seconds.get()).checked_mul(1_000_000_000)?)?
            }
            ChartAggregation::Trades(_) | ChartAggregation::CalendarMonth => {
                open_unix_nanos.checked_add(1)?
            }
        },
    };
    let price = |value: f64| chart_price_fixed_point(value, price_scale);
    Some(ReplayBar {
        open_unix_nanos,
        end_unix_nanos,
        open: price(bar.open)?,
        high: price(bar.high)?,
        low: price(bar.low)?,
        close: price(bar.close)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(end_time: Option<f64>) -> ChartReplayBar {
        ChartReplayBar {
            open_time: 1_700_000_100.0,
            end_time,
            open: 100.25,
            high: 101.5,
            low: 99.75,
            close: 100.0,
        }
    }

    #[test]
    fn revealed_bars_convert_at_the_instrument_scale() {
        let converted =
            replay_bar(&bar(Some(1_700_000_400.0)), 2, ChartInterval::Minute5).expect("bar");
        assert_eq!(converted.open_unix_nanos, 1_700_000_100_000_000_000);
        assert_eq!(converted.end_unix_nanos, 1_700_000_400_000_000_000);
        assert_eq!(converted.open.units(), 10_025);
        assert_eq!(converted.high.units(), 10_150);
        assert_eq!(converted.low.units(), 9_975);
        assert_eq!(converted.close.units(), 10_000);
        assert_eq!(converted.close.scale(), 2);
        assert_eq!(replay_bar(&bar(None), 0, ChartInterval::Minute5), None);
    }

    #[test]
    fn the_newest_bar_ends_one_interval_after_its_open() {
        let fixed = replay_bar(&bar(None), 2, ChartInterval::Minute15).expect("bar");
        assert_eq!(fixed.end_unix_nanos, 1_700_001_000_000_000_000);
        let ticks = replay_bar(&bar(None), 2, ChartInterval::Tick100).expect("bar");
        assert_eq!(ticks.end_unix_nanos, ticks.open_unix_nanos + 1);
    }

    #[test]
    fn replay_order_identities_round_trip_and_reject_account_orders() {
        let intent = |order_id: &str| ChartTradingIntent {
            order_id: Some(ChartOrderId::new(order_id).expect("order id")),
            ..chart_intent_fixture()
        };
        let id = chart_replay_order_id(42).expect("replay order id");
        assert_eq!(replay_order_id(&intent(id.as_str())), Ok(42));
        assert_eq!(
            replay_order_id(&intent("chart-7-1000")),
            Err(ChartIntentRejection::NotOnChart)
        );
    }

    fn chart_intent_fixture() -> ChartTradingIntent {
        ChartTradingIntent {
            sequence: 1,
            action: ChartTradingIntentAction::CancelOrder,
            account_id: None,
            drawing_id: None,
            order_id: None,
            position_id: None,
            pane_index: None,
            price_scale: None,
            side: None,
            kind: None,
            role: None,
            price: None,
            price_tick_index: None,
            stop_price: None,
            take_profit_price: None,
            stop_loss_price: None,
            quantity: None,
            bracket_id: None,
            oco_group_id: None,
            base_revision: 0,
        }
    }
}
