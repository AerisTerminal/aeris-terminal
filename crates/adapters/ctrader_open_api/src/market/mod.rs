//! Market-data requests and decoders for the cTrader Open API.
//!
//! Spot, depth, trendbar and tick prices arrive as integers in 1/100000 of a
//! price unit. They are rescaled exactly to the symbol's `digits` and rejected
//! when the symbol scale cannot represent them. Volumes and depth sizes are in
//! cents (quantity scale 2); trendbar volume is a tick count (scale 0).

mod catalog;
mod price;
mod requests;
mod streams;
mod ticks;
mod trendbar;

#[cfg(test)]
mod fixtures;

pub use catalog::{
    LightSymbol, MAXIMUM_CATALOG_SYMBOLS, SymbolSpec, decode_symbol_by_id, decode_symbol_list,
};
pub use price::{PriceScale, WIRE_PRICE_DIGITS};
pub use requests::{MarketRequest, QuoteSide, decode_subscription_ack};
pub use streams::{
    DepthUpdate, MAXIMUM_DEPTH_LEVELS, MAXIMUM_DEPTH_QUOTES, MAXIMUM_STREAM_SYMBOLS, MarketStreams,
    SPOT_QUANTITY_UNAVAILABLE, SpotUpdate, SymbolStream,
};
pub use ticks::{
    HistoricalTick, MAXIMUM_TICK_PAGES, MAXIMUM_TICKS_PER_PAGE, TickHistoryPaginator, TickPage,
    decode_tick_page,
};
pub use trendbar::{
    CtraderBar, MAXIMUM_TRENDBARS_PER_PAGE, TrendbarPage, TrendbarPeriod, decode_trendbar_page,
};

use crate::codec::CodecError;
use std::{error::Error, fmt};

/// Provider identity carried in canonical metadata.
pub const CTRADER_PROVIDER_ID: &str = "ctrader";

/// Local ingestion evidence assigned by the owner; never a provider sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventStamp {
    pub source_sequence: u64,
    pub received_unix_nanos: i64,
}

#[derive(Debug)]
pub enum MarketDecodeError {
    Codec(CodecError),
    MissingField(&'static str),
    InvalidField(&'static str),
    InexactPrice,
    AccountMismatch,
    SymbolMismatch,
    UnknownSymbol,
    UnknownDepthQuote,
    LimitExceeded(&'static str),
    Canonical(aeris_market_data::MarketDataValidationError),
}

impl fmt::Display for MarketDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(error) => write!(formatter, "{error}"),
            Self::MissingField(field) => write!(formatter, "missing cTrader field {field}"),
            Self::InvalidField(field) => write!(formatter, "invalid cTrader field {field}"),
            Self::InexactPrice => {
                formatter.write_str("cTrader price is not representable at the symbol scale")
            }
            Self::AccountMismatch => formatter.write_str("cTrader response is for another account"),
            Self::SymbolMismatch => formatter.write_str("cTrader response is for another symbol"),
            Self::UnknownSymbol => formatter.write_str("cTrader event for an unsubscribed symbol"),
            Self::UnknownDepthQuote => {
                formatter.write_str("cTrader depth deleted an unknown quote; resubscribe depth")
            }
            Self::LimitExceeded(what) => write!(formatter, "cTrader {what} exceeds its bound"),
            Self::Canonical(error) => write!(formatter, "invalid canonical cTrader data: {error}"),
        }
    }
}

impl Error for MarketDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}

impl From<CodecError> for MarketDecodeError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

impl From<aeris_market_data::MarketDataValidationError> for MarketDecodeError {
    fn from(error: aeris_market_data::MarketDataValidationError) -> Self {
        Self::Canonical(error)
    }
}

fn account_id(ctid: u64) -> Result<i64, MarketDecodeError> {
    i64::try_from(ctid).map_err(|_| MarketDecodeError::InvalidField("ctidTraderAccountId"))
}

fn check_account(expected: u64, actual: i64) -> Result<(), MarketDecodeError> {
    if account_id(expected)? == actual {
        Ok(())
    } else {
        Err(MarketDecodeError::AccountMismatch)
    }
}
