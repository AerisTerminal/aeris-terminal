//! Reads that resolve orders and positions after a restart or reconnect: what happened
//! to one order, the account's orders and deals over a window, and a position's deals.

use super::{
    Deal, MAXIMUM_STATE_ITEMS, MAXIMUM_TIMESTAMP_MS, OrderState, TradingRequest,
    events::{check_account, deal, order, payload, require_deals, require_orders, timestamp},
    positive_id, wire_id,
};
use crate::{
    ProtoMessage, codec,
    generated::{
        ProtoOaDealListByPositionIdReq, ProtoOaDealListByPositionIdRes, ProtoOaDealListReq,
        ProtoOaDealListRes, ProtoOaOrderDetailsReq, ProtoOaOrderDetailsRes, ProtoOaOrderListReq,
        ProtoOaOrderListRes, ProtoOaTrailingSlChangedEvent,
    },
    market::{MarketDecodeError, PriceScale},
};

/// Deals one page may return; the server pages larger windows with `hasMore`.
const MAXIMUM_DEAL_ROWS: i32 = 1000;

/// One order and every deal that filled it (`ProtoOAOrderDetailsRes`, 2182).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderDetails {
    pub order: OrderState,
    pub deals: Vec<Deal>,
}

/// One page of orders (`ProtoOAOrderListRes`, 2176).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderPage {
    pub orders: Vec<OrderState>,
    pub has_more: bool,
}

/// One page of deals (`ProtoOADealListRes` 2134 or `ProtoOADealListByPositionIdRes` 2180).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DealPage {
    pub deals: Vec<Deal>,
    pub has_more: bool,
}

/// A trailing stop moved by the server (`ProtoOATrailingSLChangedEvent`, 2107).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrailingStopChanged {
    pub position_id: u64,
    pub order_id: u64,
    pub stop_price: i64,
    pub updated_unix_ms: i64,
}

fn window(from_ms: i64, to_ms: i64) -> Result<(i64, i64), MarketDecodeError> {
    if from_ms < 0 || to_ms <= from_ms || to_ms > MAXIMUM_TIMESTAMP_MS {
        return Err(MarketDecodeError::InvalidField("timestamp range"));
    }
    Ok((from_ms, to_ms))
}

impl TradingRequest {
    /// `ProtoOAOrderDetailsReq` (2181): one order and its deals, whatever its state.
    ///
    /// # Errors
    /// Rejects a zero account or order id.
    pub fn order_details(ctid: u64, order_id: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2181,
            2182,
            &ProtoOaOrderDetailsReq {
                payload_type: None,
                ctid_trader_account_id: wire_id(ctid, "ctidTraderAccountId")?,
                order_id: wire_id(order_id, "orderId")?,
            },
        ))
    }

    /// `ProtoOAOrderListReq` (2175): orders created in a window of Unix milliseconds.
    ///
    /// # Errors
    /// Rejects a zero account id or an empty or out-of-range window.
    pub fn order_list(ctid: u64, from_ms: i64, to_ms: i64) -> Result<Self, MarketDecodeError> {
        let (from_ms, to_ms) = window(from_ms, to_ms)?;
        Ok(Self::general(
            2175,
            2176,
            &ProtoOaOrderListReq {
                payload_type: None,
                ctid_trader_account_id: wire_id(ctid, "ctidTraderAccountId")?,
                from_timestamp: Some(from_ms),
                to_timestamp: Some(to_ms),
            },
        ))
    }

    /// `ProtoOADealListReq` (2133): deals executed in a window of Unix milliseconds.
    ///
    /// # Errors
    /// Rejects a zero account id or an empty or out-of-range window.
    pub fn deal_list(ctid: u64, from_ms: i64, to_ms: i64) -> Result<Self, MarketDecodeError> {
        let (from_ms, to_ms) = window(from_ms, to_ms)?;
        Ok(Self::general(
            2133,
            2134,
            &ProtoOaDealListReq {
                payload_type: None,
                ctid_trader_account_id: wire_id(ctid, "ctidTraderAccountId")?,
                from_timestamp: Some(from_ms),
                to_timestamp: Some(to_ms),
                max_rows: Some(MAXIMUM_DEAL_ROWS),
            },
        ))
    }

    /// `ProtoOADealListByPositionIdReq` (2179): one position's deals in a window.
    ///
    /// # Errors
    /// Rejects zero ids or an empty or out-of-range window.
    pub fn position_deals(
        ctid: u64,
        position_id: u64,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Self, MarketDecodeError> {
        let (from_ms, to_ms) = window(from_ms, to_ms)?;
        Ok(Self::general(
            2179,
            2180,
            &ProtoOaDealListByPositionIdReq {
                payload_type: None,
                ctid_trader_account_id: wire_id(ctid, "ctidTraderAccountId")?,
                position_id: wire_id(position_id, "positionId")?,
                from_timestamp: Some(from_ms),
                to_timestamp: Some(to_ms),
            },
        ))
    }
}

fn bounded<T>(items: Vec<T>) -> Result<Vec<T>, MarketDecodeError> {
    if items.len() > MAXIMUM_STATE_ITEMS {
        return Err(MarketDecodeError::LimitExceeded("trading state list"));
    }
    Ok(items)
}

fn deals(
    items: &[crate::generated::ProtoOaDeal],
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<Vec<Deal>, MarketDecodeError> {
    bounded(
        items
            .iter()
            .map(|item| deal(item, scales))
            .collect::<Result<_, _>>()?,
    )
}

/// Decode a `ProtoOAOrderDetailsRes` (2182) for one account.
///
/// # Errors
/// Rejects another account, missing required fields, or invalid orders or deals.
pub fn decode_order_details(
    frame: &ProtoMessage,
    ctid: u64,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<OrderDetails, MarketDecodeError> {
    let response: ProtoOaOrderDetailsRes = codec::decode_typed(
        frame,
        2182,
        &[(2, "ctidTraderAccountId"), (3, "order")],
        |_| Ok(()),
    )?;
    let bytes = payload(frame);
    require_orders(bytes, &[3])?;
    require_deals(bytes, &[4])?;
    check_account(ctid, response.ctid_trader_account_id)?;
    Ok(OrderDetails {
        deals: deals(&response.deal, scales)?,
        order: order(response.order, scales)?,
    })
}

/// Decode a `ProtoOAOrderListRes` (2176) for one account.
///
/// # Errors
/// Rejects another account, a missing `hasMore`, oversized pages, or invalid orders.
pub fn decode_order_list(
    frame: &ProtoMessage,
    ctid: u64,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<OrderPage, MarketDecodeError> {
    let response: ProtoOaOrderListRes = codec::decode_typed(
        frame,
        2176,
        &[(2, "ctidTraderAccountId"), (4, "hasMore")],
        |_| Ok(()),
    )?;
    require_orders(payload(frame), &[3])?;
    check_account(ctid, response.ctid_trader_account_id)?;
    Ok(OrderPage {
        orders: bounded(
            response
                .order
                .into_iter()
                .map(|item| order(item, scales))
                .collect::<Result<_, _>>()?,
        )?,
        has_more: response.has_more,
    })
}

/// Decode a deal page: `ProtoOADealListRes` (2134) or `ProtoOADealListByPositionIdRes`
/// (2180), which share one shape.
///
/// # Errors
/// Rejects another account, a missing `hasMore`, oversized pages, or invalid deals.
pub fn decode_deal_page(
    frame: &ProtoMessage,
    ctid: u64,
    scales: &impl Fn(u64) -> Option<PriceScale>,
) -> Result<DealPage, MarketDecodeError> {
    const REQUIRED: [(u32, &str); 2] = [(2, "ctidTraderAccountId"), (4, "hasMore")];
    let (account, items, has_more) = if frame.payload_type == 2180 {
        let response: ProtoOaDealListByPositionIdRes =
            codec::decode_typed(frame, 2180, &REQUIRED, |_| Ok(()))?;
        (
            response.ctid_trader_account_id,
            response.deal,
            response.has_more,
        )
    } else {
        let response: ProtoOaDealListRes = codec::decode_typed(frame, 2134, &REQUIRED, |_| Ok(()))?;
        (
            response.ctid_trader_account_id,
            response.deal,
            response.has_more,
        )
    };
    require_deals(payload(frame), &[3])?;
    check_account(ctid, account)?;
    Ok(DealPage {
        deals: deals(&items, scales)?,
        has_more,
    })
}

/// Decode a `ProtoOATrailingSLChangedEvent` (2107). The event names no symbol, so the
/// caller supplies the price scale of the position's symbol.
///
/// # Errors
/// Rejects another account, missing required fields, an unknown position, or a price
/// finer than the symbol scale.
pub fn decode_trailing_stop(
    frame: &ProtoMessage,
    ctid: u64,
    position_scale: impl Fn(u64) -> Option<PriceScale>,
) -> Result<TrailingStopChanged, MarketDecodeError> {
    let event: ProtoOaTrailingSlChangedEvent = codec::decode_typed(
        frame,
        2107,
        &[
            (2, "ctidTraderAccountId"),
            (3, "positionId"),
            (4, "orderId"),
            (5, "stopPrice"),
            (6, "utcLastUpdateTimestamp"),
        ],
        |_| Ok(()),
    )?;
    check_account(ctid, event.ctid_trader_account_id)?;
    let position_id = positive_id(event.position_id, "positionId")?;
    let scale = position_scale(position_id).ok_or(MarketDecodeError::UnknownSymbol)?;
    Ok(TrailingStopChanged {
        position_id,
        order_id: positive_id(event.order_id, "orderId")?,
        stop_price: scale.from_decimal(event.stop_price)?,
        updated_unix_ms: timestamp(event.utc_last_update_timestamp, "utcLastUpdateTimestamp")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generated::{ProtoOaDeal, ProtoOaOrder, ProtoOaTradeData},
        market::fixtures::{CTID, CTID_WIRE, bytes_frame, frame, strip, strip_at},
    };
    use prost::Message;

    fn scales(symbol: u64) -> Option<PriceScale> {
        (symbol == 1).then(|| PriceScale::new(5).expect("digits 5"))
    }

    fn filled_order() -> ProtoOaOrder {
        ProtoOaOrder {
            order_id: 9,
            trade_data: ProtoOaTradeData {
                symbol_id: 1,
                volume: 100_000,
                trade_side: 2,
                ..ProtoOaTradeData::default()
            },
            order_type: 2,
            order_status: 2,
            limit_price: Some(1.0825),
            client_order_id: Some("aeris-2".into()),
            ..ProtoOaOrder::default()
        }
    }

    fn fill() -> ProtoOaDeal {
        ProtoOaDeal {
            deal_id: 501,
            order_id: 9,
            position_id: 77,
            volume: 100_000,
            filled_volume: 40_000,
            symbol_id: 1,
            create_timestamp: 1_791_466_999_900,
            execution_timestamp: 1_791_467_000_000,
            execution_price: Some(1.0825),
            trade_side: 2,
            deal_status: 3,
            ..ProtoOaDeal::default()
        }
    }

    #[test]
    fn recovery_reads_encode_their_windows_and_reject_bad_ones() {
        let request = TradingRequest::deal_list(CTID, 1_000, 2_000).expect("deals");
        assert_eq!((request.payload_type, request.response_type), (2133, 2134));
        let wire = ProtoOaDealListReq::decode(request.payload.as_slice()).expect("decode");
        assert_eq!(
            (wire.from_timestamp, wire.to_timestamp, wire.max_rows),
            (Some(1_000), Some(2_000), Some(MAXIMUM_DEAL_ROWS))
        );
        let request = TradingRequest::position_deals(CTID, 77, 0, 2_000).expect("position");
        let wire =
            ProtoOaDealListByPositionIdReq::decode(request.payload.as_slice()).expect("decode");
        assert_eq!((request.payload_type, wire.position_id), (2179, 77));
        let request = TradingRequest::order_details(CTID, 9).expect("details");
        assert_eq!((request.payload_type, request.response_type), (2181, 2182));
        let request = TradingRequest::order_list(CTID, 0, 1).expect("orders");
        assert_eq!((request.payload_type, request.response_type), (2175, 2176));
        for (from, to) in [(-1, 10), (10, 10), (0, MAXIMUM_TIMESTAMP_MS + 1)] {
            assert!(TradingRequest::order_list(CTID, from, to).is_err());
            assert!(TradingRequest::deal_list(CTID, from, to).is_err());
        }
        assert!(TradingRequest::order_details(CTID, 0).is_err());
    }

    #[test]
    fn order_details_carry_the_order_and_its_partial_fills() {
        let response = ProtoOaOrderDetailsRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            order: filled_order(),
            deal: vec![fill()],
        };
        let details = decode_order_details(&frame(2182, &response), CTID, &scales).expect("ok");
        assert_eq!(details.order.client_order_id.as_deref(), Some("aeris-2"));
        assert_eq!(details.deals[0].filled_volume, 40_000);
        assert_eq!(details.deals[0].execution_price, Some(108_250));
        let payload = response.encode_to_vec();
        assert!(
            decode_order_details(
                &bytes_frame(2182, strip_at(&payload, &[4], 5)),
                CTID,
                &scales
            )
            .is_err(),
            "a deal's filledVolume is required"
        );
        assert!(
            decode_order_details(
                &bytes_frame(2182, strip_at(&payload, &[3, 2], 3)),
                CTID,
                &scales
            )
            .is_err(),
            "an order's trade side is required"
        );
    }

    #[test]
    fn pages_report_whether_more_rows_exist() {
        let orders = ProtoOaOrderListRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            order: vec![filled_order()],
            has_more: true,
        };
        let page = decode_order_list(&frame(2176, &orders), CTID, &scales).expect("orders");
        assert!(page.has_more);
        assert_eq!(page.orders.len(), 1);
        assert!(
            decode_order_list(
                &bytes_frame(2176, strip(&orders.encode_to_vec(), 4)),
                CTID,
                &scales
            )
            .is_err(),
            "hasMore is required"
        );
        for payload_type in [2134, 2180] {
            let deals = ProtoOaDealListRes {
                payload_type: None,
                ctid_trader_account_id: CTID_WIRE,
                deal: vec![fill()],
                has_more: false,
            };
            let page =
                decode_deal_page(&frame(payload_type, &deals), CTID, &scales).expect("deal page");
            assert_eq!((page.deals.len(), page.has_more), (1, false));
            assert!(matches!(
                decode_deal_page(&frame(payload_type, &deals), CTID + 1, &scales),
                Err(MarketDecodeError::AccountMismatch)
            ));
        }
    }

    #[test]
    fn a_trailing_stop_change_uses_its_positions_scale() {
        let event = ProtoOaTrailingSlChangedEvent {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            position_id: 77,
            order_id: 12,
            stop_price: 1.0811,
            utc_last_update_timestamp: 1_791_467_100_000,
        };
        let changed = decode_trailing_stop(&frame(2107, &event), CTID, |position| {
            (position == 77).then(|| PriceScale::new(5).expect("5"))
        })
        .expect("trailing stop");
        assert_eq!((changed.position_id, changed.stop_price), (77, 108_110));
        assert!(matches!(
            decode_trailing_stop(&frame(2107, &event), CTID, |_| None),
            Err(MarketDecodeError::UnknownSymbol)
        ));
        assert!(
            decode_trailing_stop(
                &bytes_frame(2107, strip(&event.encode_to_vec(), 5)),
                CTID,
                |_| { PriceScale::new(5).ok() }
            )
            .is_err()
        );
    }
}
