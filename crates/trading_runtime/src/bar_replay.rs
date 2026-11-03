//! Trading on a bar replay. A replay shows past bars, so its orders never reach an account,
//! a venue, the store or the live quote book: each replay owns one isolated in-memory practice
//! session that fills against the bars the replay reveals. Fills use the simulated venue's
//! touch policies and the owner's position and P&L math; nothing outlives the session.

use super::execution::{FillPrice, Liquidity, RestingKind, TouchLiquidity, TouchPrice, bar_touch};
use super::ledger::CostBasis;
use super::market::unrealized_pnl;
use super::{TradingInstrument, next_position};
use aeris_trading::{
    AccountEnvironment, Fill, FillId, FixedPoint, OrderId, OrderSide, Position, TradingAccount,
    TradingAccountId, TradingProvenance,
};
use std::collections::VecDeque;
use std::fmt;

/// Working orders one replay session holds, protections included.
pub const MAXIMUM_REPLAY_WORKING_ORDERS: usize = 64;
/// Fills a replay session keeps for its chart markers; older ones are dropped.
pub const MAXIMUM_REPLAY_FILLS: usize = 500;
const REPLAY_ACCOUNT_ID: &str = "bar-replay";
const REPLAY_VENUE_ID: &str = "aeris-bar-replay";

/// One bar the replay revealed, at the instrument's price scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayBar {
    pub open_unix_nanos: i64,
    /// Where the bar ends (exclusive): the next bar's open. Bars of another interval that
    /// open before the newest seen bar ends cover time the session already traded.
    pub end_unix_nanos: i64,
    pub open: FixedPoint,
    pub high: FixedPoint,
    pub low: FixedPoint,
    pub close: FixedPoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayOrderRole {
    Entry,
    StopLoss,
    TakeProfit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayOrderType {
    Limit,
    Stop,
}

/// A working replay order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayOrder {
    pub id: u64,
    pub side: OrderSide,
    pub order_type: ReplayOrderType,
    pub role: ReplayOrderRole,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    /// The entry a protection waits for; `None` once it protects the position.
    pub entry_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayFill {
    pub id: u64,
    pub order_id: u64,
    pub side: OrderSide,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub executed_unix_nanos: i64,
    /// The P&L this fill realized; zero for a fill that only opens or adds.
    pub realized_pnl: FixedPoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayPosition {
    /// Positive long, negative short.
    pub net_quantity: FixedPoint,
    pub average_entry_price: FixedPoint,
    /// At the close of the newest revealed bar.
    pub unrealized_pnl: FixedPoint,
}

/// Where a new replay order enters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayEntry {
    /// At the close of the newest revealed bar.
    Market,
    /// Rests at the price: a limit on the favorable side of the close, a stop on the other.
    /// A price at the close fills there at once.
    At(FixedPoint),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayOrderRequest {
    pub side: OrderSide,
    pub entry: ReplayEntry,
    pub quantity: FixedPoint,
    pub stop_loss: Option<FixedPoint>,
    pub take_profit: Option<FixedPoint>,
}

/// Why a replay session refused a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BarReplayTradingError {
    /// The instrument's terms carry no point value, so P&L cannot be computed.
    MissingPointValue,
    /// No bar is revealed yet, so there is no price to trade at.
    NoBar,
    InvalidPrice,
    InvalidQuantity,
    /// A stop loss or take profit on the side of the price where it would fill at once.
    ProtectionOnWrongSide,
    WorkingOrderLimit,
    UnknownOrder,
    NoPosition,
    /// Position or P&L arithmetic failed.
    Accounting(String),
}

impl fmt::Display for BarReplayTradingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPointValue => formatter.write_str("the instrument has no point value"),
            Self::NoBar => formatter.write_str("no replay bar is revealed yet"),
            Self::InvalidPrice => formatter.write_str("the price is not valid for the instrument"),
            Self::InvalidQuantity => {
                formatter.write_str("the quantity is not valid for the instrument")
            }
            Self::ProtectionOnWrongSide => {
                formatter.write_str("the stop loss or take profit is on the wrong side")
            }
            Self::WorkingOrderLimit => write!(
                formatter,
                "a replay holds at most {MAXIMUM_REPLAY_WORKING_ORDERS} working orders"
            ),
            Self::UnknownOrder => formatter.write_str("the replay order is not working"),
            Self::NoPosition => formatter.write_str("the replay has no open position"),
            Self::Accounting(detail) => write!(formatter, "replay accounting failed: {detail}"),
        }
    }
}

impl std::error::Error for BarReplayTradingError {}

/// One bar replay's practice session on one instrument.
#[derive(Clone, Debug)]
pub struct BarReplayTrading {
    account: TradingAccount,
    instrument: TradingInstrument,
    point_value: FixedPoint,
    starting_balance: FixedPoint,
    balance: FixedPoint,
    position: Option<Position>,
    basis: Option<CostBasis>,
    orders: Vec<ReplayOrder>,
    fills: VecDeque<ReplayFill>,
    last_bar: Option<ReplayBar>,
    next_id: u64,
}

impl BarReplayTrading {
    /// Opens a session on verified instrument terms with a starting balance in the
    /// instrument's currency; the balance's scale is the session's currency scale.
    ///
    /// # Errors
    /// Returns [`BarReplayTradingError::MissingPointValue`] when the terms carry no point value.
    pub fn new(
        instrument: TradingInstrument,
        starting_balance: FixedPoint,
    ) -> Result<Self, BarReplayTradingError> {
        let point_value = instrument
            .contract
            .point_value
            .ok_or(BarReplayTradingError::MissingPointValue)?;
        let point_value =
            FixedPoint::try_new(point_value.units(), point_value.scale()).map_err(accounting)?;
        let account = TradingAccount {
            id: TradingAccountId::try_new(REPLAY_ACCOUNT_ID).map_err(accounting)?,
            display_name: REPLAY_ACCOUNT_ID.to_string(),
            environment: AccountEnvironment::Simulated,
            venue_id: REPLAY_VENUE_ID.to_string(),
            broker_ref: None,
            currency: instrument.contract.currency.clone(),
            currency_scale: starting_balance.scale(),
            starting_equity: Some(starting_balance),
        };
        Ok(Self {
            account,
            instrument,
            point_value,
            starting_balance,
            balance: starting_balance,
            position: None,
            basis: None,
            orders: Vec::new(),
            fills: VecDeque::new(),
            last_bar: None,
            next_id: 1,
        })
    }

    #[must_use]
    pub const fn instrument(&self) -> &TradingInstrument {
        &self.instrument
    }

    #[must_use]
    pub fn currency(&self) -> &str {
        &self.account.currency
    }

    /// The newest bar the session has seen.
    #[must_use]
    pub const fn last_bar(&self) -> Option<&ReplayBar> {
        self.last_bar.as_ref()
    }

    #[must_use]
    pub fn orders(&self) -> &[ReplayOrder] {
        &self.orders
    }

    /// The retained fills, oldest first.
    pub fn fills(&self) -> impl Iterator<Item = &ReplayFill> {
        self.fills.iter()
    }

    #[must_use]
    pub const fn balance(&self) -> FixedPoint {
        self.balance
    }

    /// The P&L every closed trade of this session realized.
    #[must_use]
    pub fn realized_pnl(&self) -> FixedPoint {
        difference(self.balance, self.starting_balance)
    }

    /// The open position at the newest bar's close.
    #[must_use]
    pub fn position(&self) -> Option<ReplayPosition> {
        let position = self.position.as_ref()?;
        Some(ReplayPosition {
            net_quantity: position.net_quantity,
            average_entry_price: position.average_entry_price?,
            unrealized_pnl: self.unrealized_pnl(),
        })
    }

    /// The open position's P&L at the newest bar's close; zero while flat.
    #[must_use]
    pub fn unrealized_pnl(&self) -> FixedPoint {
        let zero = self.zero();
        match (self.position.as_ref(), self.last_bar) {
            (Some(position), Some(bar)) => {
                unrealized_pnl(self.basis, position, bar.close, self.point_value).unwrap_or(zero)
            }
            _ => zero,
        }
    }

    /// Feeds bars the replay revealed, oldest first. A bar that opens before the newest seen
    /// bar ends covers time already traded and is ignored, so stepping back or switching to
    /// another interval never trades the same moment twice. The newest bar seen again as it
    /// forms trades only the move it added since.
    ///
    /// # Errors
    /// Returns an error for a bar off the instrument's price scale or failed accounting.
    pub fn advance(
        &mut self,
        bars: impl IntoIterator<Item = ReplayBar>,
    ) -> Result<(), BarReplayTradingError> {
        for bar in bars {
            for price in [bar.open, bar.high, bar.low, bar.close] {
                self.checked_price(price)?;
            }
            let traded = match self.last_bar {
                Some(last)
                    if bar.open_unix_nanos == last.open_unix_nanos
                        && bar.end_unix_nanos == last.end_unix_nanos =>
                {
                    if bar == last {
                        continue;
                    }
                    formed_since(&last, &bar)
                }
                Some(last) if bar.open_unix_nanos < last.end_unix_nanos => continue,
                _ => bar,
            };
            self.fill_resting(&traded)?;
            self.last_bar = Some(bar);
        }
        Ok(())
    }

    /// Whether the session already traded past this replay moment, as after stepping back or
    /// choosing an earlier start bar. Its orders and position then belong to a later moment.
    #[must_use]
    pub fn traded_beyond(&self, clock_unix_nanos: i64) -> bool {
        self.last_bar
            .is_some_and(|bar| bar.open_unix_nanos > clock_unix_nanos)
    }

    /// Whether the session holds any order, position or fill.
    #[must_use]
    pub fn has_activity(&self) -> bool {
        !self.orders.is_empty() || self.position.is_some() || !self.fills.is_empty()
    }

    /// Places an order, with an optional stop loss and take profit that protect the position
    /// it opens. Returns the entry's order identifier.
    ///
    /// # Errors
    /// Returns an error before any bar is revealed, for an invalid price or quantity, a
    /// protection on the wrong side, or a full order book.
    pub fn place(&mut self, request: ReplayOrderRequest) -> Result<u64, BarReplayTradingError> {
        let bar = self.last_bar.ok_or(BarReplayTradingError::NoBar)?;
        let quantity = self.checked_quantity(request.quantity)?;
        let reference = match request.entry {
            ReplayEntry::Market => bar.close,
            ReplayEntry::At(price) => self.checked_price(price)?,
        };
        let protections = [
            (ReplayOrderRole::StopLoss, request.stop_loss),
            (ReplayOrderRole::TakeProfit, request.take_profit),
        ];
        for (role, price) in protections {
            if let Some(price) = price {
                let price = self.checked_price(price)?;
                if !protects(request.side, role, price, reference) {
                    return Err(BarReplayTradingError::ProtectionOnWrongSide);
                }
            }
        }
        let needed = 1 + protections
            .iter()
            .filter(|(_, price)| price.is_some())
            .count();
        if self.orders.len() + needed > MAXIMUM_REPLAY_WORKING_ORDERS {
            return Err(BarReplayTradingError::WorkingOrderLimit);
        }
        let entry_id = self.take_id();
        let order_type = resting_type(request.side, reference, bar.close);
        self.orders.push(ReplayOrder {
            id: entry_id,
            side: request.side,
            order_type: order_type.unwrap_or(ReplayOrderType::Limit),
            role: ReplayOrderRole::Entry,
            price: reference,
            quantity,
            entry_id: None,
        });
        for (role, price) in protections {
            if let Some(price) = price {
                let id = self.take_id();
                self.orders.push(protection(
                    id,
                    opposite(request.side),
                    role,
                    price,
                    quantity,
                    Some(entry_id),
                ));
            }
        }
        if order_type.is_none() {
            self.execute(entry_id, bar.close, bar.open_unix_nanos)?;
        }
        Ok(entry_id)
    }

    /// Moves a working order. An order moved to where the market already is fills at the
    /// newest close, as it would on placement.
    ///
    /// # Errors
    /// Returns an error for an unknown order, an invalid price or a protection moved to the
    /// wrong side of its pending entry.
    pub fn modify(
        &mut self,
        order_id: u64,
        price: FixedPoint,
    ) -> Result<(), BarReplayTradingError> {
        let bar = self.last_bar.ok_or(BarReplayTradingError::NoBar)?;
        let price = self.checked_price(price)?;
        let index = self.order_index(order_id)?;
        let order = self.orders[index];
        if let Some(entry_id) = order.entry_id {
            let entry = self.orders[self.order_index(entry_id)?];
            if !protects(entry.side, order.role, price, entry.price) {
                return Err(BarReplayTradingError::ProtectionOnWrongSide);
            }
            self.orders[index].price = price;
            return Ok(());
        }
        self.orders[index].price = price;
        if marketable(order.side, order.order_type, price, bar.close) {
            self.execute(order_id, bar.close, bar.open_unix_nanos)?;
        }
        Ok(())
    }

    /// Cancels a working order and every protection waiting for it.
    ///
    /// # Errors
    /// Returns [`BarReplayTradingError::UnknownOrder`] when the order is not working.
    pub fn cancel(&mut self, order_id: u64) -> Result<(), BarReplayTradingError> {
        self.order_index(order_id)?;
        self.orders
            .retain(|order| order.id != order_id && order.entry_id != Some(order_id));
        Ok(())
    }

    /// Protects the open position with a stop loss or take profit for its whole quantity,
    /// replacing the one of that role it had. Returns the protection's order identifier.
    ///
    /// # Errors
    /// Returns an error while flat, for an invalid price, a price on the wrong side of the
    /// newest close, or a full order book.
    pub fn protect(
        &mut self,
        role: ReplayOrderRole,
        price: FixedPoint,
    ) -> Result<u64, BarReplayTradingError> {
        let bar = self.last_bar.ok_or(BarReplayTradingError::NoBar)?;
        let price = self.checked_price(price)?;
        let position = self
            .position
            .as_ref()
            .ok_or(BarReplayTradingError::NoPosition)?;
        let side = if position.net_quantity.units() > 0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        if role == ReplayOrderRole::Entry || !protects(side, role, price, bar.close) {
            return Err(BarReplayTradingError::ProtectionOnWrongSide);
        }
        let quantity = FixedPoint::try_new(
            position.net_quantity.units().abs(),
            position.net_quantity.scale(),
        )
        .map_err(accounting)?;
        self.orders
            .retain(|order| !(order.role == role && order.entry_id.is_none()));
        if self.orders.len() >= MAXIMUM_REPLAY_WORKING_ORDERS {
            return Err(BarReplayTradingError::WorkingOrderLimit);
        }
        let id = self.take_id();
        self.orders
            .push(protection(id, opposite(side), role, price, quantity, None));
        Ok(id)
    }

    /// Closes the open position at the newest close and cancels its protections.
    ///
    /// # Errors
    /// Returns [`BarReplayTradingError::NoPosition`] while flat, or failed accounting.
    pub fn close_position(&mut self) -> Result<(), BarReplayTradingError> {
        let bar = self.last_bar.ok_or(BarReplayTradingError::NoBar)?;
        let position = self
            .position
            .as_ref()
            .ok_or(BarReplayTradingError::NoPosition)?;
        let side = if position.net_quantity.units() > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let quantity = FixedPoint::try_new(
            position.net_quantity.units().abs(),
            position.net_quantity.scale(),
        )
        .map_err(accounting)?;
        let order_id = self.take_id();
        self.apply_fill(order_id, side, bar.close, quantity, bar.open_unix_nanos)?;
        self.drop_position_protections();
        Ok(())
    }

    fn fill_resting(&mut self, bar: &ReplayBar) -> Result<(), BarReplayTradingError> {
        // Protections armed before this bar are read first, so the protections an entry arms
        // in this bar wait for the next one: one bar never decides both an entry and its exit.
        let mut armed: Vec<ReplayOrder> = self
            .orders
            .iter()
            .filter(|order| order.role != ReplayOrderRole::Entry && order.entry_id.is_none())
            .copied()
            .collect();
        // A bar that reached both a stop and a target is read as stopped first.
        armed.sort_by_key(|order| order.role != ReplayOrderRole::StopLoss);
        let entries: Vec<ReplayOrder> = self
            .orders
            .iter()
            .filter(|order| order.role == ReplayOrderRole::Entry)
            .copied()
            .collect();
        for order in entries.into_iter().chain(armed) {
            if self.order_index(order.id).is_err() {
                continue;
            }
            if let Some(touch) = bar_touch(
                order.side,
                resting_kind(order.order_type),
                order.price,
                bar.open,
                bar.high,
                bar.low,
            ) {
                self.execute(order.id, touch, bar.open_unix_nanos)?;
            }
        }
        Ok(())
    }

    fn execute(
        &mut self,
        order_id: u64,
        touch: FixedPoint,
        executed_unix_nanos: i64,
    ) -> Result<(), BarReplayTradingError> {
        let order = self.orders.remove(self.order_index(order_id)?);
        let price = TouchPrice.price(touch);
        let mut quantity = TouchLiquidity.executable_quantity(order.quantity);
        if order.role != ReplayOrderRole::Entry {
            // A protection only reduces the position it guards.
            let open = self
                .position
                .as_ref()
                .map_or(0, |position| position.net_quantity.units().abs());
            let reduces = self.position.as_ref().is_some_and(|position| {
                position.net_quantity.units().signum() == -order.side.sign()
            });
            if !reduces {
                return Ok(());
            }
            quantity = FixedPoint::try_new(quantity.units().min(open), quantity.scale())
                .map_err(accounting)?;
        }
        self.apply_fill(order.id, order.side, price, quantity, executed_unix_nanos)?;
        if order.role == ReplayOrderRole::Entry {
            for child in &mut self.orders {
                if child.entry_id == Some(order.id) {
                    child.entry_id = None;
                }
            }
        }
        if self.position.is_none() {
            self.drop_position_protections();
        }
        Ok(())
    }

    fn apply_fill(
        &mut self,
        order_id: u64,
        side: OrderSide,
        price: FixedPoint,
        quantity: FixedPoint,
        executed_unix_nanos: i64,
    ) -> Result<(), BarReplayTradingError> {
        let id = self.take_id();
        let fill = Fill {
            id: FillId::try_new(format!("replay-fill-{id}")).map_err(accounting)?,
            order_id: OrderId::try_new(format!("replay-order-{order_id}")).map_err(accounting)?,
            account_id: self.account.id.clone(),
            instrument_id: self.instrument.instrument_id.clone(),
            side,
            price,
            quantity,
            execution_unix_nanos: executed_unix_nanos,
            provenance: TradingProvenance {
                venue_id: REPLAY_VENUE_ID.to_string(),
                provider_id: REPLAY_VENUE_ID.to_string(),
                session_generation: 1,
                source_sequence: id,
                observed_unix_nanos: executed_unix_nanos,
            },
        };
        let realized_before = self
            .position
            .as_ref()
            .map_or(self.zero(), |position| position.realized_pnl);
        let (position, basis) = next_position(
            self.position.as_ref(),
            self.basis,
            &fill,
            &self.account,
            &self.instrument,
        )
        .map_err(BarReplayTradingError::Accounting)?;
        let realized_pnl = difference(position.realized_pnl, realized_before);
        self.balance = self.balance.checked_add(realized_pnl).map_err(accounting)?;
        let open = position.net_quantity.units() != 0;
        self.position = open.then_some(position);
        self.basis = basis.filter(|_| open);
        if self.fills.len() == MAXIMUM_REPLAY_FILLS {
            self.fills.pop_front();
        }
        self.fills.push_back(ReplayFill {
            id,
            order_id,
            side,
            price,
            quantity,
            executed_unix_nanos,
            realized_pnl,
        });
        Ok(())
    }

    fn drop_position_protections(&mut self) {
        self.orders
            .retain(|order| order.role == ReplayOrderRole::Entry || order.entry_id.is_some());
    }

    fn order_index(&self, order_id: u64) -> Result<usize, BarReplayTradingError> {
        self.orders
            .iter()
            .position(|order| order.id == order_id)
            .ok_or(BarReplayTradingError::UnknownOrder)
    }

    fn checked_price(&self, price: FixedPoint) -> Result<FixedPoint, BarReplayTradingError> {
        (price.scale() == self.instrument.price_scale && price.units() > 0)
            .then_some(price)
            .ok_or(BarReplayTradingError::InvalidPrice)
    }

    fn checked_quantity(&self, quantity: FixedPoint) -> Result<FixedPoint, BarReplayTradingError> {
        quantity
            .exact_rescale(self.instrument.quantity_scale)
            .ok()
            .filter(|quantity| quantity.units() > 0)
            .ok_or(BarReplayTradingError::InvalidQuantity)
    }

    /// Zero at the session's currency scale.
    fn zero(&self) -> FixedPoint {
        difference(self.starting_balance, self.starting_balance)
    }

    const fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

const fn protection(
    id: u64,
    side: OrderSide,
    role: ReplayOrderRole,
    price: FixedPoint,
    quantity: FixedPoint,
    entry_id: Option<u64>,
) -> ReplayOrder {
    let order_type = match role {
        ReplayOrderRole::TakeProfit => ReplayOrderType::Limit,
        ReplayOrderRole::Entry | ReplayOrderRole::StopLoss => ReplayOrderType::Stop,
    };
    ReplayOrder {
        id,
        side,
        order_type,
        role,
        price,
        quantity,
        entry_id,
    }
}

/// Whether a protection of a position entered on `side` near `reference` is on the side of
/// it where it does not fill at once.
const fn protects(
    side: OrderSide,
    role: ReplayOrderRole,
    price: FixedPoint,
    reference: FixedPoint,
) -> bool {
    let below = price.units() < reference.units();
    let above = price.units() > reference.units();
    match (side, role) {
        (OrderSide::Buy, ReplayOrderRole::StopLoss)
        | (OrderSide::Sell, ReplayOrderRole::TakeProfit) => below,
        (OrderSide::Buy, ReplayOrderRole::TakeProfit)
        | (OrderSide::Sell, ReplayOrderRole::StopLoss) => above,
        (_, ReplayOrderRole::Entry) => false,
    }
}

/// How an entry at `price` rests against the newest close; `None` fills at once.
const fn resting_type(
    side: OrderSide,
    price: FixedPoint,
    close: FixedPoint,
) -> Option<ReplayOrderType> {
    let (price, close) = (price.units(), close.units());
    if price == close {
        return None;
    }
    let favorable = match side {
        OrderSide::Buy => price < close,
        OrderSide::Sell => price > close,
    };
    Some(if favorable {
        ReplayOrderType::Limit
    } else {
        ReplayOrderType::Stop
    })
}

const fn marketable(
    side: OrderSide,
    order_type: ReplayOrderType,
    price: FixedPoint,
    close: FixedPoint,
) -> bool {
    let (price, close) = (price.units(), close.units());
    match (side, order_type) {
        (OrderSide::Buy, ReplayOrderType::Limit) | (OrderSide::Sell, ReplayOrderType::Stop) => {
            price >= close
        }
        (OrderSide::Sell, ReplayOrderType::Limit) | (OrderSide::Buy, ReplayOrderType::Stop) => {
            price <= close
        }
    }
}

const fn resting_kind(order_type: ReplayOrderType) -> RestingKind {
    match order_type {
        ReplayOrderType::Limit => RestingKind::Limit,
        ReplayOrderType::Stop => RestingKind::Stop,
    }
}

/// The part of a forming bar added since it was last seen: from the earlier close to the new
/// one, through any new extreme. A resting order inside that move fills on it.
fn formed_since(earlier: &ReplayBar, now: &ReplayBar) -> ReplayBar {
    let from = earlier.close.units();
    let to = now.close.units();
    let high = if now.high.units() > earlier.high.units() {
        now.high
    } else if to >= from {
        now.close
    } else {
        earlier.close
    };
    let low = if now.low.units() < earlier.low.units() {
        now.low
    } else if to <= from {
        now.close
    } else {
        earlier.close
    };
    ReplayBar {
        open: earlier.close,
        high,
        low,
        ..*now
    }
}

const fn opposite(side: OrderSide) -> OrderSide {
    match side {
        OrderSide::Buy => OrderSide::Sell,
        OrderSide::Sell => OrderSide::Buy,
    }
}

/// `left - right` at their shared currency scale; both come from the session's own math.
fn difference(left: FixedPoint, right: FixedPoint) -> FixedPoint {
    FixedPoint::try_new(left.units().saturating_sub(right.units()), left.scale()).unwrap_or(left)
}

fn accounting(error: impl fmt::Display) -> BarReplayTradingError {
    BarReplayTradingError::Accounting(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_instruments::{
        ContractMetadata, InstrumentClass, InstrumentDecimal, InstrumentId,
        InstrumentMetadataProvenance, PriceGrid,
    };

    fn price(units: i64) -> FixedPoint {
        FixedPoint::try_new(units, 2).expect("price")
    }

    fn quantity(units: i64) -> FixedPoint {
        FixedPoint::try_new(units, 0).expect("quantity")
    }

    fn money(units: i64) -> FixedPoint {
        FixedPoint::try_new(units, 2).expect("money")
    }

    fn session() -> BarReplayTrading {
        let instrument = TradingInstrument {
            instrument_id: InstrumentId::try_new("instrument:test:ES").expect("instrument"),
            price_scale: 2,
            quantity_scale: 0,
            contract: ContractMetadata {
                tick_size: Some(InstrumentDecimal::try_new(25, 2).expect("tick")),
                price_grid: PriceGrid::Fixed(InstrumentDecimal::try_new(25, 2).expect("grid")),
                instrument_class: InstrumentClass::Future,
                point_value: Some(InstrumentDecimal::try_new(50, 0).expect("point value")),
                order_quantity_increment: None,
                currency: "USD".to_string(),
                expiry: None,
                first_notice: None,
                last_trade: None,
                session_hours: Vec::new(),
                provenance: InstrumentMetadataProvenance {
                    provider_id: "test".to_string(),
                    provider_symbol: "ES".to_string(),
                    display_symbol: "ES".to_string(),
                    session_generation: 1,
                },
            },
        };
        BarReplayTrading::new(instrument, money(10_000_000)).expect("session")
    }

    const MINUTE: i64 = 60_000_000_000;

    fn bar(minute: i64, open: i64, high: i64, low: i64, close: i64) -> ReplayBar {
        let open_unix_nanos = 1_700_000_000_000_000_000 + minute * MINUTE;
        ReplayBar {
            open_unix_nanos,
            end_unix_nanos: open_unix_nanos + MINUTE,
            open: price(open),
            high: price(high),
            low: price(low),
            close: price(close),
        }
    }

    fn request(side: OrderSide, entry: ReplayEntry) -> ReplayOrderRequest {
        ReplayOrderRequest {
            side,
            entry,
            quantity: quantity(1),
            stop_loss: None,
            take_profit: None,
        }
    }

    #[test]
    fn nothing_trades_before_a_bar_is_revealed() {
        let mut replay = session();
        assert_eq!(
            replay.place(request(OrderSide::Buy, ReplayEntry::Market)),
            Err(BarReplayTradingError::NoBar)
        );
    }

    #[test]
    fn a_market_order_fills_at_the_newest_close_and_marks_at_later_closes() {
        let mut replay = session();
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        replay
            .place(request(OrderSide::Buy, ReplayEntry::Market))
            .expect("market buy");
        let position = replay.position().expect("long");
        assert_eq!(position.net_quantity, quantity(1));
        assert_eq!(position.average_entry_price, price(500_000));
        replay
            .advance([bar(1, 500_000, 501_500, 499_750, 501_000)])
            .expect("bar");
        // Ten points on a 50-dollar point value.
        assert_eq!(replay.unrealized_pnl(), money(50_000));
        replay.close_position().expect("close");
        assert!(replay.position().is_none());
        assert_eq!(replay.realized_pnl(), money(50_000));
        assert_eq!(replay.balance(), money(10_050_000));
        assert_eq!(replay.fills().count(), 2);
    }

    #[test]
    fn resting_entries_fill_at_their_price_or_at_a_gapped_open() {
        let mut replay = session();
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        let limit = replay
            .place(request(OrderSide::Buy, ReplayEntry::At(price(499_000))))
            .expect("buy limit");
        let stop = replay
            .place(request(OrderSide::Sell, ReplayEntry::At(price(498_000))))
            .expect("sell stop");
        assert_eq!(replay.orders()[0].order_type, ReplayOrderType::Limit);
        assert_eq!(replay.orders()[1].order_type, ReplayOrderType::Stop);
        replay
            .advance([bar(1, 499_800, 500_000, 498_900, 499_500)])
            .expect("bar");
        assert!(replay.orders().iter().all(|order| order.id != limit));
        assert_eq!(
            replay.fills().last().map(|fill| fill.price),
            Some(price(499_000))
        );
        replay
            .advance([bar(2, 497_000, 497_500, 496_500, 497_000)])
            .expect("gap down");
        assert!(replay.orders().iter().all(|order| order.id != stop));
        assert_eq!(
            replay.fills().last().map(|fill| fill.price),
            Some(price(497_000))
        );
        assert!(replay.position().is_none(), "the stop closed the long");
    }

    #[test]
    fn a_bracket_arms_after_its_entry_and_one_exit_cancels_the_other() {
        let mut replay = session();
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        replay
            .place(ReplayOrderRequest {
                stop_loss: Some(price(498_000)),
                take_profit: Some(price(503_000)),
                ..request(OrderSide::Buy, ReplayEntry::At(price(499_000)))
            })
            .expect("bracket");
        // The entry fills in a bar that also reaches the stop; the stop waits for the next bar.
        replay
            .advance([bar(1, 499_500, 499_500, 497_500, 498_500)])
            .expect("bar");
        assert!(replay.position().is_some());
        assert!(replay.orders().iter().all(|order| order.entry_id.is_none()));
        assert_eq!(replay.orders().len(), 2);
        replay
            .advance([bar(2, 498_500, 503_500, 497_000, 503_000)])
            .expect("bar");
        assert!(replay.position().is_none());
        assert_eq!(replay.orders(), [], "the target was cancelled");
        assert_eq!(
            replay.fills().last().map(|fill| fill.price),
            Some(price(498_000)),
            "a bar reaching both exits is read as stopped"
        );
        assert_eq!(replay.realized_pnl(), money(-50_000));
    }

    #[test]
    fn protections_must_sit_where_they_do_not_fill_at_once() {
        let mut replay = session();
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        assert_eq!(
            replay.place(ReplayOrderRequest {
                stop_loss: Some(price(501_000)),
                ..request(OrderSide::Buy, ReplayEntry::Market)
            }),
            Err(BarReplayTradingError::ProtectionOnWrongSide)
        );
        assert_eq!(
            replay.protect(ReplayOrderRole::StopLoss, price(499_000)),
            Err(BarReplayTradingError::NoPosition)
        );
        replay
            .place(request(OrderSide::Sell, ReplayEntry::Market))
            .expect("short");
        let stop = replay
            .protect(ReplayOrderRole::StopLoss, price(501_000))
            .expect("stop above a short");
        let replacement = replay
            .protect(ReplayOrderRole::StopLoss, price(502_000))
            .expect("replaced stop");
        assert_ne!(stop, replacement);
        assert_eq!(replay.orders().len(), 1);
    }

    #[test]
    fn moving_an_order_through_the_market_fills_it_and_cancel_takes_its_legs() {
        let mut replay = session();
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        let entry = replay
            .place(ReplayOrderRequest {
                take_profit: Some(price(502_000)),
                ..request(OrderSide::Buy, ReplayEntry::At(price(499_000)))
            })
            .expect("bracket");
        replay.cancel(entry).expect("cancel");
        assert_eq!(replay.orders(), []);
        let limit = replay
            .place(request(OrderSide::Buy, ReplayEntry::At(price(499_000))))
            .expect("limit");
        replay.modify(limit, price(500_500)).expect("move up");
        assert_eq!(
            replay
                .position()
                .map(|position| position.average_entry_price),
            Some(price(500_000))
        );
        assert_eq!(
            replay.cancel(limit),
            Err(BarReplayTradingError::UnknownOrder)
        );
    }

    #[test]
    fn stepping_back_never_trades_a_bar_twice() {
        let mut replay = session();
        replay
            .advance([bar(1, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        replay
            .place(request(OrderSide::Buy, ReplayEntry::At(price(499_000))))
            .expect("limit");
        replay
            .advance([bar(0, 499_000, 499_000, 498_000, 498_500)])
            .expect("older bar");
        assert_eq!(replay.orders().len(), 1, "an older bar is ignored");
    }

    #[test]
    fn a_forming_bar_trades_only_the_move_it_added() {
        let mut replay = session();
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        replay
            .place(request(OrderSide::Buy, ReplayEntry::At(price(499_800))))
            .expect("limit inside the bar's range");
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("the same bar again");
        assert_eq!(replay.orders().len(), 1, "an unchanged bar trades nothing");
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 499_700)])
            .expect("the bar formed lower");
        assert_eq!(
            replay.fills().last().map(|fill| fill.price),
            Some(price(499_800))
        );
    }

    #[test]
    fn bars_of_another_interval_inside_traded_time_are_skipped() {
        let mut replay = session();
        let mut fifteen = bar(0, 500_000, 500_500, 499_500, 500_000);
        fifteen.end_unix_nanos = fifteen.open_unix_nanos + 15 * MINUTE;
        replay.advance([fifteen]).expect("fifteen-minute bar");
        replay
            .place(request(OrderSide::Buy, ReplayEntry::At(price(499_000))))
            .expect("limit");
        // Five-minute bars inside the traded fifteen minutes reach the limit but are skipped.
        replay
            .advance([
                bar(0, 500_000, 500_000, 498_000, 499_000),
                bar(5, 499_000, 499_500, 498_500, 499_000),
                bar(10, 499_000, 499_500, 498_500, 499_000),
            ])
            .expect("finer bars");
        assert_eq!(replay.orders().len(), 1);
        assert!(!replay.traded_beyond(fifteen.open_unix_nanos));
        replay
            .advance([bar(15, 499_500, 499_500, 498_900, 499_200)])
            .expect("the next bar");
        assert_eq!(replay.orders(), []);
        assert!(replay.traded_beyond(fifteen.open_unix_nanos));
        assert!(replay.has_activity());
    }

    #[test]
    fn working_orders_are_bounded() {
        let mut replay = session();
        replay
            .advance([bar(0, 500_000, 500_500, 499_500, 500_000)])
            .expect("bar");
        for _ in 0..MAXIMUM_REPLAY_WORKING_ORDERS {
            replay
                .place(request(OrderSide::Buy, ReplayEntry::At(price(490_000))))
                .expect("within the bound");
        }
        assert_eq!(
            replay.place(request(OrderSide::Buy, ReplayEntry::At(price(490_000)))),
            Err(BarReplayTradingError::WorkingOrderLimit)
        );
    }
}
