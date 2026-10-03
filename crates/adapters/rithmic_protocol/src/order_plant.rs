//! Order-plant requests and replies: accounts, trade routes, order commands,
//! order notifications, and serial execution history.

use crate::{
    ProviderTimestamp, RithmicAccountKey, RithmicAccountRef, RithmicDecimal, RithmicRequestKind,
    RithmicRequestOutcome,
};
use core::fmt;

/// Longest inclusive window accepted by one fill-history request. Rithmic
/// serves at most 30 days per `request_show_fill_history`.
pub const MAXIMUM_FILL_HISTORY_WINDOW_SECONDS: i32 = 30 * 86_400;
/// Largest record count Rithmic accepts for one fill-history request.
pub const MAXIMUM_FILL_HISTORY_RECORDS: u16 = 10_000;

/// Direction of a new order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicOrderSide {
    Buy,
    Sell,
}

/// Time in force for a new order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicOrderDuration {
    Day,
    GoodTillCancelled,
    ImmediateOrCancel,
    FillOrKill,
}

/// Whether a person or an automated strategy originated the command. Rithmic
/// conformance requires this to reflect the real originator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicOrderPlacement {
    Manual,
    Automated,
}

/// Order type with the prices it requires. Prices carry the instrument price
/// scale chosen by the caller and must convert to the wire double exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicOrderType {
    Market,
    Limit {
        price: RithmicDecimal,
    },
    StopMarket {
        trigger: RithmicDecimal,
    },
    StopLimit {
        price: RithmicDecimal,
        trigger: RithmicDecimal,
    },
}

/// Lists the accounts visible to the logged-in user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountListRequest<'a> {
    pub fcm_id: Option<&'a str>,
    pub ib_id: Option<&'a str>,
}

/// Lists trade routes; with `subscribe_for_updates` the plant pushes route
/// status changes instead of requiring polling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TradeRoutesRequest {
    pub subscribe_for_updates: bool,
}

/// Submits one order. `user_tag` is the caller's client order id; Rithmic
/// echoes it on the command reply and on the order's notifications.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NewOrderRequest<'a> {
    pub account: RithmicAccountKey<'a>,
    pub user_tag: &'a str,
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub trade_route: &'a str,
    pub side: RithmicOrderSide,
    pub quantity: u32,
    pub duration: RithmicOrderDuration,
    pub order_type: RithmicOrderType,
    pub placement: RithmicOrderPlacement,
}

/// Replaces quantity, type, or prices of one working order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModifyOrderRequest<'a> {
    pub account: RithmicAccountKey<'a>,
    pub basket_id: &'a str,
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub quantity: u32,
    pub order_type: RithmicOrderType,
    pub placement: RithmicOrderPlacement,
}

/// Cancels one working order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelOrderRequest<'a> {
    pub account: RithmicAccountKey<'a>,
    pub basket_id: &'a str,
    pub placement: RithmicOrderPlacement,
}

/// Cancels every working order of one account.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelAllOrdersRequest<'a> {
    pub account: RithmicAccountKey<'a>,
    pub placement: RithmicOrderPlacement,
}

/// Replays one account's executions over an inclusive Unix-second range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionReplayRequest<'a> {
    pub account: RithmicAccountKey<'a>,
    pub start_seconds: i32,
    pub finish_seconds: i32,
}

/// Requests one account's fills over an inclusive Unix-second range of at most
/// [`MAXIMUM_FILL_HISTORY_WINDOW_SECONDS`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FillHistoryRequest<'a> {
    pub account: RithmicAccountKey<'a>,
    pub start_seconds: i32,
    pub finish_seconds: i32,
    pub maximum_records: Option<u16>,
}

/// Outbound request accepted only on an authenticated order-plant session.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum OrderPlantRequest<'a> {
    LoginInfo,
    AccountList(AccountListRequest<'a>),
    SubscribeOrderUpdates(RithmicAccountKey<'a>),
    TradeRoutes(TradeRoutesRequest),
    NewOrder(NewOrderRequest<'a>),
    ModifyOrder(ModifyOrderRequest<'a>),
    CancelOrder(CancelOrderRequest<'a>),
    CancelAllOrders(CancelAllOrdersRequest<'a>),
    ShowOrders(RithmicAccountKey<'a>),
    ReplayExecutions(ExecutionReplayRequest<'a>),
    ShowFillHistory(FillHistoryRequest<'a>),
}

impl OrderPlantRequest<'_> {
    #[must_use]
    pub const fn kind(&self) -> RithmicRequestKind {
        match self {
            Self::LoginInfo => RithmicRequestKind::LoginInfo,
            Self::AccountList(_) => RithmicRequestKind::AccountList,
            Self::SubscribeOrderUpdates(_) => RithmicRequestKind::OrderUpdates,
            Self::TradeRoutes(_) => RithmicRequestKind::TradeRoutes,
            Self::NewOrder(_) => RithmicRequestKind::NewOrder,
            Self::ModifyOrder(_) => RithmicRequestKind::ModifyOrder,
            Self::CancelOrder(_) => RithmicRequestKind::CancelOrder,
            Self::CancelAllOrders(_) => RithmicRequestKind::CancelAllOrders,
            Self::ShowOrders(_) => RithmicRequestKind::ShowOrders,
            Self::ReplayExecutions(_) => RithmicRequestKind::ReplayExecutions,
            Self::ShowFillHistory(_) => RithmicRequestKind::FillHistory,
        }
    }
}

impl fmt::Debug for OrderPlantRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "OrderPlantRequest::{:?}", self.kind())
    }
}

/// Role of the logged-in user as reported by `response_login_info`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicUserType {
    Admin,
    Fcm,
    Ib,
    Trader,
}

/// Non-personal subset of `response_login_info`. Names, addresses, phone
/// numbers, and e-mail are intentionally never decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicLoginInfo {
    pub fcm_id: Option<String>,
    pub ib_id: Option<String>,
    pub user_type: Option<RithmicUserType>,
    pub ticker_plant_session_limit: Option<u32>,
    pub order_plant_session_limit: Option<u32>,
}

/// One tradable account from `response_account_list`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicAccount {
    pub fcm_id: String,
    pub ib_id: String,
    pub account_id: RithmicAccountRef,
    pub account_name: Option<String>,
    pub currency: Option<String>,
}

impl RithmicAccount {
    /// Borrows the routing identity used by account-scoped requests.
    #[must_use]
    pub fn key(&self) -> RithmicAccountKey<'_> {
        RithmicAccountKey {
            fcm_id: &self.fcm_id,
            ib_id: &self.ib_id,
            account_id: self.account_id.as_str(),
        }
    }
}

/// One trade route from `response_trade_routes` or a pushed `trade_route` update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicTradeRoute {
    pub fcm_id: String,
    pub ib_id: String,
    pub exchange: String,
    pub trade_route: String,
    pub status: Option<String>,
    pub is_default: Option<bool>,
}

/// Per-request acknowledgement (`rq_handler_rp_code`) of a new, modify, or
/// cancel command. An accepted acknowledgement always carries the basket id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicOrderCommandReply {
    pub request: RithmicRequestKind,
    pub user_tag: Option<String>,
    pub basket_id: Option<String>,
    pub outcome: RithmicRequestOutcome,
    pub timestamp: Option<ProviderTimestamp>,
}

/// Terminal reply (`rp_code`) that ends one request. `user_tag` and
/// `basket_id` are present only when the provider echoes them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicRequestCompletion {
    pub request: RithmicRequestKind,
    pub user_tag: Option<String>,
    pub basket_id: Option<String>,
    pub outcome: RithmicRequestOutcome,
}

/// Order side as reported by notifications, which add short sales.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicReportedSide {
    Buy,
    Sell,
    SellShort,
}

/// Order type as reported by notifications.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicReportedPriceType {
    Limit,
    Market,
    StopLimit,
    StopMarket,
}

/// Rithmic-side order lifecycle step from `rithmic_order_notification`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicOrderNotifyType {
    OrderReceivedFromClient,
    ModifyReceivedFromClient,
    CancelReceivedFromClient,
    OpenPending,
    ModifyPending,
    CancelPending,
    OrderReceivedByExchangeGateway,
    ModifyReceivedByExchangeGateway,
    CancelReceivedByExchangeGateway,
    OrderSentToExchange,
    ModifySentToExchange,
    CancelSentToExchange,
    Open,
    Modified,
    Complete,
    ModificationFailed,
    CancellationFailed,
    TriggerPending,
    Generic,
    LinkOrdersFailed,
}

/// Exchange-side report kind from `exchange_order_notification`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicExchangeNotifyType {
    Status,
    Modify,
    Cancel,
    Trigger,
    Fill,
    Reject,
    NotModified,
    NotCancelled,
    Generic,
}

/// Order state shared by both notification templates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicOrderDetails {
    pub user_tag: Option<String>,
    /// Set when the notification belongs to a `request_show_orders` snapshot
    /// rather than a live change.
    pub is_snapshot: bool,
    pub basket_id: String,
    pub account_id: RithmicAccountRef,
    pub symbol: String,
    pub exchange: String,
    pub trade_route: Option<String>,
    pub exchange_order_id: Option<String>,
    pub status: Option<String>,
    pub side: Option<RithmicReportedSide>,
    pub quantity: Option<u32>,
    pub price: Option<RithmicDecimal>,
    pub trigger_price: Option<RithmicDecimal>,
    pub price_type: Option<RithmicReportedPriceType>,
    pub duration: Option<RithmicOrderDuration>,
    pub placement: Option<RithmicOrderPlacement>,
    pub average_fill_price: Option<RithmicDecimal>,
    pub total_fill_size: Option<u32>,
    pub total_unfilled_size: Option<u32>,
    pub text: Option<String>,
    pub timestamp: Option<ProviderTimestamp>,
}

/// Decoded `rithmic_order_notification` (template 351).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicOrderNotification {
    pub notify_type: RithmicOrderNotifyType,
    pub order: RithmicOrderDetails,
    pub completion_reason: Option<String>,
}

/// One execution carried by an exchange fill notification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicFill {
    pub fill_id: String,
    pub price: RithmicDecimal,
    pub size: u32,
    pub fill_date: Option<String>,
    pub fill_time: Option<String>,
}

/// Decoded `exchange_order_notification` (template 352). `fill` is present
/// exactly when `notify_type` is [`RithmicExchangeNotifyType::Fill`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicExchangeOrderNotification {
    pub notify_type: RithmicExchangeNotifyType,
    pub order: RithmicOrderDetails,
    pub report_type: Option<String>,
    pub fill: Option<RithmicFill>,
}

/// One row of `response_show_fill_history`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicFillHistoryRow {
    pub basket_id: String,
    pub account_id: RithmicAccountRef,
    pub symbol: String,
    pub exchange: String,
    pub fill_id: String,
    pub fill_price: RithmicDecimal,
    pub fill_size: u32,
    /// Provider side label; this template carries it as free text.
    pub transaction_type: Option<String>,
    pub fill_date: Option<String>,
    pub fill_time: Option<String>,
    pub timestamp: Option<ProviderTimestamp>,
}

/// Sanitized order-plant message decoded from one provider WebSocket message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedOrderMessage {
    LoginInfo(RithmicLoginInfo),
    Account(RithmicAccount),
    TradeRoute(RithmicTradeRoute),
    TradeRouteUpdate(RithmicTradeRoute),
    OrderCommand(RithmicOrderCommandReply),
    OrderNotification(RithmicOrderNotification),
    ExchangeOrderNotification(RithmicExchangeOrderNotification),
    FillHistory(RithmicFillHistoryRow),
    RequestComplete(RithmicRequestCompletion),
}

#[cfg(rithmic_kit)]
pub(crate) use kit::{OUTBOUND_TEMPLATES, decode, encode};

#[cfg(not(rithmic_kit))]
pub(crate) const fn decode(
    _frame: &[u8],
) -> Result<Option<DecodedOrderMessage>, crate::ProtocolError> {
    Err(crate::ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
mod kit {
    use super::{
        AccountListRequest, CancelAllOrdersRequest, CancelOrderRequest, DecodedOrderMessage,
        ExecutionReplayRequest, FillHistoryRequest, MAXIMUM_FILL_HISTORY_RECORDS,
        MAXIMUM_FILL_HISTORY_WINDOW_SECONDS, ModifyOrderRequest, NewOrderRequest,
        OrderPlantRequest, RithmicAccount, RithmicExchangeNotifyType,
        RithmicExchangeOrderNotification, RithmicFill, RithmicFillHistoryRow, RithmicLoginInfo,
        RithmicOrderCommandReply, RithmicOrderDetails, RithmicOrderDuration,
        RithmicOrderNotification, RithmicOrderNotifyType, RithmicOrderPlacement, RithmicOrderSide,
        RithmicOrderType, RithmicReportedPriceType, RithmicReportedSide, RithmicRequestCompletion,
        RithmicTradeRoute, RithmicUserType,
    };
    use crate::{
        ProtocolError, RithmicRequestKind, RithmicRequestOutcome,
        generated::rti,
        plant_common::wire::{
            ResponseFrame, account_ref, bound_frame, optional_count, optional_price,
            optional_string, optional_timestamp, optional_wide_count, required_string,
            response_frame, terminal_outcome, validate_account, validate_string,
        },
    };
    use prost::Message;

    const LOGIN_INFO_REQUEST: i32 = 300;
    const LOGIN_INFO_RESPONSE: i32 = 301;
    const ACCOUNT_LIST_REQUEST: i32 = 302;
    const ACCOUNT_LIST_RESPONSE: i32 = 303;
    const ORDER_UPDATES_REQUEST: i32 = 308;
    const ORDER_UPDATES_RESPONSE: i32 = 309;
    const TRADE_ROUTES_REQUEST: i32 = 310;
    const TRADE_ROUTES_RESPONSE: i32 = 311;
    const NEW_ORDER_REQUEST: i32 = 312;
    const NEW_ORDER_RESPONSE: i32 = 313;
    const MODIFY_ORDER_REQUEST: i32 = 314;
    const MODIFY_ORDER_RESPONSE: i32 = 315;
    const CANCEL_ORDER_REQUEST: i32 = 316;
    const CANCEL_ORDER_RESPONSE: i32 = 317;
    const SHOW_ORDERS_REQUEST: i32 = 320;
    const SHOW_ORDERS_RESPONSE: i32 = 321;
    const CANCEL_ALL_ORDERS_REQUEST: i32 = 346;
    const CANCEL_ALL_ORDERS_RESPONSE: i32 = 347;
    const TRADE_ROUTE_UPDATE: i32 = 350;
    const RITHMIC_ORDER_NOTIFICATION: i32 = 351;
    const EXCHANGE_ORDER_NOTIFICATION: i32 = 352;
    const REPLAY_EXECUTIONS_REQUEST: i32 = 3506;
    const REPLAY_EXECUTIONS_RESPONSE: i32 = 3507;
    const SHOW_FILL_HISTORY_REQUEST: i32 = 3512;
    const SHOW_FILL_HISTORY_RESPONSE: i32 = 3513;
    const FILL_HISTORY_INDEX_FORMAT: &str = "ssboe";

    pub(crate) const OUTBOUND_TEMPLATES: &[i32] = &[
        LOGIN_INFO_REQUEST,
        ACCOUNT_LIST_REQUEST,
        ORDER_UPDATES_REQUEST,
        TRADE_ROUTES_REQUEST,
        NEW_ORDER_REQUEST,
        MODIFY_ORDER_REQUEST,
        CANCEL_ORDER_REQUEST,
        SHOW_ORDERS_REQUEST,
        CANCEL_ALL_ORDERS_REQUEST,
        REPLAY_EXECUTIONS_REQUEST,
        SHOW_FILL_HISTORY_REQUEST,
    ];

    pub(crate) fn encode(request: OrderPlantRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        match request {
            OrderPlantRequest::LoginInfo => Ok(rti::RequestLoginInfo {
                template_id: LOGIN_INFO_REQUEST,
                user_msg: Vec::new(),
            }
            .encode_to_vec()),
            OrderPlantRequest::AccountList(request) => encode_account_list(request),
            OrderPlantRequest::SubscribeOrderUpdates(account) => {
                validate_account(account)?;
                Ok(rti::RequestSubscribeForOrderUpdates {
                    template_id: ORDER_UPDATES_REQUEST,
                    user_msg: Vec::new(),
                    fcm_id: Some(account.fcm_id.to_string()),
                    ib_id: Some(account.ib_id.to_string()),
                    account_id: Some(account.account_id.to_string()),
                }
                .encode_to_vec())
            }
            OrderPlantRequest::TradeRoutes(request) => Ok(rti::RequestTradeRoutes {
                template_id: TRADE_ROUTES_REQUEST,
                user_msg: Vec::new(),
                subscribe_for_updates: Some(request.subscribe_for_updates),
            }
            .encode_to_vec()),
            OrderPlantRequest::NewOrder(request) => encode_new_order(request),
            OrderPlantRequest::ModifyOrder(request) => encode_modify_order(request),
            OrderPlantRequest::CancelOrder(request) => encode_cancel_order(request),
            OrderPlantRequest::CancelAllOrders(request) => encode_cancel_all_orders(request),
            OrderPlantRequest::ShowOrders(account) => {
                validate_account(account)?;
                Ok(rti::RequestShowOrders {
                    template_id: SHOW_ORDERS_REQUEST,
                    user_msg: Vec::new(),
                    fcm_id: Some(account.fcm_id.to_string()),
                    ib_id: Some(account.ib_id.to_string()),
                    account_id: Some(account.account_id.to_string()),
                }
                .encode_to_vec())
            }
            OrderPlantRequest::ReplayExecutions(request) => encode_replay_executions(request),
            OrderPlantRequest::ShowFillHistory(request) => encode_fill_history(request),
        }
    }

    fn encode_account_list(request: AccountListRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        for (field, value) in [("fcm_id", request.fcm_id), ("ib_id", request.ib_id)] {
            if let Some(value) = value {
                validate_string(field, value)?;
            }
        }
        Ok(rti::RequestAccountList {
            template_id: ACCOUNT_LIST_REQUEST,
            user_msg: Vec::new(),
            fcm_id: request.fcm_id.map(str::to_string),
            ib_id: request.ib_id.map(str::to_string),
            user_type: Some(rti::request_account_list::UserType::Trader.into()),
        }
        .encode_to_vec())
    }

    /// Wire fields shared by new and modify commands.
    struct OrderTypeFields {
        code: OrderTypeCode,
        price: Option<f64>,
        trigger_price: Option<f64>,
    }

    #[derive(Clone, Copy)]
    enum OrderTypeCode {
        Limit,
        Market,
        StopLimit,
        StopMarket,
    }

    fn order_type_fields(order_type: RithmicOrderType) -> Result<OrderTypeFields, ProtocolError> {
        let price = |value: crate::RithmicDecimal| value.to_wire_f64("order.price");
        let trigger = |value: crate::RithmicDecimal| value.to_wire_f64("order.trigger_price");
        Ok(match order_type {
            RithmicOrderType::Market => OrderTypeFields {
                code: OrderTypeCode::Market,
                price: None,
                trigger_price: None,
            },
            RithmicOrderType::Limit { price: value } => OrderTypeFields {
                code: OrderTypeCode::Limit,
                price: Some(price(value)?),
                trigger_price: None,
            },
            RithmicOrderType::StopMarket { trigger: value } => OrderTypeFields {
                code: OrderTypeCode::StopMarket,
                price: None,
                trigger_price: Some(trigger(value)?),
            },
            RithmicOrderType::StopLimit {
                price: limit,
                trigger: stop,
            } => OrderTypeFields {
                code: OrderTypeCode::StopLimit,
                price: Some(price(limit)?),
                trigger_price: Some(trigger(stop)?),
            },
        })
    }

    fn wire_quantity(quantity: u32) -> Result<i32, ProtocolError> {
        i32::try_from(quantity)
            .ok()
            .filter(|quantity| *quantity > 0)
            .ok_or(ProtocolError::InvalidQuantity)
    }

    fn new_order_placement(
        placement: RithmicOrderPlacement,
    ) -> rti::request_new_order::OrderPlacement {
        match placement {
            RithmicOrderPlacement::Manual => rti::request_new_order::OrderPlacement::Manual,
            RithmicOrderPlacement::Automated => rti::request_new_order::OrderPlacement::Auto,
        }
    }

    fn encode_new_order(request: NewOrderRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_account(request.account)?;
        validate_string("user_tag", request.user_tag)?;
        validate_string("symbol", request.symbol)?;
        validate_string("exchange", request.exchange)?;
        validate_string("trade_route", request.trade_route)?;
        let quantity = wire_quantity(request.quantity)?;
        let fields = order_type_fields(request.order_type)?;
        let price_type = match fields.code {
            OrderTypeCode::Limit => rti::request_new_order::PriceType::Limit,
            OrderTypeCode::Market => rti::request_new_order::PriceType::Market,
            OrderTypeCode::StopLimit => rti::request_new_order::PriceType::StopLimit,
            OrderTypeCode::StopMarket => rti::request_new_order::PriceType::StopMarket,
        };
        let transaction_type = match request.side {
            RithmicOrderSide::Buy => rti::request_new_order::TransactionType::Buy,
            RithmicOrderSide::Sell => rti::request_new_order::TransactionType::Sell,
        };
        let duration = match request.duration {
            RithmicOrderDuration::Day => rti::request_new_order::Duration::Day,
            RithmicOrderDuration::GoodTillCancelled => rti::request_new_order::Duration::Gtc,
            RithmicOrderDuration::ImmediateOrCancel => rti::request_new_order::Duration::Ioc,
            RithmicOrderDuration::FillOrKill => rti::request_new_order::Duration::Fok,
        };
        Ok(rti::RequestNewOrder {
            template_id: NEW_ORDER_REQUEST,
            user_tag: Some(request.user_tag.to_string()),
            fcm_id: Some(request.account.fcm_id.to_string()),
            ib_id: Some(request.account.ib_id.to_string()),
            account_id: Some(request.account.account_id.to_string()),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            quantity: Some(quantity),
            price: fields.price,
            trigger_price: fields.trigger_price,
            transaction_type: Some(transaction_type.into()),
            duration: Some(duration.into()),
            price_type: Some(price_type.into()),
            trade_route: Some(request.trade_route.to_string()),
            manual_or_auto: Some(new_order_placement(request.placement).into()),
            ..Default::default()
        }
        .encode_to_vec())
    }

    fn encode_modify_order(request: ModifyOrderRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_account(request.account)?;
        validate_string("basket_id", request.basket_id)?;
        validate_string("symbol", request.symbol)?;
        validate_string("exchange", request.exchange)?;
        let quantity = wire_quantity(request.quantity)?;
        let fields = order_type_fields(request.order_type)?;
        let price_type = match fields.code {
            OrderTypeCode::Limit => rti::request_modify_order::PriceType::Limit,
            OrderTypeCode::Market => rti::request_modify_order::PriceType::Market,
            OrderTypeCode::StopLimit => rti::request_modify_order::PriceType::StopLimit,
            OrderTypeCode::StopMarket => rti::request_modify_order::PriceType::StopMarket,
        };
        let placement = match request.placement {
            RithmicOrderPlacement::Manual => rti::request_modify_order::OrderPlacement::Manual,
            RithmicOrderPlacement::Automated => rti::request_modify_order::OrderPlacement::Auto,
        };
        Ok(rti::RequestModifyOrder {
            template_id: MODIFY_ORDER_REQUEST,
            fcm_id: Some(request.account.fcm_id.to_string()),
            ib_id: Some(request.account.ib_id.to_string()),
            account_id: Some(request.account.account_id.to_string()),
            basket_id: Some(request.basket_id.to_string()),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            quantity: Some(quantity),
            price: fields.price,
            trigger_price: fields.trigger_price,
            price_type: Some(price_type.into()),
            manual_or_auto: Some(placement.into()),
            ..Default::default()
        }
        .encode_to_vec())
    }

    fn encode_cancel_order(request: CancelOrderRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_account(request.account)?;
        validate_string("basket_id", request.basket_id)?;
        let placement = match request.placement {
            RithmicOrderPlacement::Manual => rti::request_cancel_order::OrderPlacement::Manual,
            RithmicOrderPlacement::Automated => rti::request_cancel_order::OrderPlacement::Auto,
        };
        Ok(rti::RequestCancelOrder {
            template_id: CANCEL_ORDER_REQUEST,
            user_msg: Vec::new(),
            window_name: None,
            fcm_id: Some(request.account.fcm_id.to_string()),
            ib_id: Some(request.account.ib_id.to_string()),
            account_id: Some(request.account.account_id.to_string()),
            basket_id: Some(request.basket_id.to_string()),
            manual_or_auto: Some(placement.into()),
        }
        .encode_to_vec())
    }

    fn encode_cancel_all_orders(
        request: CancelAllOrdersRequest<'_>,
    ) -> Result<Vec<u8>, ProtocolError> {
        validate_account(request.account)?;
        let placement = match request.placement {
            RithmicOrderPlacement::Manual => rti::request_cancel_all_orders::OrderPlacement::Manual,
            RithmicOrderPlacement::Automated => {
                rti::request_cancel_all_orders::OrderPlacement::Auto
            }
        };
        Ok(rti::RequestCancelAllOrders {
            template_id: CANCEL_ALL_ORDERS_REQUEST,
            user_msg: Vec::new(),
            fcm_id: Some(request.account.fcm_id.to_string()),
            ib_id: Some(request.account.ib_id.to_string()),
            account_id: Some(request.account.account_id.to_string()),
            user_type: None,
            manual_or_auto: Some(placement.into()),
        }
        .encode_to_vec())
    }

    fn validate_history_range(
        start_seconds: i32,
        finish_seconds: i32,
    ) -> Result<(), ProtocolError> {
        if start_seconds < 0 || start_seconds > finish_seconds {
            return Err(ProtocolError::InvalidRange);
        }
        Ok(())
    }

    fn encode_replay_executions(
        request: ExecutionReplayRequest<'_>,
    ) -> Result<Vec<u8>, ProtocolError> {
        validate_account(request.account)?;
        validate_history_range(request.start_seconds, request.finish_seconds)?;
        Ok(rti::RequestReplayExecutions {
            template_id: REPLAY_EXECUTIONS_REQUEST,
            user_msg: Vec::new(),
            fcm_id: Some(request.account.fcm_id.to_string()),
            ib_id: Some(request.account.ib_id.to_string()),
            account_id: Some(request.account.account_id.to_string()),
            start_index: Some(request.start_seconds),
            finish_index: Some(request.finish_seconds),
        }
        .encode_to_vec())
    }

    fn encode_fill_history(request: FillHistoryRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_account(request.account)?;
        validate_history_range(request.start_seconds, request.finish_seconds)?;
        if i64::from(request.finish_seconds) - i64::from(request.start_seconds)
            > i64::from(MAXIMUM_FILL_HISTORY_WINDOW_SECONDS)
        {
            return Err(ProtocolError::RangeTooLong {
                maximum_seconds: MAXIMUM_FILL_HISTORY_WINDOW_SECONDS,
            });
        }
        if request
            .maximum_records
            .is_some_and(|count| count == 0 || count > MAXIMUM_FILL_HISTORY_RECORDS)
        {
            return Err(ProtocolError::InvalidMaximumBars);
        }
        Ok(rti::RequestShowFillHistory {
            template_id: SHOW_FILL_HISTORY_REQUEST,
            user_msg: Vec::new(),
            fcm_id: Some(request.account.fcm_id.to_string()),
            ib_id: Some(request.account.ib_id.to_string()),
            account_id: Some(request.account.account_id.to_string()),
            index_format: Some(FILL_HISTORY_INDEX_FORMAT.to_string()),
            start_index: Some(request.start_seconds),
            finish_index: Some(request.finish_seconds),
            max_record_count: request.maximum_records.map(i32::from),
        }
        .encode_to_vec())
    }

    pub(crate) fn decode(frame: &[u8]) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        bound_frame(frame)?;
        let template = rti::MessageType::decode(frame)
            .map_err(|_| ProtocolError::Decode)?
            .template_id;
        match template {
            LOGIN_INFO_RESPONSE => decode_login_info(frame).map(Some),
            ACCOUNT_LIST_RESPONSE => decode_account_list(frame),
            ORDER_UPDATES_RESPONSE => terminal_reply(
                frame,
                RithmicRequestKind::OrderUpdates,
                |message: &rti::ResponseSubscribeForOrderUpdates| {
                    (&message.user_msg, &message.rp_code)
                },
            ),
            TRADE_ROUTES_RESPONSE => decode_trade_routes(frame),
            TRADE_ROUTE_UPDATE => {
                let message = rti::TradeRoute::decode(frame).map_err(|_| ProtocolError::Decode)?;
                Ok(Some(DecodedOrderMessage::TradeRouteUpdate(trade_route(
                    message.fcm_id,
                    message.ib_id,
                    message.exchange,
                    message.trade_route,
                    message.status,
                    message.is_default,
                )?)))
            }
            NEW_ORDER_RESPONSE => decode_new_order_reply(frame),
            MODIFY_ORDER_RESPONSE => decode_modify_order_reply(frame),
            CANCEL_ORDER_RESPONSE => decode_cancel_order_reply(frame),
            SHOW_ORDERS_RESPONSE => terminal_reply(
                frame,
                RithmicRequestKind::ShowOrders,
                |message: &rti::ResponseShowOrders| (&message.user_msg, &message.rp_code),
            ),
            CANCEL_ALL_ORDERS_RESPONSE => terminal_reply(
                frame,
                RithmicRequestKind::CancelAllOrders,
                |message: &rti::ResponseCancelAllOrders| (&message.user_msg, &message.rp_code),
            ),
            RITHMIC_ORDER_NOTIFICATION => decode_rithmic_order(frame).map(Some),
            EXCHANGE_ORDER_NOTIFICATION => decode_exchange_order(frame).map(Some),
            REPLAY_EXECUTIONS_RESPONSE => terminal_reply(
                frame,
                RithmicRequestKind::ReplayExecutions,
                |message: &rti::ResponseReplayExecutions| (&message.user_msg, &message.rp_code),
            ),
            SHOW_FILL_HISTORY_RESPONSE => decode_fill_history(frame),
            template => Err(ProtocolError::UnsupportedTemplate(template)),
        }
    }

    /// Decodes a reply that only carries a terminal `rp_code` for `request`.
    fn terminal_reply<M: Message + Default>(
        frame: &[u8],
        request: RithmicRequestKind,
        codes: impl FnOnce(&M) -> (&Vec<String>, &Vec<String>),
    ) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let message = M::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let (user_messages, terminal_codes) = codes(&message);
        Ok(Some(completion(
            request,
            terminal_outcome(user_messages, terminal_codes)?,
        )))
    }

    const fn completion(
        request: RithmicRequestKind,
        outcome: RithmicRequestOutcome,
    ) -> DecodedOrderMessage {
        DecodedOrderMessage::RequestComplete(RithmicRequestCompletion {
            request,
            user_tag: None,
            basket_id: None,
            outcome,
        })
    }

    fn decode_new_order_reply(frame: &[u8]) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let message = rti::ResponseNewOrder::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let reply = CommandReply {
            request: RithmicRequestKind::NewOrder,
            user_tag: message.user_tag,
            basket_id: message.basket_id,
            seconds: message.ssboe,
            microseconds: message.usecs,
        };
        decode_command(
            &message.user_msg,
            &message.rq_handler_rp_code,
            &message.rp_code,
            reply,
        )
    }

    fn decode_modify_order_reply(
        frame: &[u8],
    ) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let message = rti::ResponseModifyOrder::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let reply = CommandReply {
            request: RithmicRequestKind::ModifyOrder,
            user_tag: None,
            basket_id: message.basket_id,
            seconds: message.ssboe,
            microseconds: message.usecs,
        };
        decode_command(
            &message.user_msg,
            &message.rq_handler_rp_code,
            &message.rp_code,
            reply,
        )
    }

    fn decode_cancel_order_reply(
        frame: &[u8],
    ) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let message = rti::ResponseCancelOrder::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let reply = CommandReply {
            request: RithmicRequestKind::CancelOrder,
            user_tag: None,
            basket_id: message.basket_id,
            seconds: message.ssboe,
            microseconds: message.usecs,
        };
        decode_command(
            &message.user_msg,
            &message.rq_handler_rp_code,
            &message.rp_code,
            reply,
        )
    }

    fn decode_login_info(frame: &[u8]) -> Result<DecodedOrderMessage, ProtocolError> {
        let message = rti::ResponseLoginInfo::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let outcome = terminal_outcome(&message.user_msg, &message.rp_code)?;
        if !outcome.is_accepted() {
            return Ok(completion(RithmicRequestKind::LoginInfo, outcome));
        }
        let user_type = message
            .user_type
            .map(
                |value| match rti::response_login_info::UserType::try_from(value) {
                    Ok(rti::response_login_info::UserType::Admin) => Ok(RithmicUserType::Admin),
                    Ok(rti::response_login_info::UserType::Fcm) => Ok(RithmicUserType::Fcm),
                    Ok(rti::response_login_info::UserType::Ib) => Ok(RithmicUserType::Ib),
                    Ok(rti::response_login_info::UserType::Trader) => Ok(RithmicUserType::Trader),
                    Err(_) => Err(ProtocolError::UnknownEnum("login_info.user_type")),
                },
            )
            .transpose()?;
        Ok(DecodedOrderMessage::LoginInfo(RithmicLoginInfo {
            fcm_id: optional_string("login_info.fcm_id", message.fcm_id)?,
            ib_id: optional_string("login_info.ib_id", message.ib_id)?,
            user_type,
            ticker_plant_session_limit: optional_count(
                "login_info.tp_max_session_count",
                message.tp_max_session_count,
            )?,
            order_plant_session_limit: optional_count(
                "login_info.op_max_session_count",
                message.op_max_session_count,
            )?,
        }))
    }

    fn decode_account_list(frame: &[u8]) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let message = rti::ResponseAccountList::decode(frame).map_err(|_| ProtocolError::Decode)?;
        match response_frame(
            &message.user_msg,
            &message.rq_handler_rp_code,
            &message.rp_code,
        )? {
            ResponseFrame::Terminal(outcome) => {
                Ok(Some(completion(RithmicRequestKind::AccountList, outcome)))
            }
            ResponseFrame::Handler(RithmicRequestOutcome::NoData)
                if message.account_id.is_none() =>
            {
                Ok(None)
            }
            ResponseFrame::Handler(RithmicRequestOutcome::Accepted) => {
                Ok(Some(DecodedOrderMessage::Account(RithmicAccount {
                    fcm_id: required_string("account.fcm_id", message.fcm_id)?,
                    ib_id: required_string("account.ib_id", message.ib_id)?,
                    account_id: account_ref("account.account_id", message.account_id)?,
                    account_name: optional_string("account.account_name", message.account_name)?,
                    currency: optional_string(
                        "account.account_currency",
                        message.account_currency,
                    )?,
                })))
            }
            ResponseFrame::Handler(_) => Err(ProtocolError::RejectedDataFrame),
        }
    }

    fn decode_trade_routes(frame: &[u8]) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let message = rti::ResponseTradeRoutes::decode(frame).map_err(|_| ProtocolError::Decode)?;
        match response_frame(
            &message.user_msg,
            &message.rq_handler_rp_code,
            &message.rp_code,
        )? {
            ResponseFrame::Terminal(outcome) => {
                Ok(Some(completion(RithmicRequestKind::TradeRoutes, outcome)))
            }
            ResponseFrame::Handler(RithmicRequestOutcome::NoData)
                if message.trade_route.is_none() =>
            {
                Ok(None)
            }
            ResponseFrame::Handler(RithmicRequestOutcome::Accepted) => {
                Ok(Some(DecodedOrderMessage::TradeRoute(trade_route(
                    message.fcm_id,
                    message.ib_id,
                    message.exchange,
                    message.trade_route,
                    message.status,
                    message.is_default,
                )?)))
            }
            ResponseFrame::Handler(_) => Err(ProtocolError::RejectedDataFrame),
        }
    }

    fn trade_route(
        fcm_id: Option<String>,
        ib_id: Option<String>,
        exchange: Option<String>,
        route: Option<String>,
        status: Option<String>,
        is_default: Option<bool>,
    ) -> Result<RithmicTradeRoute, ProtocolError> {
        Ok(RithmicTradeRoute {
            fcm_id: required_string("trade_route.fcm_id", fcm_id)?,
            ib_id: required_string("trade_route.ib_id", ib_id)?,
            exchange: required_string("trade_route.exchange", exchange)?,
            trade_route: required_string("trade_route.trade_route", route)?,
            status: optional_string("trade_route.status", status)?,
            is_default,
        })
    }

    struct CommandReply {
        request: RithmicRequestKind,
        user_tag: Option<String>,
        basket_id: Option<String>,
        seconds: Option<i32>,
        microseconds: Option<i32>,
    }

    fn decode_command(
        user_messages: &[String],
        handler_codes: &[String],
        terminal_codes: &[String],
        reply: CommandReply,
    ) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let user_tag = optional_string("order_command.user_tag", reply.user_tag)?;
        let basket_id = optional_string("order_command.basket_id", reply.basket_id)?;
        let timestamp = optional_timestamp(reply.seconds, reply.microseconds)?;
        match response_frame(user_messages, handler_codes, terminal_codes)? {
            ResponseFrame::Handler(outcome) => {
                if outcome.is_accepted() && basket_id.is_none() {
                    return Err(ProtocolError::MissingField("order_command.basket_id"));
                }
                Ok(Some(DecodedOrderMessage::OrderCommand(
                    RithmicOrderCommandReply {
                        request: reply.request,
                        user_tag,
                        basket_id,
                        outcome,
                        timestamp,
                    },
                )))
            }
            ResponseFrame::Terminal(outcome) => Ok(Some(DecodedOrderMessage::RequestComplete(
                RithmicRequestCompletion {
                    request: reply.request,
                    user_tag,
                    basket_id,
                    outcome,
                },
            ))),
        }
    }

    /// Wire fields common to templates 351 and 352, moved out of either message.
    struct OrderWire {
        user_tag: Option<String>,
        is_snapshot: Option<bool>,
        basket_id: Option<String>,
        account_id: Option<String>,
        symbol: Option<String>,
        exchange: Option<String>,
        trade_route: Option<String>,
        exchange_order_id: Option<String>,
        status: Option<String>,
        side: Option<RithmicReportedSide>,
        quantity: Option<i32>,
        price: Option<f64>,
        trigger_price: Option<f64>,
        price_type: Option<RithmicReportedPriceType>,
        duration: Option<RithmicOrderDuration>,
        placement: Option<RithmicOrderPlacement>,
        average_fill_price: Option<f64>,
        total_fill_size: Option<i32>,
        total_unfilled_size: Option<i32>,
        text: Option<String>,
        seconds: Option<i32>,
        microseconds: Option<i32>,
    }

    fn order_details(wire: OrderWire) -> Result<RithmicOrderDetails, ProtocolError> {
        Ok(RithmicOrderDetails {
            user_tag: optional_string("order.user_tag", wire.user_tag)?,
            // `is_snapshot` is only set on snapshot replays; absence marks a live change.
            is_snapshot: wire.is_snapshot == Some(true),
            basket_id: required_string("order.basket_id", wire.basket_id)?,
            account_id: account_ref("order.account_id", wire.account_id)?,
            symbol: required_string("order.symbol", wire.symbol)?,
            exchange: required_string("order.exchange", wire.exchange)?,
            trade_route: optional_string("order.trade_route", wire.trade_route)?,
            exchange_order_id: optional_string("order.exchange_order_id", wire.exchange_order_id)?,
            status: optional_string("order.status", wire.status)?,
            side: wire.side,
            quantity: optional_count("order.quantity", wire.quantity)?,
            price: optional_price("order.price", wire.price)?,
            trigger_price: optional_price("order.trigger_price", wire.trigger_price)?,
            price_type: wire.price_type,
            duration: wire.duration,
            placement: wire.placement,
            average_fill_price: optional_price("order.avg_fill_price", wire.average_fill_price)?,
            total_fill_size: optional_count("order.total_fill_size", wire.total_fill_size)?,
            total_unfilled_size: optional_count(
                "order.total_unfilled_size",
                wire.total_unfilled_size,
            )?,
            text: optional_string("order.text", wire.text)?,
            timestamp: optional_timestamp(wire.seconds, wire.microseconds)?,
        })
    }

    macro_rules! reported_enums {
        ($module:ident, $prefix:literal) => {
            pub(super) fn side(
                value: Option<i32>,
            ) -> Result<Option<RithmicReportedSide>, ProtocolError> {
                value
                    .map(
                        |value| match rti::$module::TransactionType::try_from(value) {
                            Ok(rti::$module::TransactionType::Buy) => Ok(RithmicReportedSide::Buy),
                            Ok(rti::$module::TransactionType::Sell) => {
                                Ok(RithmicReportedSide::Sell)
                            }
                            Ok(rti::$module::TransactionType::Ss) => {
                                Ok(RithmicReportedSide::SellShort)
                            }
                            Err(_) => Err(ProtocolError::UnknownEnum(concat!(
                                $prefix,
                                ".transaction_type"
                            ))),
                        },
                    )
                    .transpose()
            }

            pub(super) fn price_type(
                value: Option<i32>,
            ) -> Result<Option<RithmicReportedPriceType>, ProtocolError> {
                value
                    .map(|value| match rti::$module::PriceType::try_from(value) {
                        Ok(rti::$module::PriceType::Limit) => Ok(RithmicReportedPriceType::Limit),
                        Ok(rti::$module::PriceType::Market) => Ok(RithmicReportedPriceType::Market),
                        Ok(rti::$module::PriceType::StopLimit) => {
                            Ok(RithmicReportedPriceType::StopLimit)
                        }
                        Ok(rti::$module::PriceType::StopMarket) => {
                            Ok(RithmicReportedPriceType::StopMarket)
                        }
                        Err(_) => Err(ProtocolError::UnknownEnum(concat!($prefix, ".price_type"))),
                    })
                    .transpose()
            }

            pub(super) fn duration(
                value: Option<i32>,
            ) -> Result<Option<RithmicOrderDuration>, ProtocolError> {
                value
                    .map(|value| match rti::$module::Duration::try_from(value) {
                        Ok(rti::$module::Duration::Day) => Ok(RithmicOrderDuration::Day),
                        Ok(rti::$module::Duration::Gtc) => {
                            Ok(RithmicOrderDuration::GoodTillCancelled)
                        }
                        Ok(rti::$module::Duration::Ioc) => {
                            Ok(RithmicOrderDuration::ImmediateOrCancel)
                        }
                        Ok(rti::$module::Duration::Fok) => Ok(RithmicOrderDuration::FillOrKill),
                        Err(_) => Err(ProtocolError::UnknownEnum(concat!($prefix, ".duration"))),
                    })
                    .transpose()
            }

            pub(super) fn placement(
                value: Option<i32>,
            ) -> Result<Option<RithmicOrderPlacement>, ProtocolError> {
                value
                    .map(
                        |value| match rti::$module::OrderPlacement::try_from(value) {
                            Ok(rti::$module::OrderPlacement::Manual) => {
                                Ok(RithmicOrderPlacement::Manual)
                            }
                            Ok(rti::$module::OrderPlacement::Auto) => {
                                Ok(RithmicOrderPlacement::Automated)
                            }
                            Err(_) => Err(ProtocolError::UnknownEnum(concat!(
                                $prefix,
                                ".manual_or_auto"
                            ))),
                        },
                    )
                    .transpose()
            }
        };
    }

    mod rithmic_order_enums {
        use super::{
            ProtocolError, RithmicOrderDuration, RithmicOrderPlacement, RithmicReportedPriceType,
            RithmicReportedSide, rti,
        };
        reported_enums!(rithmic_order_notification, "rithmic_order_notification");
    }

    mod exchange_order_enums {
        use super::{
            ProtocolError, RithmicOrderDuration, RithmicOrderPlacement, RithmicReportedPriceType,
            RithmicReportedSide, rti,
        };
        reported_enums!(exchange_order_notification, "exchange_order_notification");
    }

    fn decode_rithmic_order(frame: &[u8]) -> Result<DecodedOrderMessage, ProtocolError> {
        use rithmic_order_enums::{duration, placement, price_type, side};
        use rti::rithmic_order_notification::NotifyType;

        let message =
            rti::RithmicOrderNotification::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let notify_type = match message.notify_type.map(NotifyType::try_from) {
            None => {
                return Err(ProtocolError::MissingField(
                    "rithmic_order_notification.notify_type",
                ));
            }
            Some(Err(_)) => {
                return Err(ProtocolError::UnknownEnum(
                    "rithmic_order_notification.notify_type",
                ));
            }
            Some(Ok(value)) => match value {
                NotifyType::OrderRcvdFromClnt => RithmicOrderNotifyType::OrderReceivedFromClient,
                NotifyType::ModifyRcvdFromClnt => RithmicOrderNotifyType::ModifyReceivedFromClient,
                NotifyType::CancelRcvdFromClnt => RithmicOrderNotifyType::CancelReceivedFromClient,
                NotifyType::OpenPending => RithmicOrderNotifyType::OpenPending,
                NotifyType::ModifyPending => RithmicOrderNotifyType::ModifyPending,
                NotifyType::CancelPending => RithmicOrderNotifyType::CancelPending,
                NotifyType::OrderRcvdByExchGtwy => {
                    RithmicOrderNotifyType::OrderReceivedByExchangeGateway
                }
                NotifyType::ModifyRcvdByExchGtwy => {
                    RithmicOrderNotifyType::ModifyReceivedByExchangeGateway
                }
                NotifyType::CancelRcvdByExchGtwy => {
                    RithmicOrderNotifyType::CancelReceivedByExchangeGateway
                }
                NotifyType::OrderSentToExch => RithmicOrderNotifyType::OrderSentToExchange,
                NotifyType::ModifySentToExch => RithmicOrderNotifyType::ModifySentToExchange,
                NotifyType::CancelSentToExch => RithmicOrderNotifyType::CancelSentToExchange,
                NotifyType::Open => RithmicOrderNotifyType::Open,
                NotifyType::Modified => RithmicOrderNotifyType::Modified,
                NotifyType::Complete => RithmicOrderNotifyType::Complete,
                NotifyType::ModificationFailed => RithmicOrderNotifyType::ModificationFailed,
                NotifyType::CancellationFailed => RithmicOrderNotifyType::CancellationFailed,
                NotifyType::TriggerPending => RithmicOrderNotifyType::TriggerPending,
                NotifyType::Generic => RithmicOrderNotifyType::Generic,
                NotifyType::LinkOrdersFailed => RithmicOrderNotifyType::LinkOrdersFailed,
            },
        };
        let completion_reason = optional_string(
            "rithmic_order_notification.completion_reason",
            message.completion_reason,
        )?;
        let order = order_details(OrderWire {
            user_tag: message.user_tag,
            is_snapshot: message.is_snapshot,
            basket_id: message.basket_id,
            account_id: message.account_id,
            symbol: message.symbol,
            exchange: message.exchange,
            trade_route: message.trade_route,
            exchange_order_id: message.exchange_order_id,
            status: message.status,
            side: side(message.transaction_type)?,
            quantity: message.quantity,
            price: message.price,
            trigger_price: message.trigger_price,
            price_type: price_type(message.price_type)?,
            duration: duration(message.duration)?,
            placement: placement(message.manual_or_auto)?,
            average_fill_price: message.avg_fill_price,
            total_fill_size: message.total_fill_size,
            total_unfilled_size: message.total_unfilled_size,
            text: message.text,
            seconds: message.ssboe,
            microseconds: message.usecs,
        })?;
        Ok(DecodedOrderMessage::OrderNotification(
            RithmicOrderNotification {
                notify_type,
                order,
                completion_reason,
            },
        ))
    }

    fn decode_exchange_order(frame: &[u8]) -> Result<DecodedOrderMessage, ProtocolError> {
        use exchange_order_enums::{duration, placement, price_type, side};
        use rti::exchange_order_notification::NotifyType;

        let message =
            rti::ExchangeOrderNotification::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let notify_type = match message.notify_type.map(NotifyType::try_from) {
            None => {
                return Err(ProtocolError::MissingField(
                    "exchange_order_notification.notify_type",
                ));
            }
            Some(Err(_)) => {
                return Err(ProtocolError::UnknownEnum(
                    "exchange_order_notification.notify_type",
                ));
            }
            Some(Ok(value)) => match value {
                NotifyType::Status => RithmicExchangeNotifyType::Status,
                NotifyType::Modify => RithmicExchangeNotifyType::Modify,
                NotifyType::Cancel => RithmicExchangeNotifyType::Cancel,
                NotifyType::Trigger => RithmicExchangeNotifyType::Trigger,
                NotifyType::Fill => RithmicExchangeNotifyType::Fill,
                NotifyType::Reject => RithmicExchangeNotifyType::Reject,
                NotifyType::NotModified => RithmicExchangeNotifyType::NotModified,
                NotifyType::NotCancelled => RithmicExchangeNotifyType::NotCancelled,
                NotifyType::Generic => RithmicExchangeNotifyType::Generic,
            },
        };
        let fill = if notify_type == RithmicExchangeNotifyType::Fill {
            let size = optional_count("exchange_order_notification.fill_size", message.fill_size)?
                .filter(|size| *size > 0)
                .ok_or(ProtocolError::MissingField(
                    "exchange_order_notification.fill_size",
                ))?;
            Some(RithmicFill {
                fill_id: required_string("exchange_order_notification.fill_id", message.fill_id)?,
                price: optional_price(
                    "exchange_order_notification.fill_price",
                    message.fill_price,
                )?
                .ok_or(ProtocolError::MissingField(
                    "exchange_order_notification.fill_price",
                ))?,
                size,
                fill_date: optional_string(
                    "exchange_order_notification.fill_date",
                    message.fill_date,
                )?,
                fill_time: optional_string(
                    "exchange_order_notification.fill_time",
                    message.fill_time,
                )?,
            })
        } else {
            None
        };
        let report_type = optional_string(
            "exchange_order_notification.report_type",
            message.report_type,
        )?;
        let order = order_details(OrderWire {
            user_tag: message.user_tag,
            is_snapshot: message.is_snapshot,
            basket_id: message.basket_id,
            account_id: message.account_id,
            symbol: message.symbol,
            exchange: message.exchange,
            trade_route: message.trade_route,
            exchange_order_id: message.exchange_order_id,
            status: message.status,
            side: side(message.transaction_type)?,
            quantity: message.quantity,
            price: message.price,
            trigger_price: message.trigger_price,
            price_type: price_type(message.price_type)?,
            duration: duration(message.duration)?,
            placement: placement(message.manual_or_auto)?,
            average_fill_price: message.avg_fill_price,
            total_fill_size: message.total_fill_size,
            total_unfilled_size: message.total_unfilled_size,
            text: message.text,
            seconds: message.ssboe,
            microseconds: message.usecs,
        })?;
        Ok(DecodedOrderMessage::ExchangeOrderNotification(
            RithmicExchangeOrderNotification {
                notify_type,
                order,
                report_type,
                fill,
            },
        ))
    }

    fn decode_fill_history(frame: &[u8]) -> Result<Option<DecodedOrderMessage>, ProtocolError> {
        let message =
            rti::ResponseShowFillHistory::decode(frame).map_err(|_| ProtocolError::Decode)?;
        match response_frame(
            &message.user_msg,
            &message.rq_handler_rp_code,
            &message.rp_code,
        )? {
            ResponseFrame::Terminal(outcome) => {
                Ok(Some(completion(RithmicRequestKind::FillHistory, outcome)))
            }
            ResponseFrame::Handler(RithmicRequestOutcome::NoData)
                if message.fill_id.is_none() && message.basket_id.is_none() =>
            {
                Ok(None)
            }
            ResponseFrame::Handler(RithmicRequestOutcome::Accepted) => {
                let fill_size = optional_wide_count("fill_history.fill_size", message.fill_size)?
                    .filter(|size| *size > 0)
                    .ok_or(ProtocolError::MissingField("fill_history.fill_size"))?;
                Ok(Some(DecodedOrderMessage::FillHistory(
                    RithmicFillHistoryRow {
                        basket_id: required_string("fill_history.basket_id", message.basket_id)?,
                        account_id: account_ref("fill_history.account_id", message.account_id)?,
                        symbol: required_string("fill_history.symbol", message.symbol)?,
                        exchange: required_string("fill_history.exchange", message.exchange)?,
                        fill_id: required_string("fill_history.fill_id", message.fill_id)?,
                        fill_price: optional_price("fill_history.fill_price", message.fill_price)?
                            .ok_or(ProtocolError::MissingField("fill_history.fill_price"))?,
                        fill_size,
                        transaction_type: optional_string(
                            "fill_history.transaction_type",
                            message.transaction_type,
                        )?,
                        fill_date: optional_string("fill_history.fill_date", message.fill_date)?,
                        fill_time: optional_string("fill_history.fill_time", message.fill_time)?,
                        timestamp: optional_timestamp(message.ssboe, message.usecs)?,
                    },
                )))
            }
            ResponseFrame::Handler(_) => Err(ProtocolError::RejectedDataFrame),
        }
    }
}

#[cfg(all(test, rithmic_kit))]
mod tests {
    use super::{
        DecodedOrderMessage, FillHistoryRequest, MAXIMUM_FILL_HISTORY_RECORDS,
        MAXIMUM_FILL_HISTORY_WINDOW_SECONDS, NewOrderRequest, OrderPlantRequest,
        RithmicExchangeNotifyType, RithmicOrderDuration, RithmicOrderNotifyType,
        RithmicOrderPlacement, RithmicOrderSide, RithmicOrderType, RithmicReportedPriceType,
        RithmicReportedSide, decode, encode,
    };
    use crate::{
        ProtocolError, RithmicAccountKey, RithmicDecimal, RithmicRequestKind,
        RithmicRequestOutcome, generated::rti,
    };
    use prost::Message as _;

    const ACCOUNT: RithmicAccountKey<'static> = RithmicAccountKey {
        fcm_id: "fixture-fcm",
        ib_id: "fixture-ib",
        account_id: "fixture-account",
    };

    fn price(units: i64, scale: u8) -> RithmicDecimal {
        RithmicDecimal::try_new(units, scale).expect("fixture decimal is valid")
    }

    fn new_order(order_type: RithmicOrderType) -> NewOrderRequest<'static> {
        NewOrderRequest {
            account: ACCOUNT,
            user_tag: "client-order-1",
            symbol: "ESM7",
            exchange: "CME",
            trade_route: "fixture-route",
            side: RithmicOrderSide::Sell,
            quantity: 2,
            duration: RithmicOrderDuration::Day,
            order_type,
            placement: RithmicOrderPlacement::Manual,
        }
    }

    fn fill_history(start: i32, finish: i32, records: Option<u16>) -> OrderPlantRequest<'static> {
        OrderPlantRequest::ShowFillHistory(FillHistoryRequest {
            account: ACCOUNT,
            start_seconds: start,
            finish_seconds: finish,
            maximum_records: records,
        })
    }

    fn notification(is_snapshot: Option<bool>) -> rti::RithmicOrderNotification {
        rti::RithmicOrderNotification {
            template_id: 351,
            user_tag: Some("client-order-1".to_string()),
            notify_type: Some(rti::rithmic_order_notification::NotifyType::Open.into()),
            is_snapshot,
            status: Some("open".to_string()),
            basket_id: Some("fixture-basket".to_string()),
            account_id: Some("fixture-account".to_string()),
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            trade_route: Some("fixture-route".to_string()),
            quantity: Some(2),
            price: Some(5_100.25),
            transaction_type: Some(rti::rithmic_order_notification::TransactionType::Sell.into()),
            duration: Some(rti::rithmic_order_notification::Duration::Day.into()),
            price_type: Some(rti::rithmic_order_notification::PriceType::Limit.into()),
            manual_or_auto: Some(rti::rithmic_order_notification::OrderPlacement::Manual.into()),
            ssboe: Some(1_800_000_000),
            usecs: Some(250),
            ..Default::default()
        }
    }

    fn exchange_fill() -> rti::ExchangeOrderNotification {
        rti::ExchangeOrderNotification {
            template_id: 352,
            user_tag: Some("client-order-1".to_string()),
            notify_type: Some(rti::exchange_order_notification::NotifyType::Fill.into()),
            basket_id: Some("fixture-basket".to_string()),
            account_id: Some("fixture-account".to_string()),
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            quantity: Some(2),
            fill_id: Some("fixture-fill".to_string()),
            fill_price: Some(5_100.5),
            fill_size: Some(1),
            total_fill_size: Some(1),
            total_unfilled_size: Some(1),
            ..Default::default()
        }
    }

    #[test]
    fn new_order_carries_exact_prices_and_client_order_id() {
        let frame = encode(OrderPlantRequest::NewOrder(new_order(
            RithmicOrderType::StopLimit {
                price: price(510_025, 2),
                trigger: price(51_000, 1),
            },
        )))
        .expect("encode stop-limit order");
        let request = rti::RequestNewOrder::decode(frame.as_slice()).expect("decode new order");
        assert_eq!(request.template_id, 312);
        assert_eq!(request.user_tag.as_deref(), Some("client-order-1"));
        assert_eq!(request.account_id.as_deref(), Some("fixture-account"));
        assert_eq!(request.trade_route.as_deref(), Some("fixture-route"));
        assert_eq!(request.quantity, Some(2));
        assert_eq!(request.price, Some(5_100.25));
        assert_eq!(request.trigger_price, Some(5_100.0));
        assert_eq!(
            request.price_type,
            Some(rti::request_new_order::PriceType::StopLimit.into())
        );
        assert_eq!(
            request.transaction_type,
            Some(rti::request_new_order::TransactionType::Sell.into())
        );
        assert_eq!(
            request.manual_or_auto,
            Some(rti::request_new_order::OrderPlacement::Manual.into())
        );

        let market = encode(OrderPlantRequest::NewOrder(new_order(
            RithmicOrderType::Market,
        )))
        .expect("encode market order");
        let market = rti::RequestNewOrder::decode(market.as_slice()).expect("decode market");
        assert_eq!(market.price, None);
        assert_eq!(market.trigger_price, None);
    }

    #[test]
    fn invalid_order_commands_are_rejected_before_encoding() {
        let mut zero = new_order(RithmicOrderType::Market);
        zero.quantity = 0;
        assert_eq!(
            encode(OrderPlantRequest::NewOrder(zero)),
            Err(ProtocolError::InvalidQuantity)
        );
        let mut oversized = new_order(RithmicOrderType::Market);
        oversized.quantity = u32::MAX;
        assert_eq!(
            encode(OrderPlantRequest::NewOrder(oversized)),
            Err(ProtocolError::InvalidQuantity)
        );
        let mut untagged = new_order(RithmicOrderType::Market);
        untagged.user_tag = "";
        assert_eq!(
            encode(OrderPlantRequest::NewOrder(untagged)),
            Err(ProtocolError::EmptyField("user_tag"))
        );
    }

    #[test]
    fn fill_history_is_bounded_to_thirty_days_and_ten_thousand_records() {
        let start = 1_800_000_000;
        let finish = start + MAXIMUM_FILL_HISTORY_WINDOW_SECONDS;
        let frame = encode(fill_history(
            start,
            finish,
            Some(MAXIMUM_FILL_HISTORY_RECORDS),
        ))
        .expect("a thirty-day window is accepted");
        let request =
            rti::RequestShowFillHistory::decode(frame.as_slice()).expect("decode fill history");
        assert_eq!(request.template_id, 3512);
        assert_eq!(request.index_format.as_deref(), Some("ssboe"));
        assert_eq!(request.start_index, Some(start));
        assert_eq!(request.finish_index, Some(finish));
        assert_eq!(request.max_record_count, Some(10_000));

        assert_eq!(
            encode(fill_history(start, finish + 1, None)),
            Err(ProtocolError::RangeTooLong {
                maximum_seconds: MAXIMUM_FILL_HISTORY_WINDOW_SECONDS
            })
        );
        assert_eq!(
            encode(fill_history(finish, start, None)),
            Err(ProtocolError::InvalidRange)
        );
        for records in [0, MAXIMUM_FILL_HISTORY_RECORDS + 1] {
            assert_eq!(
                encode(fill_history(start, finish, Some(records))),
                Err(ProtocolError::InvalidMaximumBars)
            );
        }
    }

    #[test]
    fn accepted_command_acknowledgements_require_a_basket_id() {
        let acknowledgement = |basket_id: Option<&str>, code: &[&str]| {
            rti::ResponseNewOrder {
                template_id: 313,
                user_tag: Some("client-order-1".to_string()),
                rq_handler_rp_code: code.iter().map(|code| (*code).to_string()).collect(),
                basket_id: basket_id.map(str::to_string),
                ssboe: Some(1_800_000_000),
                usecs: Some(1),
                ..Default::default()
            }
            .encode_to_vec()
        };
        assert_eq!(
            decode(&acknowledgement(None, &["0"])),
            Err(ProtocolError::MissingField("order_command.basket_id"))
        );
        let Ok(Some(DecodedOrderMessage::OrderCommand(reply))) =
            decode(&acknowledgement(Some("fixture-basket"), &["0"]))
        else {
            panic!("accepted acknowledgement decodes as a command reply");
        };
        assert_eq!(reply.request, RithmicRequestKind::NewOrder);
        assert_eq!(reply.user_tag.as_deref(), Some("client-order-1"));
        assert_eq!(reply.basket_id.as_deref(), Some("fixture-basket"));
        assert!(reply.outcome.is_accepted());

        let Ok(Some(DecodedOrderMessage::OrderCommand(rejected))) =
            decode(&acknowledgement(None, &["3", "fixture rejection"]))
        else {
            panic!("rejected acknowledgement decodes without a basket id");
        };
        assert_eq!(
            rejected.outcome,
            RithmicRequestOutcome::Rejected {
                code: 3,
                reason: Some("fixture rejection".to_string()),
            }
        );

        let terminal = rti::ResponseNewOrder {
            template_id: 313,
            user_tag: Some("client-order-1".to_string()),
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            decode(&terminal),
            Ok(Some(DecodedOrderMessage::RequestComplete(completion)))
                if completion.request == RithmicRequestKind::NewOrder
                    && completion.user_tag.as_deref() == Some("client-order-1")
        ));
    }

    #[test]
    fn order_notifications_keep_snapshot_marker_and_exact_prices() {
        let Ok(Some(DecodedOrderMessage::OrderNotification(snapshot))) =
            decode(&notification(Some(true)).encode_to_vec())
        else {
            panic!("snapshot notification decodes");
        };
        assert_eq!(snapshot.notify_type, RithmicOrderNotifyType::Open);
        assert!(snapshot.order.is_snapshot);
        assert_eq!(snapshot.order.basket_id, "fixture-basket");
        assert_eq!(snapshot.order.side, Some(RithmicReportedSide::Sell));
        assert_eq!(
            snapshot.order.price_type,
            Some(RithmicReportedPriceType::Limit)
        );
        let order_price = snapshot.order.price.expect("price is present");
        assert_eq!((order_price.units(), order_price.scale()), (510_025, 2));
        assert_eq!(
            snapshot
                .order
                .timestamp
                .map(|time| (time.seconds, time.microseconds)),
            Some((1_800_000_000, 250))
        );
        assert!(!format!("{snapshot:?}").contains("fixture-account"));

        let Ok(Some(DecodedOrderMessage::OrderNotification(live))) =
            decode(&notification(None).encode_to_vec())
        else {
            panic!("live notification decodes");
        };
        assert!(!live.order.is_snapshot);

        let mut unbasketed = notification(None);
        unbasketed.basket_id = None;
        assert_eq!(
            decode(&unbasketed.encode_to_vec()),
            Err(ProtocolError::MissingField("order.basket_id"))
        );
        let mut unknown = notification(None);
        unknown.notify_type = Some(999);
        assert_eq!(
            decode(&unknown.encode_to_vec()),
            Err(ProtocolError::UnknownEnum(
                "rithmic_order_notification.notify_type"
            ))
        );
    }

    #[test]
    fn exchange_fills_require_identity_price_and_size() {
        let Ok(Some(DecodedOrderMessage::ExchangeOrderNotification(update))) =
            decode(&exchange_fill().encode_to_vec())
        else {
            panic!("fill notification decodes");
        };
        assert_eq!(update.notify_type, RithmicExchangeNotifyType::Fill);
        let fill = update.fill.expect("fill notifications carry the execution");
        assert_eq!(fill.fill_id, "fixture-fill");
        assert_eq!(fill.size, 1);
        assert_eq!((fill.price.units(), fill.price.scale()), (51_005, 1));

        let mut unidentified = exchange_fill();
        unidentified.fill_id = None;
        assert_eq!(
            decode(&unidentified.encode_to_vec()),
            Err(ProtocolError::MissingField(
                "exchange_order_notification.fill_id"
            ))
        );
        let mut empty = exchange_fill();
        empty.fill_size = Some(0);
        assert_eq!(
            decode(&empty.encode_to_vec()),
            Err(ProtocolError::MissingField(
                "exchange_order_notification.fill_size"
            ))
        );
        let mut status = exchange_fill();
        status.notify_type = Some(rti::exchange_order_notification::NotifyType::Status.into());
        assert!(matches!(
            decode(&status.encode_to_vec()),
            Ok(Some(DecodedOrderMessage::ExchangeOrderNotification(update)))
                if update.fill.is_none()
        ));
    }

    #[test]
    fn no_data_rows_are_skipped_and_terminal_no_data_completes() {
        let empty_accounts = rti::ResponseAccountList {
            template_id: 303,
            rq_handler_rp_code: vec!["7".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(decode(&empty_accounts), Ok(None));
        let empty_fills = rti::ResponseShowFillHistory {
            template_id: 3513,
            rq_handler_rp_code: vec!["7".to_string(), "no data".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(decode(&empty_fills), Ok(None));
        let fills_complete = rti::ResponseShowFillHistory {
            template_id: 3513,
            rp_code: vec!["7".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            decode(&fills_complete),
            Ok(Some(DecodedOrderMessage::RequestComplete(completion)))
                if completion.request == RithmicRequestKind::FillHistory
                    && completion.outcome == RithmicRequestOutcome::NoData
        ));
        let ambiguous = rti::ResponseAccountList {
            template_id: 303,
            rq_handler_rp_code: vec!["0".to_string()],
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(decode(&ambiguous), Err(ProtocolError::ResponseCodeShape));
    }

    #[test]
    fn account_rows_and_fill_rows_decode_required_identity() {
        let account = rti::ResponseAccountList {
            template_id: 303,
            rq_handler_rp_code: vec!["0".to_string()],
            fcm_id: Some("fixture-fcm".to_string()),
            ib_id: Some("fixture-ib".to_string()),
            account_id: Some("fixture-account".to_string()),
            account_currency: Some("USD".to_string()),
            ..Default::default()
        }
        .encode_to_vec();
        let Ok(Some(DecodedOrderMessage::Account(row))) = decode(&account) else {
            panic!("account row decodes");
        };
        assert_eq!(row.key(), ACCOUNT);
        assert_eq!(row.currency.as_deref(), Some("USD"));

        let fill = rti::ResponseShowFillHistory {
            template_id: 3513,
            rq_handler_rp_code: vec!["0".to_string()],
            basket_id: Some("fixture-basket".to_string()),
            account_id: Some("fixture-account".to_string()),
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            fill_id: Some("fixture-fill".to_string()),
            fill_price: Some(5_100.75),
            fill_size: Some(2),
            ssboe: Some(1_800_000_000),
            usecs: Some(0),
            ..Default::default()
        };
        let Ok(Some(DecodedOrderMessage::FillHistory(row))) = decode(&fill.encode_to_vec()) else {
            panic!("fill history row decodes");
        };
        assert_eq!(row.fill_size, 2);
        assert_eq!(
            (row.fill_price.units(), row.fill_price.scale()),
            (510_075, 2)
        );
        let mut unpriced = fill;
        unpriced.fill_price = None;
        assert_eq!(
            decode(&unpriced.encode_to_vec()),
            Err(ProtocolError::MissingField("fill_history.fill_price"))
        );
    }
}
