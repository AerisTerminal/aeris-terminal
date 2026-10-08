//! cTrader trading relay (D8). The cTrader supervisor keeps the one session per host and
//! carries the trading owner's venue contract over the demo session. This module
//! translates between `aeris_trading::venue` and the adapter's trading messages; it holds
//! no order, fill, position or account state. Its only memory is which demo accounts it
//! serves, their symbol price scales, and when the session last dropped, so a reconnect
//! can replay what the owner missed.

use super::{
    BTreeMap, BTreeSet, Duration, Failure, HostFault, Instant, Mutex, Receiver, Route, Worker,
    classify, now_nanos, parse_instrument_id, request_error,
};
use aeris_ctrader_open_api_adapter::{
    ProtoMessage,
    market::{MarketDecodeError, MarketRequest, PriceScale, SymbolSpec, decode_symbol_by_id},
    trading::{
        self as wire, NewOrder, OrderAmendment, OrderPrice, TradingRequest, decode_deal_page,
        decode_execution_event, decode_reconcile, decode_trader, event_account, referenced_symbols,
    },
};
use aeris_instruments::InstrumentId;
use aeris_observability::diagnostic;
use aeris_trading::{
    AccountEnvironment, FixedPoint, OrderSide, OrderType, TimeInForce,
    venue::{
        BrokerFill, BrokerOrder, BrokerOrderKind, BrokerOrderState, ObservedAccount, Protection,
        RealizedClose, VenueAmendment, VenueEvent, VenueOrder, VenuePosition, VenueRequest,
        VenueSnapshot, VenueUpdate,
    },
};

/// Venue requests handled per worker turn, so market events keep flowing.
const REQUESTS_PER_TURN: usize = 16;
/// Retry bounds while the demo session cannot be opened (for example, not connected).
const FIRST_OPEN_RETRY: Duration = Duration::from_secs(5);
const MAXIMUM_OPEN_RETRY: Duration = Duration::from_secs(300);
/// Symbol specifications kept for trading; the cache restarts when it fills.
const MAXIMUM_SPECS: usize = 1024;
/// Deals are replayed from a little before the session dropped.
const RESYNC_MARGIN_MILLIS: i64 = 60_000;
/// Volumes travel in cents of a unit.
const VOLUME_SCALE: u8 = 2;
const EXECUTION_EVENT: u32 = 2126;
const ORDER_ERROR_EVENT: u32 = 2132;
const TRADER_UPDATED_EVENT: u32 = 2123;
const TRAILING_STOP_EVENT: u32 = 2107;

/// Delivers one venue event to the trading owner's bounded inbox.
pub type VenueEventSink = Box<dyn Fn(VenueEvent) -> Result<(), String> + Send>;

/// The trading owner's side of one venue generation: its bounded request queue and the
/// sink for events stamped with that generation.
pub struct VenueLink {
    pub generation: u64,
    pub requests: Receiver<VenueRequest>,
    pub events: VenueEventSink,
}

/// The latest attachment handed from the market service to the cTrader worker. A newer
/// one replaces an older one the worker has not taken yet.
#[derive(Default)]
pub struct VenueSlot(Mutex<Option<VenueLink>>);

impl VenueSlot {
    pub(in crate::market_service) fn put(&self, link: VenueLink) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(link);
    }

    fn take(&self) -> Option<VenueLink> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// The relay's state for one attached venue generation.
pub(super) struct VenueRelay {
    link: VenueLink,
    /// Demo accounts announced to the owner; their events are relayed.
    accounts: BTreeSet<u64>,
    announced: bool,
    specs: BTreeMap<(u64, u64), SymbolSpec>,
    /// When the demo session dropped while serving; the next turn reconciles from then.
    resync_since_millis: Option<i64>,
    /// While the demo session cannot be opened, requests are refused until this time.
    unavailable: Option<(Instant, Duration, String)>,
}

impl VenueRelay {
    pub(super) fn new(link: VenueLink) -> Self {
        Self {
            link,
            accounts: BTreeSet::new(),
            announced: false,
            specs: BTreeMap::new(),
            resync_since_millis: None,
            unavailable: None,
        }
    }
}

fn broker_account(ctid: u64) -> String {
    ctid.to_string()
}

fn account_ctid(broker_account: &str) -> Result<u64, String> {
    broker_account
        .parse::<u64>()
        .ok()
        .filter(|ctid| *ctid > 0)
        .ok_or_else(|| "cTrader broker account is invalid".into())
}

fn broker_id(value: &str, field: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| format!("cTrader {field} is invalid"))
}

/// The demo symbol an instrument names, which must belong to the request's account.
fn demo_symbol(instrument: &InstrumentId, ctid: u64) -> Result<u64, String> {
    let route = parse_instrument_id(instrument.as_str())?;
    if route.live {
        return Err("cTrader live accounts are data-only; trading is disabled".into());
    }
    if route.ctid != ctid {
        return Err("instrument belongs to another cTrader account".into());
    }
    Ok(route.symbol_id)
}

fn instrument_id(ctid: u64, symbol_id: u64) -> Result<InstrumentId, String> {
    InstrumentId::try_new(
        Route {
            live: false,
            ctid,
            symbol_id,
        }
        .instrument_id(),
    )
    .map_err(|error| error.to_string())
}

fn price_units(price: FixedPoint, scale: PriceScale) -> Result<i64, String> {
    price
        .exact_rescale(scale.digits())
        .map(FixedPoint::units)
        .map_err(|_| "price does not match the cTrader symbol digits".into())
}

fn volume(quantity: FixedPoint) -> Result<u64, String> {
    quantity
        .exact_rescale(VOLUME_SCALE)
        .ok()
        .and_then(|cents| u64::try_from(cents.units()).ok())
        .ok_or_else(|| "quantity is not a whole number of cTrader volume cents".into())
}

fn point(units: i64, scale: u8) -> Result<FixedPoint, String> {
    FixedPoint::try_new(units, scale).map_err(|error| error.to_string())
}

fn cents(volume: u64) -> Result<FixedPoint, String> {
    point(
        i64::try_from(volume).map_err(|_| "cTrader volume is out of range")?,
        VOLUME_SCALE,
    )
}

fn money(amount: wire::Money) -> Result<FixedPoint, String> {
    point(amount.units, amount.digits)
}

fn average(price: wire::AveragePrice) -> Result<FixedPoint, String> {
    point(price.units, price.digits)
}

fn millis_to_nanos(millis: i64) -> Result<i64, String> {
    millis
        .checked_mul(1_000_000)
        .ok_or_else(|| "cTrader timestamp is out of range".into())
}

const fn wire_side(side: OrderSide) -> wire::TradeSide {
    match side {
        OrderSide::Buy => wire::TradeSide::Buy,
        OrderSide::Sell => wire::TradeSide::Sell,
    }
}

const fn order_side(side: wire::TradeSide) -> OrderSide {
    match side {
        wire::TradeSide::Buy => OrderSide::Buy,
        wire::TradeSide::Sell => OrderSide::Sell,
    }
}

fn protection(
    level: Option<Protection>,
    scale: PriceScale,
) -> Result<Option<wire::Protection>, String> {
    level
        .map(|level| {
            Ok(match level {
                Protection::Price(price) => wire::Protection::Price(price_units(price, scale)?),
                Protection::Distance(distance) => {
                    wire::Protection::Distance(price_units(distance, scale)?)
                }
            })
        })
        .transpose()
}

/// A pending order's time in force. Market orders always fill or cancel at once, which
/// cTrader accepts as immediate-or-cancel (observed on demo).
fn pending_time_in_force(time_in_force: TimeInForce) -> Result<wire::TimeInForce, String> {
    match time_in_force {
        TimeInForce::GoodTillCancelled => Ok(wire::TimeInForce::GoodTillCancel),
        TimeInForce::ImmediateOrCancel => Ok(wire::TimeInForce::ImmediateOrCancel),
        TimeInForce::FillOrKill => Ok(wire::TimeInForce::FillOrKill),
        TimeInForce::Day => Err("cTrader has no day orders; use good-till-cancelled".into()),
    }
}

fn wire_error(error: &MarketDecodeError) -> String {
    error.to_string()
}

/// Encodes a new order for an observed demo account.
pub(super) fn place_request(
    account: &aeris_ctrader_open_api_adapter::accounts::DemoAccount,
    spec: &SymbolSpec,
    order: &VenueOrder,
) -> Result<TradingRequest, String> {
    let scale = spec.price_scale;
    let (order_type, time_in_force) = match order.order_type {
        OrderType::Market => (
            wire::OrderType::Market,
            wire::TimeInForce::ImmediateOrCancel,
        ),
        OrderType::Limit => (
            wire::OrderType::Limit {
                price: price_units(order.limit_price.ok_or("limit price is missing")?, scale)?,
            },
            pending_time_in_force(order.time_in_force)?,
        ),
        OrderType::Stop => (
            wire::OrderType::Stop {
                price: price_units(order.stop_price.ok_or("stop price is missing")?, scale)?,
            },
            pending_time_in_force(order.time_in_force)?,
        ),
        OrderType::StopLimit => return Err("cTrader stop-limit orders are not supported".into()),
    };
    let request = NewOrder {
        symbol_id: spec.symbol_id,
        side: wire_side(order.side),
        order_type,
        volume: volume(order.quantity)?,
        time_in_force,
        stop_loss: protection(order.stop_loss, scale)?,
        take_profit: protection(order.take_profit, scale)?,
        client_order_id: order.client_order_id.as_str().to_string(),
        position_id: order
            .broker_position_id
            .as_deref()
            .map(|id| broker_id(id, "position id"))
            .transpose()?,
    };
    TradingRequest::new_order(account, scale, &request).map_err(|error| wire_error(&error))
}

/// Encodes a pending-order change.
pub(super) fn amend_request(
    account: &aeris_ctrader_open_api_adapter::accounts::DemoAccount,
    spec: &SymbolSpec,
    amendment: &VenueAmendment,
) -> Result<TradingRequest, String> {
    let scale = spec.price_scale;
    let price = match (amendment.limit_price, amendment.stop_price) {
        (None, None) => None,
        (Some(limit), None) => Some(OrderPrice::Limit(price_units(limit, scale)?)),
        (None, Some(stop)) => Some(OrderPrice::Stop(price_units(stop, scale)?)),
        (Some(_), Some(_)) => return Err("cTrader stop-limit orders are not supported".into()),
    };
    let request = OrderAmendment {
        order_id: broker_id(&amendment.broker_order_id, "order id")?,
        volume: amendment.quantity.map(volume).transpose()?,
        price,
        stop_loss: protection(amendment.stop_loss, scale)?,
        take_profit: protection(amendment.take_profit, scale)?,
    };
    TradingRequest::amend_order(account, scale, &request).map_err(|error| wire_error(&error))
}

/// What an order report means for the owner's order.
const fn order_state(
    execution: wire::ExecutionType,
    status: wire::OrderStatus,
) -> Option<BrokerOrderState> {
    Some(match execution {
        wire::ExecutionType::Accepted => BrokerOrderState::Accepted,
        wire::ExecutionType::Replaced => BrokerOrderState::Replaced,
        wire::ExecutionType::Cancelled => BrokerOrderState::Cancelled,
        wire::ExecutionType::Expired => BrokerOrderState::Expired,
        wire::ExecutionType::Rejected => BrokerOrderState::Rejected,
        wire::ExecutionType::CancelRejected => BrokerOrderState::CancelRejected,
        wire::ExecutionType::Filled if matches!(status, wire::OrderStatus::Filled) => {
            BrokerOrderState::Filled
        }
        wire::ExecutionType::Filled | wire::ExecutionType::PartialFill => {
            BrokerOrderState::PartiallyFilled
        }
        wire::ExecutionType::Swap
        | wire::ExecutionType::DepositWithdraw
        | wire::ExecutionType::BonusDepositWithdraw => return None,
    })
}

/// Translations of decoded trading messages for one demo account.
pub(super) struct Translator<'a> {
    pub(super) ctid: u64,
    pub(super) specs: &'a BTreeMap<(u64, u64), SymbolSpec>,
}

impl Translator<'_> {
    fn digits(&self, symbol_id: u64) -> Result<u8, String> {
        self.specs
            .get(&(self.ctid, symbol_id))
            .map(|spec| spec.price_scale.digits())
            .ok_or_else(|| "cTrader symbol specification is not loaded".into())
    }

    pub(super) fn scales(&self) -> impl Fn(u64) -> Option<PriceScale> + '_ {
        |symbol_id| {
            self.specs
                .get(&(self.ctid, symbol_id))
                .map(|spec| spec.price_scale)
        }
    }

    fn order(&self, order: &wire::OrderState) -> Result<BrokerOrder, String> {
        let digits = self.digits(order.symbol_id)?;
        let price = |units: Option<i64>| units.map(|units| point(units, digits)).transpose();
        Ok(BrokerOrder {
            broker_order_id: order.order_id.to_string(),
            client_order_id: order.client_order_id.clone(),
            instrument_id: instrument_id(self.ctid, order.symbol_id)?,
            side: order_side(order.side),
            kind: match order.kind {
                wire::OrderKind::Market => BrokerOrderKind::Market,
                wire::OrderKind::Limit => BrokerOrderKind::Limit,
                wire::OrderKind::Stop => BrokerOrderKind::Stop,
                wire::OrderKind::StopLimit => BrokerOrderKind::StopLimit,
                wire::OrderKind::StopLossTakeProfit => BrokerOrderKind::Protection,
                wire::OrderKind::MarketRange => BrokerOrderKind::Other,
            },
            quantity: cents(order.volume)?,
            filled_quantity: cents(order.executed_volume.unwrap_or(0))?,
            limit_price: price(order.limit_price)?,
            stop_price: price(order.stop_price)?,
            broker_position_id: order.position_id.map(|id| id.to_string()),
            closing: order.closing,
        })
    }

    fn position(&self, position: &wire::PositionState) -> Result<VenuePosition, String> {
        let digits = self.digits(position.symbol_id)?;
        let price = |units: Option<i64>| units.map(|units| point(units, digits)).transpose();
        let opened = position
            .opened_unix_ms
            .or(position.updated_unix_ms)
            .ok_or("cTrader position has no time")?;
        Ok(VenuePosition {
            broker_position_id: position.position_id.to_string(),
            instrument_id: instrument_id(self.ctid, position.symbol_id)?,
            side: order_side(position.side),
            quantity: cents(position.volume)?,
            entry_price: position.price.map(average).transpose()?,
            stop_loss: price(position.stop_loss)?,
            take_profit: price(position.take_profit)?,
            swap: money(position.swap)?,
            commission: position
                .commission
                .map_or_else(|| point(0, position.swap.digits), money)?,
            opened_unix_nanos: millis_to_nanos(opened)?,
        })
    }

    /// A filled deal as an owner fill; unfilled or rejected deals add nothing.
    fn fill(&self, deal: &wire::Deal) -> Result<Option<BrokerFill>, String> {
        if !matches!(
            deal.status,
            wire::DealStatus::Filled | wire::DealStatus::PartiallyFilled
        ) || deal.filled_volume == 0
        {
            return Ok(None);
        }
        let digits = self.digits(deal.symbol_id)?;
        let price = deal
            .execution_price
            .ok_or("cTrader filled deal has no execution price")?;
        Ok(Some(BrokerFill {
            broker_deal_id: deal.deal_id.to_string(),
            broker_order_id: deal.order_id.to_string(),
            broker_position_id: deal.position_id.to_string(),
            instrument_id: instrument_id(self.ctid, deal.symbol_id)?,
            side: order_side(deal.side),
            price: point(price, digits)?,
            quantity: cents(deal.filled_volume)?,
            executed_unix_nanos: millis_to_nanos(deal.executed_unix_ms)?,
            commission: deal.commission.map(money).transpose()?,
            realized: deal
                .closed
                .map(|closed| {
                    Ok::<_, String>(RealizedClose {
                        gross_profit: money(closed.gross_profit)?,
                        swap: money(closed.swap)?,
                        commission: money(closed.commission)?,
                        balance: money(closed.balance)?,
                    })
                })
                .transpose()?,
        }))
    }

    /// Every owner update one execution event carries, in apply order: the order, its
    /// fill, the position, then the balance a closing fill leaves.
    pub(super) fn execution(
        &self,
        event: &wire::ExecutionEvent,
    ) -> Result<Vec<VenueUpdate>, String> {
        let mut updates = Vec::new();
        if let Some(order) = &event.order
            && let Some(state) = order_state(event.execution, order.status)
        {
            updates.push(VenueUpdate::Order {
                order: self.order(order)?,
                state,
                reason: event.error_code.clone(),
            });
        }
        if let Some(deal) = &event.deal {
            if let Some(fill) = self.fill(deal)? {
                updates.push(VenueUpdate::Fill(fill));
            }
            if let Some(closed) = deal.closed {
                updates.push(VenueUpdate::Balance {
                    balance: money(closed.balance)?,
                });
            }
        }
        if let Some(position) = &event.position {
            match position.status {
                wire::PositionStatus::Open => {
                    updates.push(VenueUpdate::Position(self.position(position)?));
                }
                wire::PositionStatus::Closed => updates.push(VenueUpdate::PositionClosed {
                    broker_position_id: position.position_id.to_string(),
                }),
                wire::PositionStatus::Created | wire::PositionStatus::Error => {}
            }
        }
        Ok(updates)
    }

    pub(super) fn snapshot(
        &self,
        reconciliation: &wire::Reconciliation,
    ) -> Result<VenueSnapshot, String> {
        Ok(VenueSnapshot {
            orders: reconciliation
                .orders
                .iter()
                .map(|order| self.order(order))
                .collect::<Result<_, _>>()?,
            positions: reconciliation
                .positions
                .iter()
                .filter(|position| position.status == wire::PositionStatus::Open)
                .map(|position| self.position(position))
                .collect::<Result<_, _>>()?,
        })
    }

    pub(super) fn fills(&self, deals: &[wire::Deal]) -> Result<Vec<VenueUpdate>, String> {
        deals
            .iter()
            .filter_map(|deal| self.fill(deal).transpose())
            .map(|fill| fill.map(VenueUpdate::Fill))
            .collect()
    }
}

/// The request a refused venue request is reported against, if the owner tracks one.
fn refused(request: &VenueRequest, reason: String) -> Option<VenueUpdate> {
    let client_order_id = match request {
        VenueRequest::Place(order) => order.client_order_id.clone(),
        VenueRequest::Amend(amendment) => amendment.client_order_id.clone(),
        VenueRequest::Cancel {
            client_order_id, ..
        } => client_order_id.clone(),
        VenueRequest::ClosePosition { .. }
        | VenueRequest::AmendPositionProtection { .. }
        | VenueRequest::Reconcile { .. } => return None,
    };
    Some(VenueUpdate::Refused {
        client_order_id,
        reason,
    })
}

impl Worker {
    /// Takes a newly attached venue generation; the previous one is retired with it.
    pub(super) fn take_venue(&mut self) {
        if let Some(link) = self.ports.venue.take() {
            diagnostic!(
                "Aeris cTrader trading venue attached (generation {})",
                link.generation
            );
            self.venue = Some(VenueRelay::new(link));
        }
    }

    /// A demo session drop while serving makes the next turn replay from then.
    pub(super) fn mark_venue_resync(&mut self) {
        if let Some(relay) = self.venue.as_mut()
            && relay.announced
            && relay.resync_since_millis.is_none()
        {
            relay.resync_since_millis = now_nanos()
                .ok()
                .map(|nanos| nanos / 1_000_000 - RESYNC_MARGIN_MILLIS);
        }
    }

    /// A new authorization makes an unavailable venue retry at once.
    pub(super) fn retry_venue_now(&mut self) {
        if let Some(relay) = self.venue.as_mut() {
            relay.unavailable = None;
        }
    }

    /// One relay turn. When the demo session cannot be opened (for example, cTrader is
    /// not connected), trading waits with its own backoff and refuses queued requests with
    /// the reason, so market data recovery is never disturbed by it.
    pub(super) fn venue_step(&mut self) -> Result<(), HostFault> {
        if self.venue.is_none() || Instant::now() < self.retry_at || self.epoch_state.paused {
            return Ok(());
        }
        if let Some((until, _, reason)) = self
            .venue
            .as_ref()
            .and_then(|relay| relay.unavailable.clone())
            && Instant::now() < until
        {
            self.refuse_queued(&reason);
            return Ok(());
        }
        let had_demo = self.hosts.contains_key(&false);
        match self.venue_turn() {
            Err(fault) if !had_demo && !self.hosts.contains_key(&false) => {
                let delay = self
                    .venue
                    .as_ref()
                    .and_then(|relay| relay.unavailable.as_ref())
                    .map_or(FIRST_OPEN_RETRY, |(_, delay, _)| {
                        delay.saturating_mul(2).min(MAXIMUM_OPEN_RETRY)
                    });
                if delay == FIRST_OPEN_RETRY {
                    diagnostic!(
                        "Aeris cTrader trading is waiting for a session: {}",
                        fault.detail
                    );
                }
                if let Some(relay) = self.venue.as_mut() {
                    relay.unavailable = Some((Instant::now() + delay, delay, fault.detail.clone()));
                }
                self.refuse_queued(&fault.detail);
                Ok(())
            }
            Err(fault) => Err(fault),
            Ok(()) => {
                if let Some(relay) = self.venue.as_mut() {
                    relay.unavailable = None;
                }
                Ok(())
            }
        }
    }

    fn refuse_queued(&mut self, reason: &str) {
        while let Some(request) = self
            .venue
            .as_ref()
            .and_then(|relay| relay.link.requests.try_recv().ok())
        {
            match account_ctid(request.broker_account()) {
                Ok(ctid) => self.report_failure(ctid, &request, reason),
                Err(error) => diagnostic!("Aeris cTrader venue request refused: {error}"),
            }
        }
    }

    /// Announce accounts once, replay after a reconnect, then handle a bounded number of
    /// the owner's requests.
    fn venue_turn(&mut self) -> Result<(), HostFault> {
        if self.venue.as_ref().is_some_and(|relay| !relay.announced) {
            self.announce_accounts()?;
        }
        if let Some(since) = self
            .venue
            .as_mut()
            .and_then(|relay| relay.resync_since_millis.take())
        {
            let accounts: Vec<u64> = self
                .venue
                .as_ref()
                .map(|relay| relay.accounts.iter().copied().collect())
                .unwrap_or_default();
            for ctid in accounts {
                self.reconcile_account(ctid, Some(since))?;
            }
        }
        for _ in 0..REQUESTS_PER_TURN {
            let Some(request) = self
                .venue
                .as_ref()
                .and_then(|relay| relay.link.requests.try_recv().ok())
            else {
                break;
            };
            self.handle_venue_request(&request)?;
        }
        Ok(())
    }

    fn emit(&mut self, ctid: u64, update: VenueUpdate) {
        let Some(relay) = self.venue.as_ref() else {
            return;
        };
        let event = now_nanos().map(|observed_unix_nanos| VenueEvent {
            session_generation: relay.link.generation,
            broker_account: broker_account(ctid),
            update,
            observed_unix_nanos,
        });
        if let Err(error) = event.and_then(|event| (relay.link.events)(event)) {
            // The owner is stopping or stalled; it reattaches with a new generation.
            diagnostic!("Aeris cTrader trading venue detached: {error}");
            self.venue = None;
        }
    }

    /// Tells the owner which demo accounts this connection can trade, with their deposit
    /// currency and balance.
    fn announce_accounts(&mut self) -> Result<(), HostFault> {
        let accounts: Vec<_> = self
            .ensure_host(false)
            .map_err(HostFault::from)?
            .link
            .accounts()
            .iter()
            .filter(|account| !account.is_live)
            .cloned()
            .collect();
        for account in accounts {
            match self.observe_account(account.ctid, &account) {
                Ok(()) => {}
                Err(Failure::Host(fault)) => return Err(fault),
                Err(Failure::Request(error)) => {
                    diagnostic!("Aeris cTrader demo account could not be announced: {error}");
                }
            }
        }
        if let Some(relay) = self.venue.as_mut() {
            relay.announced = true;
        }
        Ok(())
    }

    fn observe_account(
        &mut self,
        ctid: u64,
        account: &aeris_ctrader_open_api_adapter::accounts::CtraderAccount,
    ) -> Result<(), Failure> {
        let host = self.ensure_host(false)?;
        host.authorize(ctid)?;
        let frame = host.request(trading(
            TradingRequest::trader(ctid).map_err(|error| request_error(&error))?,
        ))?;
        let trader = decode_trader(&frame, ctid).map_err(|error| request_error(&error))?;
        let currency = self.asset_name(
            Route {
                live: false,
                ctid,
                symbol_id: 0,
            },
            trader.deposit_asset_id,
        )?;
        let broker = account.broker_title.as_deref().unwrap_or("cTrader");
        let login = account
            .trader_login
            .map_or_else(|| ctid.to_string(), |login| login.to_string());
        if let Some(relay) = self.venue.as_mut() {
            relay.accounts.insert(ctid);
        }
        self.emit(
            ctid,
            VenueUpdate::AccountObserved(ObservedAccount {
                environment: AccountEnvironment::Demo,
                display_name: format!("cTrader Demo · {broker} {login}"),
                currency,
                currency_scale: trader.balance.digits,
            }),
        );
        let balance = money(trader.balance).map_err(Failure::Request)?;
        self.emit(ctid, VenueUpdate::Balance { balance });
        Ok(())
    }

    fn handle_venue_request(&mut self, request: &VenueRequest) -> Result<(), HostFault> {
        let ctid = match account_ctid(request.broker_account()) {
            Ok(ctid) => ctid,
            Err(error) => {
                diagnostic!("Aeris cTrader venue request refused: {error}");
                return Ok(());
            }
        };
        if let VenueRequest::Reconcile {
            deals_since_unix_nanos,
            ..
        } = request
        {
            return self
                .reconcile_account(ctid, deals_since_unix_nanos.map(|nanos| nanos / 1_000_000));
        }
        match self.send_trading(ctid, request) {
            Ok(frame) => {
                self.relay_frame(ctid, &frame, Some(request));
                Ok(())
            }
            Err(Failure::Request(reason)) => {
                self.report_failure(ctid, request, &reason);
                Ok(())
            }
            Err(Failure::Host(fault)) => {
                self.report_failure(ctid, request, &fault.detail);
                Err(fault)
            }
        }
    }

    fn report_failure(&mut self, ctid: u64, request: &VenueRequest, reason: &str) {
        match refused(request, reason.to_string()) {
            Some(update) => self.emit(ctid, update),
            None => diagnostic!("Aeris cTrader venue request failed: {reason}"),
        }
    }

    fn send_trading(&mut self, ctid: u64, request: &VenueRequest) -> Result<ProtoMessage, Failure> {
        let host = self.ensure_host(false)?;
        host.authorize(ctid)?;
        if let Some(relay) = self.venue.as_mut() {
            relay.accounts.insert(ctid);
        }
        let account = self
            .hosts
            .get(&false)
            .ok_or_else(|| Failure::host("cTrader demo session is unavailable"))?
            .link
            .demo_account(ctid)
            .map_err(|fault| classify(&fault))?;
        let encoded = match request {
            VenueRequest::Place(order) => {
                let spec = self.venue_spec(
                    ctid,
                    demo_symbol(&order.instrument_id, ctid).map_err(Failure::Request)?,
                )?;
                place_request(&account, &spec, order)
            }
            VenueRequest::Amend(amendment) => {
                let spec = self.venue_spec(
                    ctid,
                    demo_symbol(&amendment.instrument_id, ctid).map_err(Failure::Request)?,
                )?;
                amend_request(&account, &spec, amendment)
            }
            VenueRequest::Cancel {
                broker_order_id, ..
            } => broker_id(broker_order_id, "order id").and_then(|order_id| {
                TradingRequest::cancel_order(&account, order_id).map_err(|error| wire_error(&error))
            }),
            VenueRequest::ClosePosition {
                broker_position_id,
                quantity,
                ..
            } => broker_id(broker_position_id, "position id").and_then(|position_id| {
                TradingRequest::close_position(&account, position_id, volume(*quantity)?)
                    .map_err(|error| wire_error(&error))
            }),
            VenueRequest::AmendPositionProtection {
                instrument_id,
                broker_position_id,
                stop_loss,
                take_profit,
                ..
            } => {
                let spec = self.venue_spec(
                    ctid,
                    demo_symbol(instrument_id, ctid).map_err(Failure::Request)?,
                )?;
                let scale = spec.price_scale;
                (|| {
                    TradingRequest::amend_position_protection(
                        &account,
                        scale,
                        broker_id(broker_position_id, "position id")?,
                        stop_loss
                            .map(|price| price_units(price, scale))
                            .transpose()?,
                        take_profit
                            .map(|price| price_units(price, scale))
                            .transpose()?,
                    )
                    .map_err(|error| wire_error(&error))
                })()
            }
            VenueRequest::Reconcile { .. } => {
                return Err(Failure::Request(
                    "reconcile is not a trading request".into(),
                ));
            }
        }
        .map_err(Failure::Request)?;
        let host = self.ensure_host(false)?;
        host.request(trading(encoded))
    }

    /// The specification of one demo symbol, loaded once per relay generation.
    fn venue_spec(&mut self, ctid: u64, symbol_id: u64) -> Result<SymbolSpec, Failure> {
        if let Some(spec) = self
            .venue
            .as_ref()
            .and_then(|relay| relay.specs.get(&(ctid, symbol_id)))
        {
            return Ok(spec.clone());
        }
        let host = self.ensure_host(false)?;
        host.authorize(ctid)?;
        let frame = host.request(
            MarketRequest::symbol_by_id(ctid, &[symbol_id])
                .map_err(|error| request_error(&error))?,
        )?;
        let spec = decode_symbol_by_id(&frame, ctid)
            .map_err(|error| request_error(&error))?
            .into_iter()
            .find(|spec| spec.symbol_id == symbol_id)
            .ok_or_else(|| Failure::Request("cTrader symbol details are missing".into()))?;
        if let Some(relay) = self.venue.as_mut() {
            if relay.specs.len() >= MAXIMUM_SPECS {
                relay.specs.clear();
            }
            relay.specs.insert((ctid, symbol_id), spec.clone());
        }
        Ok(spec)
    }

    fn ensure_frame_specs(&mut self, ctid: u64, frame: &ProtoMessage) -> Result<(), Failure> {
        let symbols = referenced_symbols(frame).map_err(|error| request_error(&error))?;
        for symbol_id in symbols {
            self.venue_spec(ctid, symbol_id)?;
        }
        Ok(())
    }

    /// Replays deals since `since_millis` (when given), then reports the account's open
    /// orders and positions as one snapshot.
    fn reconcile_account(&mut self, ctid: u64, since_millis: Option<i64>) -> Result<(), HostFault> {
        match self.reconcile_updates(ctid, since_millis) {
            Ok(updates) => {
                for update in updates {
                    self.emit(ctid, update);
                }
                Ok(())
            }
            Err(Failure::Request(error)) => {
                diagnostic!("Aeris cTrader reconcile failed: {error}");
                Ok(())
            }
            Err(Failure::Host(fault)) => Err(fault),
        }
    }

    fn reconcile_updates(
        &mut self,
        ctid: u64,
        since_millis: Option<i64>,
    ) -> Result<Vec<VenueUpdate>, Failure> {
        let host = self.ensure_host(false)?;
        host.authorize(ctid)?;
        if let Some(relay) = self.venue.as_mut() {
            relay.accounts.insert(ctid);
        }
        let mut updates = Vec::new();
        if let Some(since) = since_millis {
            let now = now_nanos().map_err(Failure::Request)? / 1_000_000;
            let host = self.ensure_host(false)?;
            let frame = host.request(trading(
                TradingRequest::deal_list(ctid, since.max(0), now.max(since.max(0) + 1))
                    .map_err(|error| request_error(&error))?,
            ))?;
            self.ensure_frame_specs(ctid, &frame)?;
            let specs = self.venue_specs();
            let translator = Translator {
                ctid,
                specs: &specs,
            };
            let page = decode_deal_page(&frame, ctid, &translator.scales())
                .map_err(|error| request_error(&error))?;
            if page.has_more {
                diagnostic!(
                    "Aeris cTrader deal replay exceeded one page; older deals were skipped"
                );
            }
            updates.extend(translator.fills(&page.deals).map_err(Failure::Request)?);
        }
        let host = self.ensure_host(false)?;
        let frame = host.request(trading(
            TradingRequest::reconcile(ctid).map_err(|error| request_error(&error))?,
        ))?;
        self.ensure_frame_specs(ctid, &frame)?;
        let specs = self.venue_specs();
        let translator = Translator {
            ctid,
            specs: &specs,
        };
        let reconciliation = decode_reconcile(&frame, ctid, &translator.scales())
            .map_err(|error| request_error(&error))?;
        updates.push(VenueUpdate::Snapshot(
            translator
                .snapshot(&reconciliation)
                .map_err(Failure::Request)?,
        ));
        Ok(updates)
    }

    fn venue_specs(&self) -> BTreeMap<(u64, u64), SymbolSpec> {
        self.venue
            .as_ref()
            .map(|relay| relay.specs.clone())
            .unwrap_or_default()
    }

    /// Relays one trading frame from the demo session: the answer to a request, or an
    /// unsolicited event for an account the relay serves.
    pub(super) fn relay_frame(
        &mut self,
        ctid: u64,
        frame: &ProtoMessage,
        request: Option<&VenueRequest>,
    ) {
        if let Err(error) = self.translate_frame(ctid, frame, request) {
            diagnostic!("Aeris cTrader trading event could not be relayed: {error}");
        }
    }

    fn translate_frame(
        &mut self,
        ctid: u64,
        frame: &ProtoMessage,
        request: Option<&VenueRequest>,
    ) -> Result<(), String> {
        match frame.payload_type {
            EXECUTION_EVENT => {
                self.ensure_frame_specs(ctid, frame)
                    .map_err(|failure| failure.detail().to_string())?;
                let specs = self.venue_specs();
                let translator = Translator {
                    ctid,
                    specs: &specs,
                };
                let event = decode_execution_event(frame, ctid, &translator.scales())
                    .map_err(|error| wire_error(&error))?;
                for update in translator.execution(&event)? {
                    self.emit(ctid, update);
                }
            }
            ORDER_ERROR_EVENT => {
                let error = wire::decode_order_error_event(frame, ctid)
                    .map_err(|error| wire_error(&error))?;
                let reason = error.description.unwrap_or(error.error_code);
                match request.and_then(|request| refused(request, reason.clone())) {
                    Some(update) => self.emit(ctid, update),
                    None => diagnostic!("Aeris cTrader order error without a request: {reason}"),
                }
            }
            TRADER_UPDATED_EVENT => {
                let trader = decode_trader(frame, ctid).map_err(|error| wire_error(&error))?;
                self.emit(
                    ctid,
                    VenueUpdate::Balance {
                        balance: money(trader.balance)?,
                    },
                );
            }
            // Aeris never requests trailing stops; the next reconcile reports a stop moved
            // by another client.
            TRAILING_STOP_EVENT => {}
            other => return Err(format!("unexpected cTrader trading answer {other}")),
        }
        Ok(())
    }

    /// Routes an unsolicited demo-session frame to the relay when it is a trading event
    /// for an account the relay serves. Returns whether the frame was a trading event.
    pub(super) fn venue_event(&mut self, live: bool, frame: &ProtoMessage) -> bool {
        if !matches!(
            frame.payload_type,
            EXECUTION_EVENT | ORDER_ERROR_EVENT | TRADER_UPDATED_EVENT | TRAILING_STOP_EVENT
        ) {
            return false;
        }
        let served = event_account(frame).filter(|ctid| {
            !live
                && self
                    .venue
                    .as_ref()
                    .is_some_and(|relay| relay.accounts.contains(ctid))
        });
        if let Some(ctid) = served {
            self.relay_frame(ctid, frame, None);
        }
        true
    }

    /// Whether the demo session must stay open for trading.
    pub(super) const fn venue_holds_demo(&self) -> bool {
        self.venue.is_some()
    }
}

/// Trading requests travel through the same session request path as market requests.
fn trading(request: TradingRequest) -> MarketRequest {
    MarketRequest {
        payload_type: request.payload_type,
        response_type: request.response_type,
        bucket: request.bucket,
        payload: request.payload,
    }
}
