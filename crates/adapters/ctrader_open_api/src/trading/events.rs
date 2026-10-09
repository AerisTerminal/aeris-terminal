use super::{MAXIMUM_TIMESTAMP_MS, TradeSide, positive_id};
use crate::{
    ProtoMessage,
    codec::{self, require_nested_path},
    generated::{
        ProtoOaClosePositionDetail, ProtoOaDeal, ProtoOaExecutionEvent, ProtoOaOrder,
        ProtoOaOrderErrorEvent, ProtoOaPosition, ProtoOaReconcileRes, ProtoOaTraderRes,
        ProtoOaTraderUpdatedEvent,
    },
    market::{MarketDecodeError, PriceScale},
};
use prost::Message as _;
use std::collections::BTreeSet;

/// Bound on each list of positions, orders or deals one response may report.
pub const MAXIMUM_STATE_ITEMS: usize = 4096;
const MAXIMUM_MONEY_DIGITS: u32 = 18;
const MAXIMUM_PRICE_DIGITS: i32 = 10;
const MAXIMUM_TEXT_BYTES: usize = 512;
const MAXIMUM_CLIENT_ORDER_ID_BYTES: usize = 50;

const TRADE_DATA: [(u32, &str); 3] = [(1, "symbolId"), (2, "volume"), (3, "tradeSide")];
const ORDER: [(u32, &str); 4] = [
    (1, "orderId"),
    (2, "tradeData"),
    (3, "orderType"),
    (4, "orderStatus"),
];
const POSITION: [(u32, &str); 4] = [
    (1, "positionId"),
    (2, "tradeData"),
    (3, "positionStatus"),
    (4, "swap"),
];
const DEAL: [(u32, &str); 10] = [
    (1, "dealId"),
    (2, "orderId"),
    (3, "positionId"),
    (4, "volume"),
    (5, "filledVolume"),
    (6, "symbolId"),
    (7, "createTimestamp"),
    (8, "executionTimestamp"),
    (11, "tradeSide"),
    (12, "dealStatus"),
];
const CLOSE_DETAIL: [(u32, &str); 5] = [
    (1, "entryPrice"),
    (2, "grossProfit"),
    (3, "swap"),
    (4, "commission"),
    (5, "balance"),
];

/// A money amount: `units / 10^digits` in the account's deposit currency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Money {
    pub units: i64,
    pub digits: u8,
}

/// A volume-weighted average price, at the finest scale (symbol digits up to 10) that
/// holds the provider's value exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AveragePrice {
    pub units: i64,
    pub digits: u8,
}

/// `ProtoOAExecutionType`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionType {
    Accepted,
    Filled,
    Replaced,
    Cancelled,
    Expired,
    Rejected,
    CancelRejected,
    Swap,
    DepositWithdraw,
    PartialFill,
    BonusDepositWithdraw,
}

/// `ProtoOAOrderType`, including the server-created types Aeris never sends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderKind {
    Market,
    Limit,
    Stop,
    StopLossTakeProfit,
    MarketRange,
    StopLimit,
}

/// `ProtoOAOrderStatus`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderStatus {
    Accepted,
    Filled,
    Rejected,
    Expired,
    Cancelled,
}

/// `ProtoOAPositionStatus`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionStatus {
    Open,
    Closed,
    /// An empty position created for a pending order.
    Created,
    Error,
}

/// `ProtoOADealStatus`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DealStatus {
    Filled,
    PartiallyFilled,
    Rejected,
    InternallyRejected,
    Error,
    Missed,
}

/// `ProtoOAAccountType`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountType {
    Hedged,
    Netted,
    SpreadBetting,
}

/// One order (`ProtoOAOrder`). Prices are at the symbol's scale; volumes in cents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderState {
    pub order_id: u64,
    pub symbol_id: u64,
    pub side: TradeSide,
    pub volume: u64,
    pub kind: OrderKind,
    pub status: OrderStatus,
    pub limit_price: Option<i64>,
    pub stop_price: Option<i64>,
    pub stop_loss: Option<i64>,
    pub take_profit: Option<i64>,
    pub executed_volume: Option<u64>,
    pub execution_price: Option<AveragePrice>,
    pub client_order_id: Option<String>,
    pub position_id: Option<u64>,
    pub closing: bool,
    pub updated_unix_ms: Option<i64>,
}

/// One position (`ProtoOAPosition`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionState {
    pub position_id: u64,
    pub symbol_id: u64,
    pub side: TradeSide,
    pub volume: u64,
    pub status: PositionStatus,
    pub price: Option<AveragePrice>,
    pub stop_loss: Option<i64>,
    pub take_profit: Option<i64>,
    pub swap: Money,
    pub commission: Option<Money>,
    pub opened_unix_ms: Option<i64>,
    pub updated_unix_ms: Option<i64>,
}

/// What a closing deal realized (`ProtoOAClosePositionDetail`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosedVolume {
    pub entry_price: AveragePrice,
    pub gross_profit: Money,
    pub swap: Money,
    pub commission: Money,
    pub balance: Money,
    pub closed_volume: Option<u64>,
}

/// One execution (`ProtoOADeal`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Deal {
    pub deal_id: u64,
    pub order_id: u64,
    pub position_id: u64,
    pub symbol_id: u64,
    pub side: TradeSide,
    pub volume: u64,
    pub filled_volume: u64,
    pub status: DealStatus,
    pub execution_price: Option<i64>,
    pub created_unix_ms: i64,
    pub executed_unix_ms: i64,
    pub commission: Option<Money>,
    pub closed: Option<ClosedVolume>,
}

/// `ProtoOAExecutionEvent` (2126).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionEvent {
    pub execution: ExecutionType,
    pub order: Option<OrderState>,
    pub position: Option<PositionState>,
    pub deal: Option<Deal>,
    pub error_code: Option<String>,
    /// Raised by server logic (for example a stop-out) rather than a request.
    pub server_event: bool,
}

/// `ProtoOAOrderErrorEvent` (2132).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderErrorEvent {
    pub error_code: String,
    pub order_id: Option<u64>,
    pub position_id: Option<u64>,
    pub description: Option<String>,
}

/// Open positions and pending orders (`ProtoOAReconcileRes`, 2125).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Reconciliation {
    pub positions: Vec<PositionState>,
    pub orders: Vec<OrderState>,
}

/// Account state (`ProtoOATrader`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraderAccount {
    pub balance: Money,
    pub deposit_asset_id: u64,
    pub account_type: AccountType,
    pub trader_login: Option<i64>,
    pub leverage_in_cents: Option<u32>,
    /// Limited-risk accounts require a guaranteed stop loss on every position.
    pub limited_risk: bool,
}

pub(super) fn check_account(expected: u64, actual: i64) -> Result<(), MarketDecodeError> {
    if i64::try_from(expected).ok() == Some(actual) {
        Ok(())
    } else {
        Err(MarketDecodeError::AccountMismatch)
    }
}

pub(super) fn payload(frame: &ProtoMessage) -> &[u8] {
    frame.payload.as_deref().unwrap_or_default()
}

fn require(
    payload: &[u8],
    path: &[u32],
    fields: &[(u32, &'static str)],
) -> Result<(), MarketDecodeError> {
    Ok(require_nested_path(payload, path, fields)?)
}

/// Required wire fields of every order at `path`, including its trade data.
pub(super) fn require_orders(payload: &[u8], path: &[u32]) -> Result<(), MarketDecodeError> {
    require(payload, path, &ORDER)?;
    require(payload, &[path, &[2]].concat(), &TRADE_DATA)
}

/// Required wire fields of every deal at `path`, including any close detail.
pub(super) fn require_deals(payload: &[u8], path: &[u32]) -> Result<(), MarketDecodeError> {
    require(payload, path, &DEAL)?;
    require(payload, &[path, &[16]].concat(), &CLOSE_DETAIL)
}

fn require_positions(payload: &[u8], path: &[u32]) -> Result<(), MarketDecodeError> {
    require(payload, path, &POSITION)?;
    require(payload, &[path, &[2]].concat(), &TRADE_DATA)
}

fn volume(value: i64, field: &'static str) -> Result<u64, MarketDecodeError> {
    u64::try_from(value).map_err(|_| MarketDecodeError::InvalidField(field))
}

pub(super) fn timestamp(value: i64, field: &'static str) -> Result<i64, MarketDecodeError> {
    if value <= 0 || value > MAXIMUM_TIMESTAMP_MS {
        return Err(MarketDecodeError::InvalidField(field));
    }
    Ok(value)
}

pub(super) fn money(units: i64, digits: Option<u32>) -> Result<Money, MarketDecodeError> {
    let digits = digits
        .filter(|digits| *digits <= MAXIMUM_MONEY_DIGITS)
        .ok_or(MarketDecodeError::InvalidField("moneyDigits"))?;
    Ok(Money {
        units,
        digits: u8::try_from(digits).map_err(|_| MarketDecodeError::InvalidField("moneyDigits"))?,
    })
}

fn text(value: String, limit: usize, field: &'static str) -> Result<String, MarketDecodeError> {
    if value.trim().is_empty() || value.len() > limit {
        return Err(MarketDecodeError::InvalidField(field));
    }
    Ok(value)
}

fn price(scale: PriceScale, value: Option<f64>) -> Result<Option<i64>, MarketDecodeError> {
    value.map(|value| scale.from_decimal(value)).transpose()
}

fn average_price(scale: PriceScale, value: f64) -> Result<AveragePrice, MarketDecodeError> {
    let mut last = MarketDecodeError::InexactPrice;
    for digits in i32::from(scale.digits())..=MAXIMUM_PRICE_DIGITS {
        let scale = PriceScale::new(digits)?;
        match scale.from_decimal(value) {
            Ok(units) => {
                return Ok(AveragePrice {
                    units,
                    digits: scale.digits(),
                });
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// `scales` gives the price scale of each symbol a message may name; a symbol the caller
/// has no specification for rejects the message instead of guessing a scale.
fn symbol_scale(
    scales: &impl Fn(u64) -> Option<PriceScale>,
    symbol: i64,
) -> Result<(u64, PriceScale), MarketDecodeError> {
    let symbol_id = positive_id(symbol, "symbolId")?;
    let scale = scales(symbol_id).ok_or(MarketDecodeError::UnknownSymbol)?;
    Ok((symbol_id, scale))
}

const fn execution_type(value: i32) -> Result<ExecutionType, MarketDecodeError> {
    Ok(match value {
        2 => ExecutionType::Accepted,
        3 => ExecutionType::Filled,
        4 => ExecutionType::Replaced,
        5 => ExecutionType::Cancelled,
        6 => ExecutionType::Expired,
        7 => ExecutionType::Rejected,
        8 => ExecutionType::CancelRejected,
        9 => ExecutionType::Swap,
        10 => ExecutionType::DepositWithdraw,
        11 => ExecutionType::PartialFill,
        12 => ExecutionType::BonusDepositWithdraw,
        _ => return Err(MarketDecodeError::InvalidField("executionType")),
    })
}

const fn order_kind(value: i32) -> Result<OrderKind, MarketDecodeError> {
    Ok(match value {
        1 => OrderKind::Market,
        2 => OrderKind::Limit,
        3 => OrderKind::Stop,
        4 => OrderKind::StopLossTakeProfit,
        5 => OrderKind::MarketRange,
        6 => OrderKind::StopLimit,
        _ => return Err(MarketDecodeError::InvalidField("orderType")),
    })
}

const fn order_status(value: i32) -> Result<OrderStatus, MarketDecodeError> {
    Ok(match value {
        1 => OrderStatus::Accepted,
        2 => OrderStatus::Filled,
        3 => OrderStatus::Rejected,
        4 => OrderStatus::Expired,
        5 => OrderStatus::Cancelled,
        _ => return Err(MarketDecodeError::InvalidField("orderStatus")),
    })
}

const fn position_status(value: i32) -> Result<PositionStatus, MarketDecodeError> {
    Ok(match value {
        1 => PositionStatus::Open,
        2 => PositionStatus::Closed,
        3 => PositionStatus::Created,
        4 => PositionStatus::Error,
        _ => return Err(MarketDecodeError::InvalidField("positionStatus")),
    })
}

const fn deal_status(value: i32) -> Result<DealStatus, MarketDecodeError> {
    Ok(match value {
        2 => DealStatus::Filled,
        3 => DealStatus::PartiallyFilled,
        4 => DealStatus::Rejected,
        5 => DealStatus::InternallyRejected,
        6 => DealStatus::Error,
        7 => DealStatus::Missed,
        _ => return Err(MarketDecodeError::InvalidField("dealStatus")),
    })
}

const fn account_type(value: Option<i32>) -> Result<AccountType, MarketDecodeError> {
    // The schema declares HEDGED as the default when the field is absent.
    Ok(match value {
        None | Some(0) => AccountType::Hedged,
        Some(1) => AccountType::Netted,
        Some(2) => AccountType::SpreadBetting,
        Some(_) => return Err(MarketDecodeError::InvalidField("accountType")),
    })
}

pub(super) fn order(
    order: ProtoOaOrder,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<OrderState, MarketDecodeError> {
    let (symbol_id, scale) = symbol_scale(scales, order.trade_data.symbol_id)?;
    Ok(OrderState {
        order_id: positive_id(order.order_id, "orderId")?,
        symbol_id,
        side: TradeSide::from_wire(order.trade_data.trade_side)?,
        volume: volume(order.trade_data.volume, "volume")?,
        kind: order_kind(order.order_type)?,
        status: order_status(order.order_status)?,
        limit_price: price(scale, order.limit_price)?,
        stop_price: price(scale, order.stop_price)?,
        stop_loss: price(scale, order.stop_loss)?,
        take_profit: price(scale, order.take_profit)?,
        executed_volume: order
            .executed_volume
            .map(|value| volume(value, "executedVolume"))
            .transpose()?,
        execution_price: order
            .execution_price
            .map(|value| average_price(scale, value))
            .transpose()?,
        client_order_id: order
            .client_order_id
            .map(|id| text(id, MAXIMUM_CLIENT_ORDER_ID_BYTES, "clientOrderId"))
            .transpose()?,
        position_id: order
            .position_id
            .map(|id| positive_id(id, "positionId"))
            .transpose()?,
        closing: order.closing_order == Some(true),
        updated_unix_ms: order
            .utc_last_update_timestamp
            .map(|value| timestamp(value, "utcLastUpdateTimestamp"))
            .transpose()?,
    })
}

fn position(
    position: &ProtoOaPosition,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<PositionState, MarketDecodeError> {
    let (symbol_id, scale) = symbol_scale(scales, position.trade_data.symbol_id)?;
    Ok(PositionState {
        position_id: positive_id(position.position_id, "positionId")?,
        symbol_id,
        side: TradeSide::from_wire(position.trade_data.trade_side)?,
        volume: volume(position.trade_data.volume, "volume")?,
        status: position_status(position.position_status)?,
        // Observed on demo: a position with no fills yet (a pending order's empty position,
        // or before a market fill) and a closed position carry `price = 0`, meaning no
        // average price rather than a price of zero.
        price: position
            .price
            .filter(|value| *value != 0.0)
            .map(|value| average_price(scale, value))
            .transpose()?,
        stop_loss: price(scale, position.stop_loss)?,
        take_profit: price(scale, position.take_profit)?,
        swap: money(position.swap, position.money_digits)?,
        commission: position
            .commission
            .map(|units| money(units, position.money_digits))
            .transpose()?,
        opened_unix_ms: position
            .trade_data
            .open_timestamp
            .map(|value| timestamp(value, "openTimestamp"))
            .transpose()?,
        updated_unix_ms: position
            .utc_last_update_timestamp
            .map(|value| timestamp(value, "utcLastUpdateTimestamp"))
            .transpose()?,
    })
}

fn closed(
    detail: &ProtoOaClosePositionDetail,
    scale: PriceScale,
) -> Result<ClosedVolume, MarketDecodeError> {
    let digits = detail.money_digits;
    Ok(ClosedVolume {
        entry_price: average_price(scale, detail.entry_price)?,
        gross_profit: money(detail.gross_profit, digits)?,
        swap: money(detail.swap, digits)?,
        commission: money(detail.commission, digits)?,
        balance: money(detail.balance, digits)?,
        closed_volume: detail
            .closed_volume
            .map(|value| volume(value, "closedVolume"))
            .transpose()?,
    })
}

pub(super) fn deal(
    deal: &ProtoOaDeal,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<Deal, MarketDecodeError> {
    let (symbol_id, scale) = symbol_scale(scales, deal.symbol_id)?;
    Ok(Deal {
        deal_id: positive_id(deal.deal_id, "dealId")?,
        order_id: positive_id(deal.order_id, "orderId")?,
        position_id: positive_id(deal.position_id, "positionId")?,
        symbol_id,
        side: TradeSide::from_wire(deal.trade_side)?,
        volume: volume(deal.volume, "volume")?,
        filled_volume: volume(deal.filled_volume, "filledVolume")?,
        status: deal_status(deal.deal_status)?,
        execution_price: price(scale, deal.execution_price)?,
        created_unix_ms: timestamp(deal.create_timestamp, "createTimestamp")?,
        executed_unix_ms: timestamp(deal.execution_timestamp, "executionTimestamp")?,
        commission: deal
            .commission
            .map(|units| money(units, deal.money_digits))
            .transpose()?,
        closed: deal
            .close_position_detail
            .as_ref()
            .map(|detail| closed(detail, scale))
            .transpose()?,
    })
}

/// Decode a `ProtoOAExecutionEvent` (2126) for one account.
///
/// # Errors
/// Rejects another account, missing required fields at any depth, unknown enum values,
/// symbols without a known scale, and prices finer than the symbol scale.
pub fn decode_execution_event(
    frame: &ProtoMessage,
    ctid: u64,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<ExecutionEvent, MarketDecodeError> {
    let event: ProtoOaExecutionEvent = codec::decode_typed(
        frame,
        2126,
        &[(2, "ctidTraderAccountId"), (3, "executionType")],
        |_| Ok(()),
    )?;
    let bytes = payload(frame);
    require_positions(bytes, &[4])?;
    require_orders(bytes, &[5])?;
    require_deals(bytes, &[6])?;
    check_account(ctid, event.ctid_trader_account_id)?;
    Ok(ExecutionEvent {
        execution: execution_type(event.execution_type)?,
        order: event.order.map(|value| order(value, scales)).transpose()?,
        position: event
            .position
            .as_ref()
            .map(|value| position(value, scales))
            .transpose()?,
        deal: event
            .deal
            .as_ref()
            .map(|value| deal(value, scales))
            .transpose()?,
        error_code: event
            .error_code
            .map(|code| text(code, MAXIMUM_TEXT_BYTES, "errorCode"))
            .transpose()?,
        server_event: event.is_server_event == Some(true),
    })
}

/// The trading account an unsolicited trading event belongs to, read before it is
/// decoded for that account. Other payload types name none.
#[must_use]
pub fn event_account(frame: &ProtoMessage) -> Option<u64> {
    let bytes = payload(frame);
    let account = match frame.payload_type {
        2126 => ProtoOaExecutionEvent::decode(bytes)
            .ok()
            .map(|event| event.ctid_trader_account_id),
        2132 => ProtoOaOrderErrorEvent::decode(bytes)
            .ok()
            .map(|event| event.ctid_trader_account_id),
        2123 => ProtoOaTraderUpdatedEvent::decode(bytes)
            .ok()
            .map(|event| event.ctid_trader_account_id),
        2107 => crate::generated::ProtoOaTrailingSlChangedEvent::decode(bytes)
            .ok()
            .map(|event| event.ctid_trader_account_id),
        _ => None,
    }?;
    u64::try_from(account).ok().filter(|ctid| *ctid > 0)
}

/// The symbol ids a trading frame names, so a caller can load their price scales before
/// decoding it. Payload types that name no symbol return none.
///
/// # Errors
/// Rejects a malformed frame.
pub fn referenced_symbols(frame: &ProtoMessage) -> Result<BTreeSet<u64>, MarketDecodeError> {
    fn decode<M: prost::Message + Default>(bytes: &[u8]) -> Result<M, MarketDecodeError> {
        M::decode(bytes).map_err(|error| codec::CodecError::MalformedProtobuf(error).into())
    }
    let bytes = payload(frame);
    let ids: Vec<i64> = match frame.payload_type {
        2126 => {
            let event: ProtoOaExecutionEvent = decode(bytes)?;
            event
                .order
                .map(|order| order.trade_data.symbol_id)
                .into_iter()
                .chain(event.position.map(|position| position.trade_data.symbol_id))
                .chain(event.deal.map(|deal| deal.symbol_id))
                .collect()
        }
        2125 => {
            let response: ProtoOaReconcileRes = decode(bytes)?;
            response
                .position
                .iter()
                .map(|position| position.trade_data.symbol_id)
                .chain(
                    response
                        .order
                        .iter()
                        .map(|order| order.trade_data.symbol_id),
                )
                .collect()
        }
        2182 => {
            let response: crate::generated::ProtoOaOrderDetailsRes = decode(bytes)?;
            std::iter::once(response.order.trade_data.symbol_id)
                .chain(response.deal.iter().map(|deal| deal.symbol_id))
                .collect()
        }
        2134 | 2180 => {
            let response: crate::generated::ProtoOaDealListRes = decode(bytes)?;
            response.deal.iter().map(|deal| deal.symbol_id).collect()
        }
        _ => Vec::new(),
    };
    Ok(ids
        .into_iter()
        .filter_map(|id| u64::try_from(id).ok().filter(|id| *id > 0))
        .collect())
}

/// Decode a `ProtoOAOrderErrorEvent` (2132) for one account.
///
/// # Errors
/// Rejects another account, a missing or blank error code, or invalid ids.
pub fn decode_order_error_event(
    frame: &ProtoMessage,
    ctid: u64,
) -> Result<OrderErrorEvent, MarketDecodeError> {
    let event: ProtoOaOrderErrorEvent = codec::decode_typed(
        frame,
        2132,
        &[(5, "ctidTraderAccountId"), (2, "errorCode")],
        |_| Ok(()),
    )?;
    check_account(ctid, event.ctid_trader_account_id)?;
    Ok(OrderErrorEvent {
        error_code: text(event.error_code, MAXIMUM_TEXT_BYTES, "errorCode")?,
        order_id: event
            .order_id
            .map(|id| positive_id(id, "orderId"))
            .transpose()?,
        position_id: event
            .position_id
            .map(|id| positive_id(id, "positionId"))
            .transpose()?,
        description: event
            .description
            .filter(|description| !description.trim().is_empty())
            .map(|description| text(description, MAXIMUM_TEXT_BYTES, "description"))
            .transpose()?,
    })
}

/// Decode a `ProtoOAReconcileRes` (2125) for one account.
///
/// # Errors
/// Rejects another account, oversized lists, and any invalid position or order.
pub fn decode_reconcile(
    frame: &ProtoMessage,
    ctid: u64,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<Reconciliation, MarketDecodeError> {
    let response: ProtoOaReconcileRes =
        codec::decode_typed(frame, 2125, &[(2, "ctidTraderAccountId")], |_| Ok(()))?;
    let bytes = payload(frame);
    require_positions(bytes, &[3])?;
    require_orders(bytes, &[4])?;
    check_account(ctid, response.ctid_trader_account_id)?;
    if response.position.len() > MAXIMUM_STATE_ITEMS || response.order.len() > MAXIMUM_STATE_ITEMS {
        return Err(MarketDecodeError::LimitExceeded("reconcile"));
    }
    Ok(Reconciliation {
        positions: response
            .position
            .iter()
            .map(|value| position(value, scales))
            .collect::<Result<_, _>>()?,
        orders: response
            .order
            .into_iter()
            .map(|value| order(value, scales))
            .collect::<Result<_, _>>()?,
    })
}

/// Decode a `ProtoOATraderRes` (2122) or `ProtoOATraderUpdatedEvent` (2123) for one
/// account; both carry the whole `ProtoOATrader`.
///
/// # Errors
/// Rejects another account, missing required fields, or unknown account types.
pub fn decode_trader(frame: &ProtoMessage, ctid: u64) -> Result<TraderAccount, MarketDecodeError> {
    const REQUIRED: [(u32, &str); 2] = [(2, "ctidTraderAccountId"), (3, "trader")];
    let response = if frame.payload_type == 2123 {
        let event: ProtoOaTraderUpdatedEvent =
            codec::decode_typed(frame, 2123, &REQUIRED, |_| Ok(()))?;
        ProtoOaTraderRes {
            payload_type: None,
            ctid_trader_account_id: event.ctid_trader_account_id,
            trader: event.trader,
        }
    } else {
        codec::decode_typed(frame, 2122, &REQUIRED, |_| Ok(()))?
    };
    require(
        payload(frame),
        &[3],
        &[
            (1, "ctidTraderAccountId"),
            (2, "balance"),
            (8, "depositAssetId"),
        ],
    )?;
    check_account(ctid, response.ctid_trader_account_id)?;
    let trader = response.trader;
    check_account(ctid, trader.ctid_trader_account_id)?;
    Ok(TraderAccount {
        balance: money(trader.balance, trader.money_digits)?,
        deposit_asset_id: positive_id(trader.deposit_asset_id, "depositAssetId")?,
        account_type: account_type(trader.account_type)?,
        trader_login: trader.trader_login,
        leverage_in_cents: trader.leverage_in_cents,
        limited_risk: trader.is_limited_risk == Some(true),
    })
}

/// Fixtures carry the fields a demo EURUSD session sent on 2026-10-08 (captured with the
/// `ctrader_trading_capture` example); ids, prices and the account are placeholders.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generated::{ProtoOaTradeData, ProtoOaTrader},
        market::fixtures::{CTID, CTID_WIRE, bytes_frame, frame, strip, strip_at},
    };
    use prost::Message;

    const EURUSD: u64 = 1;

    fn scales(symbol: u64) -> Option<PriceScale> {
        (symbol == EURUSD).then(|| PriceScale::new(5).expect("digits 5"))
    }

    fn trade_data(volume: i64) -> ProtoOaTradeData {
        ProtoOaTradeData {
            symbol_id: 1,
            volume,
            trade_side: 1,
            ..ProtoOaTradeData::default()
        }
    }

    fn filled_market_order() -> ProtoOaExecutionEvent {
        ProtoOaExecutionEvent {
            ctid_trader_account_id: CTID_WIRE,
            execution_type: 3,
            order: Some(ProtoOaOrder {
                order_id: 9,
                trade_data: trade_data(100_000),
                order_type: 1,
                order_status: 2,
                execution_price: Some(1.08543),
                executed_volume: Some(100_000),
                client_order_id: Some("aeris-1".into()),
                position_id: Some(77),
                utc_last_update_timestamp: Some(1_791_467_000_000),
                closing_order: Some(false),
                time_in_force: Some(3),
                relative_stop_loss: Some(5_000),
                relative_take_profit: Some(5_000),
                stop_trigger_method: Some(1),
                ..ProtoOaOrder::default()
            }),
            position: Some(ProtoOaPosition {
                position_id: 77,
                trade_data: trade_data(100_000),
                position_status: 1,
                swap: 0,
                price: Some(1.08543),
                stop_loss: Some(1.08343),
                commission: Some(-350),
                margin_rate: Some(1.08543),
                mirroring_commission: Some(0),
                guaranteed_stop_loss: Some(false),
                used_margin: Some(361),
                money_digits: Some(2),
                trailing_stop_loss: Some(false),
                ..ProtoOaPosition::default()
            }),
            deal: Some(ProtoOaDeal {
                deal_id: 501,
                order_id: 9,
                position_id: 77,
                volume: 100_000,
                filled_volume: 100_000,
                symbol_id: 1,
                create_timestamp: 1_791_466_999_900,
                execution_timestamp: 1_791_467_000_000,
                execution_price: Some(1.08543),
                trade_side: 1,
                deal_status: 2,
                commission: Some(-350),
                money_digits: Some(2),
                ..ProtoOaDeal::default()
            }),
            is_server_event: Some(false),
            ..ProtoOaExecutionEvent::default()
        }
    }

    #[test]
    fn an_accepted_market_orders_empty_position_has_no_average_price() {
        let mut event = filled_market_order();
        event.execution_type = 2;
        event.deal = None;
        if let Some(order) = event.order.as_mut() {
            order.order_status = 1;
            order.execution_price = None;
            order.executed_volume = Some(0);
        }
        if let Some(position) = event.position.as_mut() {
            position.price = Some(0.0);
            position.stop_loss = None;
        }
        let decoded = decode_execution_event(&frame(2126, &event), CTID, &scales).expect("ack");
        assert_eq!(decoded.execution, ExecutionType::Accepted);
        assert_eq!(decoded.position.expect("position").price, None);
    }

    #[test]
    fn protection_arrives_as_a_server_created_closing_order() {
        // After a protected market fill the server sends, without the request's id, an
        // accepted STOP_LOSS_TAKE_PROFIT order that closes the position.
        let mut event = filled_market_order();
        event.execution_type = 2;
        event.deal = None;
        event.is_server_event = Some(true);
        event.order = Some(ProtoOaOrder {
            order_id: 10,
            trade_data: ProtoOaTradeData {
                trade_side: 2,
                ..trade_data(100_000)
            },
            order_type: 4,
            order_status: 1,
            limit_price: Some(1.09043),
            stop_price: Some(1.08043),
            executed_volume: Some(0),
            closing_order: Some(true),
            client_order_id: Some("aeris-1".into()),
            position_id: Some(77),
            ..ProtoOaOrder::default()
        });
        let decoded = decode_execution_event(&frame(2126, &event), CTID, &scales).expect("sl/tp");
        assert!(decoded.server_event);
        let order = decoded.order.expect("protection order");
        assert_eq!(
            (order.kind, order.side, order.closing),
            (OrderKind::StopLossTakeProfit, TradeSide::Sell, true)
        );
        assert_eq!(
            (order.limit_price, order.stop_price),
            (Some(109_043), Some(108_043))
        );
    }

    #[test]
    fn a_fill_decodes_order_position_and_deal_at_exact_scales() {
        let event = decode_execution_event(&frame(2126, &filled_market_order()), CTID, &scales)
            .expect("fill");
        assert_eq!(event.execution, ExecutionType::Filled);
        assert!(!event.server_event);
        let order = event.order.expect("order");
        assert_eq!(
            (order.order_id, order.kind, order.status),
            (9, OrderKind::Market, OrderStatus::Filled)
        );
        assert_eq!(order.client_order_id.as_deref(), Some("aeris-1"));
        assert_eq!(
            order.execution_price,
            Some(AveragePrice {
                units: 108_543,
                digits: 5
            })
        );
        let position = event.position.expect("position");
        assert_eq!((position.position_id, position.side), (77, TradeSide::Buy));
        assert_eq!(position.stop_loss, Some(108_343));
        assert_eq!(
            position.commission,
            Some(Money {
                units: -350,
                digits: 2
            })
        );
        let deal = event.deal.expect("deal");
        assert_eq!((deal.deal_id, deal.filled_volume), (501, 100_000));
        assert_eq!(deal.execution_price, Some(108_543));
        assert_eq!(deal.status, DealStatus::Filled);
        assert!(deal.closed.is_none());
    }

    #[test]
    fn an_average_price_keeps_its_extra_precision_instead_of_rounding() {
        let mut event = filled_market_order();
        if let Some(position) = event.position.as_mut() {
            position.price = Some(1.085_435);
        }
        let decoded =
            decode_execution_event(&frame(2126, &event), CTID, &scales).expect("vwap position");
        assert_eq!(
            decoded.position.expect("position").price,
            Some(AveragePrice {
                units: 1_085_435,
                digits: 6
            })
        );
        // A quoted price finer than the symbol scale is rejected, not rounded.
        if let Some(deal) = event.deal.as_mut() {
            deal.execution_price = Some(1.085_435);
        }
        assert!(matches!(
            decode_execution_event(&frame(2126, &event), CTID, &scales),
            Err(MarketDecodeError::InexactPrice)
        ));
    }

    #[test]
    fn a_closing_deal_reports_what_it_realized() {
        let mut event = filled_market_order();
        if let Some(deal) = event.deal.as_mut() {
            deal.close_position_detail = Some(ProtoOaClosePositionDetail {
                entry_price: 1.08543,
                gross_profit: 1_250,
                swap: -12,
                commission: -350,
                balance: 1_000_888,
                closed_volume: Some(100_000),
                money_digits: Some(2),
                ..ProtoOaClosePositionDetail::default()
            });
        }
        let closed = decode_execution_event(&frame(2126, &event), CTID, &scales)
            .expect("close")
            .deal
            .and_then(|deal| deal.closed)
            .expect("closed volume");
        assert_eq!(
            closed.gross_profit,
            Money {
                units: 1_250,
                digits: 2
            }
        );
        assert_eq!(closed.balance.units, 1_000_888);
        assert_eq!(closed.closed_volume, Some(100_000));
    }

    #[test]
    fn required_fields_are_enforced_at_every_depth() {
        let payload = filled_market_order().encode_to_vec();
        let cases: [(&[u32], u32); 8] = [
            (&[], 3),
            (&[5], 1),
            (&[5], 4),
            (&[5, 2], 3),
            (&[4], 4),
            (&[4, 2], 1),
            (&[6], 1),
            (&[6], 12),
        ];
        for (path, field) in cases {
            let stripped = if path.is_empty() {
                strip(&payload, field)
            } else {
                strip_at(&payload, path, field)
            };
            assert!(
                decode_execution_event(&bytes_frame(2126, stripped), CTID, &scales).is_err(),
                "field {field} under {path:?} must be required"
            );
        }
    }

    #[test]
    fn foreign_accounts_unknown_symbols_and_money_without_digits_are_rejected() {
        let fill = frame(2126, &filled_market_order());
        assert!(matches!(
            decode_execution_event(&fill, CTID + 1, &scales),
            Err(MarketDecodeError::AccountMismatch)
        ));
        assert!(matches!(
            decode_execution_event(&fill, CTID, &|_| None),
            Err(MarketDecodeError::UnknownSymbol)
        ));
        let mut event = filled_market_order();
        if let Some(position) = event.position.as_mut() {
            position.money_digits = None;
        }
        assert!(matches!(
            decode_execution_event(&frame(2126, &event), CTID, &scales),
            Err(MarketDecodeError::InvalidField("moneyDigits"))
        ));
        let mut event = filled_market_order();
        event.execution_type = 1;
        assert!(decode_execution_event(&frame(2126, &event), CTID, &scales).is_err());
    }

    #[test]
    fn order_errors_carry_their_code_and_ids() {
        let event = ProtoOaOrderErrorEvent {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            error_code: "TRADING_BAD_VOLUME".into(),
            order_id: Some(9),
            position_id: None,
            description: Some(" ".into()),
        };
        let decoded = decode_order_error_event(&frame(2132, &event), CTID).expect("error");
        assert_eq!(decoded.error_code, "TRADING_BAD_VOLUME");
        assert_eq!((decoded.order_id, decoded.position_id), (Some(9), None));
        assert_eq!(decoded.description, None);
        let payload = event.encode_to_vec();
        assert!(decode_order_error_event(&bytes_frame(2132, strip(&payload, 2)), CTID).is_err());
        assert!(matches!(
            decode_order_error_event(&frame(2132, &event), CTID + 1),
            Err(MarketDecodeError::AccountMismatch)
        ));
    }

    #[test]
    fn reconcile_lists_open_positions_and_pending_orders() {
        let event = filled_market_order();
        let response = ProtoOaReconcileRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            position: event.position.into_iter().collect(),
            order: vec![ProtoOaOrder {
                order_id: 10,
                trade_data: trade_data(50_000),
                order_type: 2,
                order_status: 1,
                limit_price: Some(1.0825),
                ..ProtoOaOrder::default()
            }],
        };
        let reconciled = decode_reconcile(&frame(2125, &response), CTID, &scales).expect("state");
        assert_eq!(reconciled.positions.len(), 1);
        assert_eq!(reconciled.orders[0].limit_price, Some(108_250));
        assert_eq!(reconciled.orders[0].kind, OrderKind::Limit);
        let payload = response.encode_to_vec();
        assert!(
            decode_reconcile(
                &bytes_frame(2125, strip_at(&payload, &[4, 2], 2)),
                CTID,
                &scales
            )
            .is_err()
        );
    }

    #[test]
    fn trader_reports_balance_deposit_asset_and_account_type() {
        let response = ProtoOaTraderRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            trader: ProtoOaTrader {
                ctid_trader_account_id: CTID_WIRE,
                balance: 1_000_000,
                deposit_asset_id: 11,
                money_digits: Some(2),
                account_type: Some(1),
                leverage_in_cents: Some(50_000),
                ..ProtoOaTrader::default()
            },
        };
        let trader = decode_trader(&frame(2122, &response), CTID).expect("trader");
        assert_eq!(
            trader.balance,
            Money {
                units: 1_000_000,
                digits: 2
            }
        );
        assert_eq!(
            (trader.deposit_asset_id, trader.account_type),
            (11, AccountType::Netted)
        );
        assert!(!trader.limited_risk);
        // An absent account type is the schema's HEDGED default.
        let mut hedged = response.clone();
        hedged.trader.account_type = None;
        assert_eq!(
            decode_trader(&frame(2122, &hedged), CTID)
                .expect("hedged")
                .account_type,
            AccountType::Hedged
        );
        let payload = response.encode_to_vec();
        assert!(decode_trader(&bytes_frame(2122, strip_at(&payload, &[3], 8)), CTID).is_err());
        let mut foreign = response;
        foreign.trader.ctid_trader_account_id = CTID_WIRE + 1;
        assert!(decode_trader(&frame(2122, &foreign), CTID).is_err());
    }
}
