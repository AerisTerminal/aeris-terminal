//! Trading requests and decoders for the cTrader Open API.
//!
//! Order prices travel as absolute decimal doubles and are converted exactly to and from
//! the symbol's [`PriceScale`](crate::market::PriceScale); relative protection distances
//! travel in 1/100000 of a price unit. Volumes are in cents of a unit (quantity scale 2)
//! and money values carry their own `moneyDigits`. Every request that can change an order or position accepts only a
//! [`DemoAccount`](crate::accounts::DemoAccount) while live trading is disabled.
//!
//! Decoding reuses [`MarketDecodeError`]: the failure kinds (missing or invalid fields,
//! inexact prices, foreign accounts, unknown symbols, bounds) are the same.

mod events;
mod recovery;
mod requests;

pub use recovery::{
    DealPage, OrderDetails, OrderPage, TrailingStopChanged, decode_deal_page, decode_order_details,
    decode_order_list, decode_trailing_stop,
};

pub use events::{
    AccountType, AveragePrice, ClosedVolume, Deal, DealStatus, ExecutionEvent, ExecutionType,
    MAXIMUM_STATE_ITEMS, Money, OrderErrorEvent, OrderKind, OrderState, OrderStatus, PositionState,
    PositionStatus, Reconciliation, TraderAccount, decode_execution_event,
    decode_order_error_event, decode_reconcile, decode_trader, event_account, referenced_symbols,
};
pub use requests::{
    MAXIMUM_CLIENT_ORDER_ID_BYTES, NewOrder, OrderAmendment, OrderPrice, OrderType, Protection,
    TimeInForce, TradingRequest,
};

use crate::market::MarketDecodeError;

/// The schema bounds timestamps to 1970..=2038-01-19 in milliseconds.
const MAXIMUM_TIMESTAMP_MS: i64 = 2_147_483_646_000;

/// Order and position direction (`ProtoOATradeSide`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TradeSide {
    Buy,
    Sell,
}

impl TradeSide {
    const fn wire(self) -> i32 {
        match self {
            Self::Buy => 1,
            Self::Sell => 2,
        }
    }

    const fn from_wire(value: i32) -> Result<Self, MarketDecodeError> {
        match value {
            1 => Ok(Self::Buy),
            2 => Ok(Self::Sell),
            _ => Err(MarketDecodeError::InvalidField("tradeSide")),
        }
    }
}

fn positive_id(value: i64, field: &'static str) -> Result<u64, MarketDecodeError> {
    u64::try_from(value)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(MarketDecodeError::InvalidField(field))
}

fn wire_id(value: u64, field: &'static str) -> Result<i64, MarketDecodeError> {
    i64::try_from(value)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(MarketDecodeError::InvalidField(field))
}
