use super::{MAXIMUM_TIMESTAMP_MS, TradeSide, wire_id};
use crate::{
    accounts::DemoAccount,
    generated::{
        ProtoOaAmendOrderReq, ProtoOaAmendPositionSltpReq, ProtoOaCancelOrderReq,
        ProtoOaClosePositionReq, ProtoOaNewOrderReq, ProtoOaReconcileReq, ProtoOaTraderReq,
    },
    market::{MarketDecodeError, PriceScale},
    transport::Bucket,
};
use prost::Message;

/// `ProtoOANewOrderReq.clientOrderId` holds at most 50 characters.
pub const MAXIMUM_CLIENT_ORDER_ID_BYTES: usize = 50;
const EXECUTION_EVENT: u32 = 2126;

/// One encoded trading request with its first expected response and rate bucket.
///
/// Order requests are answered by execution events; one request can produce several
/// (accepted, then filled), so the first response does not end the order's lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingRequest {
    pub payload_type: u32,
    pub response_type: u32,
    pub bucket: Bucket,
    pub payload: Vec<u8>,
}

/// The order's type and, for pending orders, its trigger price at symbol scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderType {
    Market,
    Limit { price: i64 },
    Stop { price: i64 },
}

/// A changed pending-order price; the variant must match the order's type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderPrice {
    Limit(i64),
    Stop(i64),
}

/// `ProtoOATimeInForce` values Aeris sends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeInForce {
    GoodTillCancel,
    GoodTillDate { expires_unix_ms: i64 },
    ImmediateOrCancel,
    FillOrKill,
}

/// A stop-loss or take-profit level: an absolute price, or a distance from the entry
/// price, both at symbol scale. Market orders accept only distances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protection {
    Price(i64),
    Distance(i64),
}

/// A new order. Volume is in cents of a unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewOrder {
    pub symbol_id: u64,
    pub side: TradeSide,
    pub order_type: OrderType,
    pub volume: u64,
    pub time_in_force: TimeInForce,
    pub stop_loss: Option<Protection>,
    pub take_profit: Option<Protection>,
    pub client_order_id: String,
    /// The position this order adds to or reduces, on a hedged account.
    pub position_id: Option<u64>,
}

/// A change to a pending order. Only the fields that are `Some` are sent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OrderAmendment {
    pub order_id: u64,
    pub volume: Option<u64>,
    pub price: Option<OrderPrice>,
    pub stop_loss: Option<Protection>,
    pub take_profit: Option<Protection>,
}

fn volume(value: u64) -> Result<i64, MarketDecodeError> {
    wire_id(value, "volume")
}

fn decimal(scale: PriceScale, price: i64) -> Result<f64, MarketDecodeError> {
    scale.to_decimal(price)
}

fn distance(scale: PriceScale, value: i64) -> Result<i64, MarketDecodeError> {
    if value <= 0 {
        return Err(MarketDecodeError::InvalidField("relative protection"));
    }
    scale.to_wire(value)
}

/// Splits one protection level into its absolute and relative wire fields.
fn protection(
    scale: PriceScale,
    level: Option<Protection>,
) -> Result<(Option<f64>, Option<i64>), MarketDecodeError> {
    match level {
        None => Ok((None, None)),
        Some(Protection::Price(price)) => Ok((Some(decimal(scale, price)?), None)),
        Some(Protection::Distance(value)) => Ok((None, Some(distance(scale, value)?))),
    }
}

fn client_order_id(value: &str) -> Result<String, MarketDecodeError> {
    if value.is_empty()
        || value.len() > MAXIMUM_CLIENT_ORDER_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(MarketDecodeError::InvalidField("clientOrderId"));
    }
    Ok(value.to_string())
}

fn time_in_force(value: TimeInForce) -> Result<(i32, Option<i64>), MarketDecodeError> {
    Ok(match value {
        TimeInForce::GoodTillDate { expires_unix_ms } => {
            if expires_unix_ms <= 0 || expires_unix_ms > MAXIMUM_TIMESTAMP_MS {
                return Err(MarketDecodeError::InvalidField("expirationTimestamp"));
            }
            (1, Some(expires_unix_ms))
        }
        TimeInForce::GoodTillCancel => (2, None),
        TimeInForce::ImmediateOrCancel => (3, None),
        TimeInForce::FillOrKill => (4, None),
    })
}

fn ctid(account: &DemoAccount) -> Result<i64, MarketDecodeError> {
    wire_id(account.account().ctid, "ctidTraderAccountId")
}

impl TradingRequest {
    pub(super) fn general(payload_type: u32, response_type: u32, message: &impl Message) -> Self {
        Self {
            payload_type,
            response_type,
            bucket: Bucket::General,
            payload: message.encode_to_vec(),
        }
    }

    /// `ProtoOANewOrderReq` (2106).
    ///
    /// # Errors
    /// Rejects a zero volume, a non-positive or inexact price, absolute protection on a
    /// market order, an invalid expiry, or a client order id that is empty, longer than
    /// 50 bytes or not printable ASCII.
    pub fn new_order(
        account: &DemoAccount,
        scale: PriceScale,
        order: &NewOrder,
    ) -> Result<Self, MarketDecodeError> {
        let (order_type, limit_price, stop_price) = match order.order_type {
            OrderType::Market => (1, None, None),
            OrderType::Limit { price } => (2, Some(decimal(scale, price)?), None),
            OrderType::Stop { price } => (3, None, Some(decimal(scale, price)?)),
        };
        if order.order_type == OrderType::Market
            && [order.stop_loss, order.take_profit]
                .iter()
                .any(|level| matches!(level, Some(Protection::Price(_))))
        {
            return Err(MarketDecodeError::InvalidField(
                "absolute protection on a market order",
            ));
        }
        let (stop_loss, relative_stop_loss) = protection(scale, order.stop_loss)?;
        let (take_profit, relative_take_profit) = protection(scale, order.take_profit)?;
        let (time_in_force, expiration_timestamp) = time_in_force(order.time_in_force)?;
        Ok(Self::general(
            2106,
            EXECUTION_EVENT,
            &ProtoOaNewOrderReq {
                payload_type: None,
                ctid_trader_account_id: ctid(account)?,
                symbol_id: wire_id(order.symbol_id, "symbolId")?,
                order_type,
                trade_side: order.side.wire(),
                volume: volume(order.volume)?,
                limit_price,
                stop_price,
                time_in_force: Some(time_in_force),
                expiration_timestamp,
                stop_loss,
                take_profit,
                comment: None,
                base_slippage_price: None,
                slippage_in_points: None,
                label: None,
                position_id: order
                    .position_id
                    .map(|id| wire_id(id, "positionId"))
                    .transpose()?,
                client_order_id: Some(client_order_id(&order.client_order_id)?),
                relative_stop_loss,
                relative_take_profit,
                guaranteed_stop_loss: None,
                trailing_stop_loss: None,
                stop_trigger_method: None,
            },
        ))
    }

    /// `ProtoOAAmendOrderReq` (2109).
    ///
    /// # Errors
    /// Rejects an amendment that changes nothing, a zero id or volume, or an invalid price.
    pub fn amend_order(
        account: &DemoAccount,
        scale: PriceScale,
        amendment: &OrderAmendment,
    ) -> Result<Self, MarketDecodeError> {
        if amendment.volume.is_none()
            && amendment.price.is_none()
            && amendment.stop_loss.is_none()
            && amendment.take_profit.is_none()
        {
            return Err(MarketDecodeError::InvalidField("empty order amendment"));
        }
        let (limit_price, stop_price) = match amendment.price {
            None => (None, None),
            Some(OrderPrice::Limit(price)) => (Some(decimal(scale, price)?), None),
            Some(OrderPrice::Stop(price)) => (None, Some(decimal(scale, price)?)),
        };
        let (stop_loss, relative_stop_loss) = protection(scale, amendment.stop_loss)?;
        let (take_profit, relative_take_profit) = protection(scale, amendment.take_profit)?;
        Ok(Self::general(
            2109,
            EXECUTION_EVENT,
            &ProtoOaAmendOrderReq {
                payload_type: None,
                ctid_trader_account_id: ctid(account)?,
                order_id: wire_id(amendment.order_id, "orderId")?,
                volume: amendment.volume.map(volume).transpose()?,
                limit_price,
                stop_price,
                expiration_timestamp: None,
                stop_loss,
                take_profit,
                slippage_in_points: None,
                relative_stop_loss,
                relative_take_profit,
                guaranteed_stop_loss: None,
                trailing_stop_loss: None,
                stop_trigger_method: None,
            },
        ))
    }

    /// `ProtoOACancelOrderReq` (2108).
    ///
    /// # Errors
    /// Rejects a zero order id.
    pub fn cancel_order(account: &DemoAccount, order_id: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2108,
            EXECUTION_EVENT,
            &ProtoOaCancelOrderReq {
                payload_type: None,
                ctid_trader_account_id: ctid(account)?,
                order_id: wire_id(order_id, "orderId")?,
            },
        ))
    }

    /// `ProtoOAClosePositionReq` (2111): close `volume` cents of a position.
    ///
    /// # Errors
    /// Rejects a zero position id or volume.
    pub fn close_position(
        account: &DemoAccount,
        position_id: u64,
        close_volume: u64,
    ) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2111,
            EXECUTION_EVENT,
            &ProtoOaClosePositionReq {
                payload_type: None,
                ctid_trader_account_id: ctid(account)?,
                position_id: wire_id(position_id, "positionId")?,
                volume: volume(close_volume)?,
            },
        ))
    }

    /// `ProtoOAAmendPositionSLTPReq` (2110). The request states the position's complete
    /// protection: a level left `None` is not kept.
    ///
    /// # Errors
    /// Rejects a zero position id or a non-positive or inexact price.
    pub fn amend_position_protection(
        account: &DemoAccount,
        scale: PriceScale,
        position_id: u64,
        stop_loss: Option<i64>,
        take_profit: Option<i64>,
    ) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2110,
            EXECUTION_EVENT,
            &ProtoOaAmendPositionSltpReq {
                payload_type: None,
                ctid_trader_account_id: ctid(account)?,
                position_id: wire_id(position_id, "positionId")?,
                stop_loss: stop_loss.map(|price| decimal(scale, price)).transpose()?,
                take_profit: take_profit.map(|price| decimal(scale, price)).transpose()?,
                guaranteed_stop_loss: None,
                trailing_stop_loss: None,
                stop_loss_trigger_method: None,
            },
        ))
    }

    /// `ProtoOAReconcileReq` (2124): open positions and pending orders. Reading state is
    /// allowed for every observed account, live ones included.
    ///
    /// # Errors
    /// Rejects a zero account id.
    pub fn reconcile(ctid: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2124,
            2125,
            &ProtoOaReconcileReq {
                payload_type: None,
                ctid_trader_account_id: wire_id(ctid, "ctidTraderAccountId")?,
                return_protection_orders: Some(false),
            },
        ))
    }

    /// `ProtoOATraderReq` (2121): balance, deposit asset and account type.
    ///
    /// # Errors
    /// Rejects a zero account id.
    pub fn trader(ctid: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2121,
            2122,
            &ProtoOaTraderReq {
                payload_type: None,
                ctid_trader_account_id: wire_id(ctid, "ctidTraderAccountId")?,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::fixtures::{CTID, CTID_WIRE};

    fn account() -> DemoAccount {
        DemoAccount::observed_for_tests(CTID)
    }

    fn five() -> PriceScale {
        PriceScale::new(5).expect("digits 5")
    }

    fn market_buy() -> NewOrder {
        NewOrder {
            symbol_id: 1,
            side: TradeSide::Buy,
            order_type: OrderType::Market,
            volume: 100_000,
            time_in_force: TimeInForce::ImmediateOrCancel,
            stop_loss: Some(Protection::Distance(200)),
            take_profit: Some(Protection::Distance(400)),
            client_order_id: "aeris-1".into(),
            position_id: None,
        }
    }

    #[test]
    fn market_orders_carry_relative_protection_in_wire_units() {
        let request = TradingRequest::new_order(&account(), five(), &market_buy()).expect("order");
        assert_eq!(
            (request.payload_type, request.response_type, request.bucket),
            (2106, 2126, Bucket::General)
        );
        let wire = ProtoOaNewOrderReq::decode(request.payload.as_slice()).expect("decode");
        assert_eq!(wire.ctid_trader_account_id, CTID_WIRE);
        assert_eq!(
            (wire.symbol_id, wire.order_type, wire.trade_side),
            (1, 1, 1)
        );
        assert_eq!(wire.volume, 100_000);
        assert_eq!(wire.time_in_force, Some(3));
        assert_eq!((wire.limit_price, wire.stop_price), (None, None));
        assert_eq!((wire.stop_loss, wire.take_profit), (None, None));
        assert_eq!(
            (wire.relative_stop_loss, wire.relative_take_profit),
            (Some(200), Some(400))
        );
        assert_eq!(wire.client_order_id.as_deref(), Some("aeris-1"));

        // A two-digit symbol's distance of 0.05 is 5000 wire units.
        let two = PriceScale::new(2).expect("digits 2");
        let request = TradingRequest::new_order(
            &account(),
            two,
            &NewOrder {
                stop_loss: Some(Protection::Distance(5)),
                take_profit: None,
                ..market_buy()
            },
        )
        .expect("order");
        let wire = ProtoOaNewOrderReq::decode(request.payload.as_slice()).expect("decode");
        assert_eq!(wire.relative_stop_loss, Some(5_000));
    }

    #[test]
    fn pending_orders_carry_exact_decimal_prices() {
        let order = NewOrder {
            order_type: OrderType::Limit { price: 108_250 },
            time_in_force: TimeInForce::GoodTillDate {
                expires_unix_ms: 1_791_500_000_000,
            },
            stop_loss: Some(Protection::Price(108_000)),
            take_profit: Some(Protection::Distance(500)),
            position_id: Some(77),
            ..market_buy()
        };
        let request = TradingRequest::new_order(&account(), five(), &order).expect("order");
        let wire = ProtoOaNewOrderReq::decode(request.payload.as_slice()).expect("decode");
        assert_eq!(wire.order_type, 2);
        assert_eq!(
            wire.limit_price.map(f64::to_bits),
            Some(1.0825_f64.to_bits())
        );
        assert_eq!(wire.stop_loss.map(f64::to_bits), Some(1.08_f64.to_bits()));
        assert_eq!(wire.relative_take_profit, Some(500));
        assert_eq!(
            (wire.time_in_force, wire.expiration_timestamp),
            (Some(1), Some(1_791_500_000_000))
        );
        assert_eq!(wire.position_id, Some(77));

        let stop = NewOrder {
            order_type: OrderType::Stop { price: 109_000 },
            ..order
        };
        let wire = ProtoOaNewOrderReq::decode(
            TradingRequest::new_order(&account(), five(), &stop)
                .expect("stop")
                .payload
                .as_slice(),
        )
        .expect("decode");
        assert_eq!((wire.order_type, wire.limit_price), (3, None));
        assert_eq!(wire.stop_price.map(f64::to_bits), Some(1.09_f64.to_bits()));
    }

    #[test]
    fn invalid_orders_are_rejected_before_encoding() {
        let invalid = [
            // Market orders do not support absolute protection.
            NewOrder {
                stop_loss: Some(Protection::Price(108_000)),
                ..market_buy()
            },
            NewOrder {
                volume: 0,
                ..market_buy()
            },
            NewOrder {
                symbol_id: 0,
                ..market_buy()
            },
            NewOrder {
                order_type: OrderType::Limit { price: 0 },
                ..market_buy()
            },
            NewOrder {
                stop_loss: Some(Protection::Distance(0)),
                ..market_buy()
            },
            NewOrder {
                time_in_force: TimeInForce::GoodTillDate { expires_unix_ms: 0 },
                ..market_buy()
            },
            NewOrder {
                client_order_id: String::new(),
                ..market_buy()
            },
            NewOrder {
                client_order_id: "x".repeat(MAXIMUM_CLIENT_ORDER_ID_BYTES + 1),
                ..market_buy()
            },
            NewOrder {
                client_order_id: "has space".into(),
                ..market_buy()
            },
        ];
        for order in invalid {
            assert!(
                TradingRequest::new_order(&account(), five(), &order).is_err(),
                "{order:?} must be rejected"
            );
        }
        // A seven-digit symbol cannot express a distance finer than the wire's five digits.
        let seven = PriceScale::new(7).expect("digits 7");
        assert!(matches!(
            TradingRequest::new_order(
                &account(),
                seven,
                &NewOrder {
                    stop_loss: Some(Protection::Distance(201)),
                    ..market_buy()
                }
            ),
            Err(MarketDecodeError::InexactPrice)
        ));
    }

    #[test]
    fn amendments_send_only_what_changes() {
        let amendment = OrderAmendment {
            order_id: 9,
            price: Some(OrderPrice::Limit(108_300)),
            ..OrderAmendment::default()
        };
        let request = TradingRequest::amend_order(&account(), five(), &amendment).expect("amend");
        assert_eq!((request.payload_type, request.response_type), (2109, 2126));
        let wire = ProtoOaAmendOrderReq::decode(request.payload.as_slice()).expect("decode");
        assert_eq!(wire.order_id, 9);
        assert_eq!(
            wire.limit_price.map(f64::to_bits),
            Some(1.083_f64.to_bits())
        );
        assert_eq!(
            (
                wire.volume,
                wire.stop_price,
                wire.stop_loss,
                wire.take_profit
            ),
            (None, None, None, None)
        );
        assert!(
            TradingRequest::amend_order(
                &account(),
                five(),
                &OrderAmendment {
                    order_id: 9,
                    ..OrderAmendment::default()
                }
            )
            .is_err(),
            "an amendment that changes nothing is not sent"
        );
    }

    #[test]
    fn cancel_close_and_position_protection_encode_their_ids() {
        let cancel = TradingRequest::cancel_order(&account(), 9).expect("cancel");
        let wire = ProtoOaCancelOrderReq::decode(cancel.payload.as_slice()).expect("decode");
        assert_eq!((cancel.payload_type, wire.order_id), (2108, 9));

        let close = TradingRequest::close_position(&account(), 77, 50_000).expect("close");
        let wire = ProtoOaClosePositionReq::decode(close.payload.as_slice()).expect("decode");
        assert_eq!(
            (close.payload_type, wire.position_id, wire.volume),
            (2111, 77, 50_000)
        );
        assert!(TradingRequest::close_position(&account(), 77, 0).is_err());

        let protect =
            TradingRequest::amend_position_protection(&account(), five(), 77, Some(108_000), None)
                .expect("protection");
        let wire = ProtoOaAmendPositionSltpReq::decode(protect.payload.as_slice()).expect("decode");
        assert_eq!(protect.payload_type, 2110);
        assert_eq!(wire.stop_loss.map(f64::to_bits), Some(1.08_f64.to_bits()));
        assert_eq!(wire.take_profit, None);
    }

    #[test]
    fn state_reads_name_the_account_and_their_responses() {
        let reconcile = TradingRequest::reconcile(CTID).expect("reconcile");
        assert_eq!(
            (reconcile.payload_type, reconcile.response_type),
            (2124, 2125)
        );
        let wire = ProtoOaReconcileReq::decode(reconcile.payload.as_slice()).expect("decode");
        assert_eq!(
            (wire.ctid_trader_account_id, wire.return_protection_orders),
            (CTID_WIRE, Some(false))
        );
        let trader = TradingRequest::trader(CTID).expect("trader");
        assert_eq!((trader.payload_type, trader.response_type), (2121, 2122));
        assert!(TradingRequest::reconcile(0).is_err());
    }
}
