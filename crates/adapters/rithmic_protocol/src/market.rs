use crate::ProtocolError;

#[cfg(rithmic_kit)]
const MAX_FRAME_BYTES: usize = 1024 * 1024;
#[cfg(rithmic_kit)]
const MAX_FIELD_BYTES: usize = 256;
#[cfg(rithmic_kit)]
const MAX_DEPTH_LEVELS_PER_SIDE: usize = 4_096;

/// Provider instrument identity carried by a market-data update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketIdentity {
    pub symbol: String,
    pub exchange: String,
}

/// Provider timestamp represented without local-clock interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderTimestamp {
    pub seconds: i32,
    pub microseconds: i32,
}

/// Provider-reported aggressor side.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TradeAggressor {
    Buy,
    Sell,
}

/// Bounded last-trade update.
#[derive(Clone, Debug, PartialEq)]
pub struct TradeUpdate {
    pub identity: MarketIdentity,
    pub price: f64,
    pub size: u32,
    pub aggressor: Option<TradeAggressor>,
    pub is_snapshot: bool,
    pub timestamp: ProviderTimestamp,
}

/// One side of a top-of-book update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuoteLevel {
    pub price: f64,
    pub size: u32,
    pub orders: Option<u32>,
}

/// Bounded best-bid/offer update. A `None` side is explicitly absent or cleared.
#[derive(Clone, Debug, PartialEq)]
pub struct QuoteUpdate {
    pub identity: MarketIdentity,
    pub bid: Option<QuoteLevel>,
    pub ask: Option<QuoteLevel>,
    pub is_snapshot: bool,
    pub timestamp: ProviderTimestamp,
}

/// Provider aggregate-book chunk semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookUpdateKind {
    Clear,
    Unavailable,
    Snapshot,
    Begin,
    Middle,
    End,
    Solo,
}

/// One aggregate depth level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrderBookLevel {
    pub price: f64,
    pub size: u32,
    pub orders: Option<u32>,
    pub implied_size: Option<u32>,
}

/// One bounded aggregate order-book chunk.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderBookUpdate {
    pub identity: MarketIdentity,
    pub kind: OrderBookUpdateKind,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: ProviderTimestamp,
}

/// Sanitized market-data message decoded from one binary WebSocket message.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedMarketMessage {
    Trade(TradeUpdate),
    Quote(QuoteUpdate),
    OrderBook(OrderBookUpdate),
}

#[cfg(rithmic_kit)]
pub(crate) fn decode(frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    bound_frame(frame)?;
    let message_type = rti::MessageType::decode(frame).map_err(|_| ProtocolError::Decode)?;
    match message_type.template_id {
        150 => decode_trade(frame),
        151 => decode_quote(frame),
        156 => decode_order_book(frame),
        template => Err(ProtocolError::UnsupportedTemplate(template)),
    }
}

#[cfg(not(rithmic_kit))]
pub(crate) fn decode(_frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    Err(ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
fn decode_trade(frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::LastTrade::decode(frame).map_err(|_| ProtocolError::Decode)?;
    let identity = identity(message.symbol, message.exchange)?;
    let price = finite_required("trade_price", message.trade_price)?;
    let size = nonnegative_required("trade_size", message.trade_size)?;
    let aggressor = message
        .aggressor
        .map(
            |value| match rti::last_trade::TransactionType::try_from(value) {
                Ok(rti::last_trade::TransactionType::Buy) => Ok(TradeAggressor::Buy),
                Ok(rti::last_trade::TransactionType::Sell) => Ok(TradeAggressor::Sell),
                Err(_) => Err(ProtocolError::UnknownEnum("last_trade.aggressor")),
            },
        )
        .transpose()?;
    Ok(DecodedMarketMessage::Trade(TradeUpdate {
        identity,
        price,
        size,
        aggressor,
        is_snapshot: message.is_snapshot.unwrap_or(false),
        timestamp: timestamp(message.ssboe, message.usecs)?,
    }))
}

#[cfg(rithmic_kit)]
fn decode_quote(frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::BestBidOffer::decode(frame).map_err(|_| ProtocolError::Decode)?;
    let presence = message.presence_bits.unwrap_or(0);
    let clear = message.clear_bits.unwrap_or(0);
    if presence & clear != 0 || presence & !0b111 != 0 || clear & !0b111 != 0 {
        return Err(ProtocolError::InvalidPresenceBits);
    }
    let bid = quote_level(
        "bid",
        presence & 1 != 0,
        clear & 1 != 0,
        message.bid_price,
        message.bid_size,
        message.bid_orders,
    )?;
    let ask = quote_level(
        "ask",
        presence & 2 != 0,
        clear & 2 != 0,
        message.ask_price,
        message.ask_size,
        message.ask_orders,
    )?;
    Ok(DecodedMarketMessage::Quote(QuoteUpdate {
        identity: identity(message.symbol, message.exchange)?,
        bid,
        ask,
        is_snapshot: message.is_snapshot.unwrap_or(false),
        timestamp: timestamp(message.ssboe, message.usecs)?,
    }))
}

#[cfg(rithmic_kit)]
fn decode_order_book(frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::OrderBook::decode(frame).map_err(|_| ProtocolError::Decode)?;
    let kind = match message
        .update_type
        .and_then(|value| rti::order_book::UpdateType::try_from(value).ok())
    {
        Some(rti::order_book::UpdateType::ClearOrderBook) => OrderBookUpdateKind::Clear,
        Some(rti::order_book::UpdateType::NoBook) => OrderBookUpdateKind::Unavailable,
        Some(rti::order_book::UpdateType::SnapshotImage) => OrderBookUpdateKind::Snapshot,
        Some(rti::order_book::UpdateType::Begin) => OrderBookUpdateKind::Begin,
        Some(rti::order_book::UpdateType::Middle) => OrderBookUpdateKind::Middle,
        Some(rti::order_book::UpdateType::End) => OrderBookUpdateKind::End,
        Some(rti::order_book::UpdateType::Solo) => OrderBookUpdateKind::Solo,
        None => return Err(ProtocolError::UnknownEnum("order_book.update_type")),
    };
    let bids = depth_levels(
        "order_book.bid",
        message.bid_price,
        &message.bid_size,
        &message.bid_orders,
        &message.impl_bid_size,
    )?;
    let asks = depth_levels(
        "order_book.ask",
        message.ask_price,
        &message.ask_size,
        &message.ask_orders,
        &message.impl_ask_size,
    )?;
    Ok(DecodedMarketMessage::OrderBook(OrderBookUpdate {
        identity: identity(message.symbol, message.exchange)?,
        kind,
        bids,
        asks,
        timestamp: timestamp(message.ssboe, message.usecs)?,
    }))
}

#[cfg(rithmic_kit)]
fn bound_frame(frame: &[u8]) -> Result<(), ProtocolError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            requested: frame.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(rithmic_kit)]
fn identity(
    symbol: Option<String>,
    exchange: Option<String>,
) -> Result<MarketIdentity, ProtocolError> {
    let symbol = required_string("symbol", symbol)?;
    let exchange = required_string("exchange", exchange)?;
    Ok(MarketIdentity { symbol, exchange })
}

#[cfg(rithmic_kit)]
fn required_string(field: &'static str, value: Option<String>) -> Result<String, ProtocolError> {
    let value = value.ok_or(ProtocolError::MissingField(field))?;
    if value.is_empty() {
        return Err(ProtocolError::EmptyField(field));
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(ProtocolError::FieldTooLong {
            field,
            maximum: MAX_FIELD_BYTES,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ProtocolError::ControlCharacter(field));
    }
    Ok(value)
}

#[cfg(rithmic_kit)]
fn timestamp(
    seconds: Option<i32>,
    microseconds: Option<i32>,
) -> Result<ProviderTimestamp, ProtocolError> {
    let seconds = seconds.ok_or(ProtocolError::MissingField("ssboe"))?;
    let microseconds = microseconds.ok_or(ProtocolError::MissingField("usecs"))?;
    if seconds < 0 || !(0..1_000_000).contains(&microseconds) {
        return Err(ProtocolError::InvalidHeartbeat);
    }
    Ok(ProviderTimestamp {
        seconds,
        microseconds,
    })
}

#[cfg(rithmic_kit)]
fn finite_required(field: &'static str, value: Option<f64>) -> Result<f64, ProtocolError> {
    let value = value.ok_or(ProtocolError::MissingField(field))?;
    if !value.is_finite() {
        return Err(ProtocolError::InvalidNumber(field));
    }
    Ok(value)
}

#[cfg(rithmic_kit)]
fn nonnegative_required(field: &'static str, value: Option<i32>) -> Result<u32, ProtocolError> {
    let value = value.ok_or(ProtocolError::MissingField(field))?;
    u32::try_from(value).map_err(|_| ProtocolError::InvalidNumber(field))
}

#[cfg(rithmic_kit)]
fn optional_nonnegative(
    field: &'static str,
    value: Option<i32>,
) -> Result<Option<u32>, ProtocolError> {
    value
        .map(|value| u32::try_from(value).map_err(|_| ProtocolError::InvalidNumber(field)))
        .transpose()
}

#[cfg(rithmic_kit)]
fn quote_level(
    field: &'static str,
    present: bool,
    cleared: bool,
    price: Option<f64>,
    size: Option<i32>,
    orders: Option<i32>,
) -> Result<Option<QuoteLevel>, ProtocolError> {
    if cleared {
        if price.is_some() || size.is_some() || orders.is_some() {
            return Err(ProtocolError::InconsistentFields(field));
        }
        return Ok(None);
    }
    if !present {
        if price.is_some() || size.is_some() || orders.is_some() {
            return Err(ProtocolError::InconsistentFields(field));
        }
        return Ok(None);
    }
    Ok(Some(QuoteLevel {
        price: finite_required(field, price)?,
        size: nonnegative_required(field, size)?,
        orders: optional_nonnegative(field, orders)?,
    }))
}

#[cfg(rithmic_kit)]
fn depth_levels(
    field: &'static str,
    prices: Vec<f64>,
    sizes: &[i32],
    orders: &[i32],
    implied_sizes: &[i32],
) -> Result<Vec<OrderBookLevel>, ProtocolError> {
    if prices.len() > MAX_DEPTH_LEVELS_PER_SIDE {
        return Err(ProtocolError::RepeatedFieldLimitExceeded {
            field,
            maximum: MAX_DEPTH_LEVELS_PER_SIDE,
        });
    }
    if prices.len() != sizes.len()
        || (!orders.is_empty() && orders.len() != prices.len())
        || (!implied_sizes.is_empty() && implied_sizes.len() != prices.len())
    {
        return Err(ProtocolError::ParallelFieldLength(field));
    }
    prices
        .into_iter()
        .enumerate()
        .map(|(index, price)| {
            if !price.is_finite() {
                return Err(ProtocolError::InvalidNumber(field));
            }
            Ok(OrderBookLevel {
                price,
                size: u32::try_from(sizes[index])
                    .map_err(|_| ProtocolError::InvalidNumber(field))?,
                orders: orders
                    .get(index)
                    .copied()
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| ProtocolError::InvalidNumber(field))?,
                implied_size: implied_sizes
                    .get(index)
                    .copied()
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| ProtocolError::InvalidNumber(field))?,
            })
        })
        .collect()
}

#[cfg(all(test, rithmic_kit))]
mod tests {
    use super::*;
    use crate::{RithmicProtocolCodec, generated::rti};
    use prost::Message;

    #[test]
    fn decodes_trade_quote_and_bounded_book() {
        let codec = RithmicProtocolCodec;
        let trade = rti::LastTrade {
            template_id: 150,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            presence_bits: Some(1),
            clear_bits: Some(0),
            is_snapshot: Some(false),
            trade_price: Some(5_100.25),
            trade_size: Some(3),
            aggressor: Some(rti::last_trade::TransactionType::Buy.into()),
            exchange_order_id: None,
            aggressor_exchange_order_id: None,
            net_change: None,
            percent_change: None,
            volume: None,
            vwap: None,
            trade_time: None,
            ssboe: Some(1_800_000_000),
            usecs: Some(123_456),
            source_ssboe: None,
            source_usecs: None,
            source_nsecs: None,
            jop_ssboe: None,
            jop_nsecs: None,
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_market(&trade).expect("trade decodes"),
            DecodedMarketMessage::Trade(TradeUpdate {
                size: 3,
                aggressor: Some(TradeAggressor::Buy),
                ..
            })
        ));

        let quote = rti::BestBidOffer {
            template_id: 151,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            presence_bits: Some(3),
            clear_bits: Some(0),
            is_snapshot: Some(true),
            bid_price: Some(5_100.0),
            bid_size: Some(10),
            bid_orders: Some(2),
            bid_implicit_size: None,
            bid_time: None,
            ask_price: Some(5_100.25),
            ask_size: Some(12),
            ask_orders: Some(3),
            ask_implicit_size: None,
            ask_time: None,
            lean_price: None,
            ssboe: Some(1_800_000_000),
            usecs: Some(123_457),
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_market(&quote).expect("quote decodes"),
            DecodedMarketMessage::Quote(QuoteUpdate {
                bid: Some(QuoteLevel { size: 10, .. }),
                ask: Some(QuoteLevel { size: 12, .. }),
                ..
            })
        ));

        let book = order_book(vec![5_100.0], vec![10]);
        assert!(matches!(
            codec.decode_market(&book).expect("book decodes"),
            DecodedMarketMessage::OrderBook(OrderBookUpdate {
                kind: OrderBookUpdateKind::Solo,
                bids,
                ..
            }) if bids.len() == 1
        ));
    }

    #[test]
    fn malformed_market_vectors_and_numbers_fail_closed() {
        let codec = RithmicProtocolCodec;
        let mismatched = order_book(vec![5_100.0, 5_099.75], vec![10]);
        assert!(matches!(
            codec.decode_market(&mismatched),
            Err(ProtocolError::ParallelFieldLength("order_book.bid"))
        ));

        let nonfinite = order_book(vec![f64::NAN], vec![10]);
        assert!(matches!(
            codec.decode_market(&nonfinite),
            Err(ProtocolError::InvalidNumber("order_book.bid"))
        ));
    }

    fn order_book(bid_price: Vec<f64>, bid_size: Vec<i32>) -> Vec<u8> {
        rti::OrderBook {
            template_id: 156,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            presence_bits: Some(1),
            update_type: Some(rti::order_book::UpdateType::Solo.into()),
            bid_price,
            bid_size,
            bid_orders: Vec::new(),
            impl_bid_size: Vec::new(),
            ask_price: Vec::new(),
            ask_size: Vec::new(),
            ask_orders: Vec::new(),
            impl_ask_size: Vec::new(),
            ssboe: Some(1_800_000_000),
            usecs: Some(123_458),
        }
        .encode_to_vec()
    }
}
