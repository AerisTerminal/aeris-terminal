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

/// Exact mutation carried for one quote side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuoteSideUpdate {
    Unchanged,
    Cleared,
    Value(QuoteLevel),
}

/// Bounded best-bid/offer mutation preserving absent versus cleared sides.
#[derive(Clone, Debug, PartialEq)]
pub struct QuoteUpdate {
    pub identity: MarketIdentity,
    pub bid: QuoteSideUpdate,
    pub ask: QuoteSideUpdate,
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

/// Sides explicitly present in one aggregate-book chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderBookSides {
    pub bids: bool,
    pub asks: bool,
}

/// One bounded aggregate order-book chunk.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderBookUpdate {
    pub identity: MarketIdentity,
    pub kind: OrderBookUpdateKind,
    pub present_sides: OrderBookSides,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: Option<ProviderTimestamp>,
}

/// Sanitized market-data message decoded from one binary WebSocket message.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedMarketMessage {
    Trade(TradeUpdate),
    Quote(QuoteUpdate),
    OrderBook(OrderBookUpdate),
}

#[cfg(rithmic_kit)]
pub(crate) fn decode(frame: &[u8]) -> Result<Option<DecodedMarketMessage>, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    bound_frame(frame)?;
    let message_type = rti::MessageType::decode(frame).map_err(|_| ProtocolError::Decode)?;
    match message_type.template_id {
        150 => decode_trade(frame),
        151 => decode_quote(frame).map(Some),
        156 => decode_order_book(frame).map(Some),
        template => Err(ProtocolError::UnsupportedTemplate(template)),
    }
}

#[cfg(not(rithmic_kit))]
pub(crate) fn decode(_frame: &[u8]) -> Result<Option<DecodedMarketMessage>, ProtocolError> {
    Err(ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
fn decode_trade(frame: &[u8]) -> Result<Option<DecodedMarketMessage>, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::LastTrade::decode(frame).map_err(|_| ProtocolError::Decode)?;
    let identity = identity(message.symbol, message.exchange)?;
    if message.trade_price.is_none() && message.trade_size.is_none() {
        // Schema-valid session/clear marker: a LastTrade frame without price
        // and size carries no trade. The plant emits these around session
        // boundaries and subscription snapshots. Never fabricate a price and
        // never poison the stream; the session layer skips marker frames.
        return Ok(None);
    }
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
    Ok(Some(DecodedMarketMessage::Trade(TradeUpdate {
        identity,
        price,
        size,
        aggressor,
        is_snapshot: message.is_snapshot.unwrap_or(false),
        timestamp: timestamp(message.ssboe, message.usecs)?,
    })))
}

#[cfg(rithmic_kit)]
fn decode_quote(frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::BestBidOffer::decode(frame).map_err(|_| ProtocolError::Decode)?;
    let presence = message.presence_bits.unwrap_or(0);
    let clear = message.clear_bits.unwrap_or(0);
    if presence & !0b111 != 0 || clear & !0b111 != 0 {
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
    let presence = message.presence_bits.unwrap_or(0);
    if presence & !0b11 != 0 {
        return Err(ProtocolError::InvalidPresenceBits);
    }
    let present_sides = OrderBookSides {
        bids: presence & 1 != 0,
        asks: presence & 2 != 0,
    };
    let bids = depth_levels(
        "order_book.bid",
        present_sides.bids,
        message.bid_price,
        &message.bid_size,
        &message.bid_orders,
        &message.impl_bid_size,
    )?;
    let asks = depth_levels(
        "order_book.ask",
        present_sides.asks,
        message.ask_price,
        &message.ask_size,
        &message.ask_orders,
        &message.impl_ask_size,
    )?;
    Ok(DecodedMarketMessage::OrderBook(OrderBookUpdate {
        identity: identity(message.symbol, message.exchange)?,
        kind,
        present_sides,
        bids,
        asks,
        timestamp: optional_timestamp(message.ssboe, message.usecs)?,
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
    let microseconds = microseconds.unwrap_or(0);
    if seconds < 0 || !(0..1_000_000).contains(&microseconds) {
        return Err(ProtocolError::InvalidHeartbeat);
    }
    Ok(ProviderTimestamp {
        seconds,
        microseconds,
    })
}

#[cfg(rithmic_kit)]
fn optional_timestamp(
    seconds: Option<i32>,
    microseconds: Option<i32>,
) -> Result<Option<ProviderTimestamp>, ProtocolError> {
    match (seconds, microseconds) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(ProtocolError::MissingField("ssboe")),
        (Some(seconds), microseconds) => timestamp(Some(seconds), microseconds).map(Some),
    }
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
) -> Result<QuoteSideUpdate, ProtocolError> {
    if cleared && (price.is_none() || size.is_none()) {
        if price.is_some_and(|price| !price.is_finite())
            || size.is_some_and(|size| size < 0)
            || orders.is_some_and(|orders| orders < 0)
        {
            return Err(ProtocolError::InvalidNumber(field));
        }
        return Ok(QuoteSideUpdate::Cleared);
    }
    if !present {
        if price.is_some() || size.is_some() || orders.is_some() {
            return Err(ProtocolError::InconsistentFields(field));
        }
        return Ok(QuoteSideUpdate::Unchanged);
    }
    Ok(QuoteSideUpdate::Value(QuoteLevel {
        price: finite_required(field, price)?,
        size: nonnegative_required(field, size)?,
        orders: optional_nonnegative(field, orders)?,
    }))
}

#[cfg(rithmic_kit)]
fn depth_levels(
    field: &'static str,
    present: bool,
    prices: Vec<f64>,
    sizes: &[i32],
    orders: &[i32],
    implied_sizes: &[i32],
) -> Result<Vec<OrderBookLevel>, ProtocolError> {
    if !present
        && (!prices.is_empty()
            || !sizes.is_empty()
            || !orders.is_empty()
            || !implied_sizes.is_empty())
    {
        return Err(ProtocolError::InconsistentFields(field));
    }
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
    fn omitted_market_microseconds_default_to_zero() {
        assert_eq!(
            timestamp(Some(1_800_000_000), None),
            Ok(ProviderTimestamp {
                seconds: 1_800_000_000,
                microseconds: 0,
            })
        );
    }

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
            Some(DecodedMarketMessage::Trade(TradeUpdate {
                size: 3,
                aggressor: Some(TradeAggressor::Buy),
                ..
            }))
        ));

        let quote = quote_message().encode_to_vec();
        assert!(matches!(
            codec.decode_market(&quote).expect("quote decodes"),
            Some(DecodedMarketMessage::Quote(QuoteUpdate {
                bid: QuoteSideUpdate::Value(QuoteLevel { size: 10, .. }),
                ask: QuoteSideUpdate::Value(QuoteLevel { size: 12, .. }),
                ..
            }))
        ));

        let book = order_book(vec![5_100.0], vec![10]);
        assert!(matches!(
            codec.decode_market(&book).expect("book decodes"),
            Some(DecodedMarketMessage::OrderBook(OrderBookUpdate {
                kind: OrderBookUpdateKind::Solo,
                bids,
                ..
            })) if bids.len() == 1
        ));
    }

    #[test]
    fn quote_clear_bits_support_snapshot_replacement_and_empty_side_removal() {
        let codec = RithmicProtocolCodec;
        let mut snapshot_clear = quote_message();
        snapshot_clear.clear_bits = Some(3);
        assert!(matches!(
            codec
                .decode_market(&snapshot_clear.encode_to_vec())
                .expect("snapshot clear-and-set decodes"),
            Some(DecodedMarketMessage::Quote(QuoteUpdate {
                bid: QuoteSideUpdate::Value(_),
                ask: QuoteSideUpdate::Value(_),
                ..
            }))
        ));

        let clear_bid = rti::BestBidOffer {
            presence_bits: Some(0),
            clear_bits: Some(1),
            bid_price: None,
            bid_size: None,
            bid_orders: Some(0),
            ask_price: None,
            ask_size: None,
            ask_orders: None,
            ..snapshot_clear
        };
        assert!(matches!(
            codec
                .decode_market(&clear_bid.encode_to_vec())
                .expect("clear-only quote decodes"),
            Some(DecodedMarketMessage::Quote(QuoteUpdate {
                bid: QuoteSideUpdate::Cleared,
                ask: QuoteSideUpdate::Unchanged,
                ..
            }))
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

        let mut absent_side = order_book_message(vec![5_100.0], vec![10]);
        absent_side.presence_bits = Some(0);
        assert!(matches!(
            codec.decode_market(&absent_side.encode_to_vec()),
            Err(ProtocolError::InconsistentFields("order_book.bid"))
        ));

        let mut unknown_presence = order_book_message(Vec::new(), Vec::new());
        unknown_presence.presence_bits = Some(4);
        assert!(matches!(
            codec.decode_market(&unknown_presence.encode_to_vec()),
            Err(ProtocolError::InvalidPresenceBits)
        ));
    }

    #[test]
    fn aggregate_book_accepts_provider_frames_without_exchange_timestamp() {
        let mut book = order_book_message(vec![5_100.0], vec![10]);
        book.ssboe = None;
        book.usecs = None;
        assert!(matches!(
            RithmicProtocolCodec.decode_market(&book.encode_to_vec()),
            Ok(Some(DecodedMarketMessage::OrderBook(OrderBookUpdate {
                timestamp: None,
                ..
            })))
        ));
    }

    #[test]
    fn priceless_trade_marker_skips_without_an_event_or_a_failure() {
        let codec = RithmicProtocolCodec;
        // Mirrors the live plant's session/clear marker: presence and clear
        // bits set, but no price, size, or timestamp. It carries no trade, so
        // it decodes to no event; it is schema-valid, so it must not fail.
        let marker = trade_message();
        let marker = rti::LastTrade {
            trade_price: None,
            trade_size: None,
            aggressor: None,
            ssboe: None,
            usecs: None,
            clear_bits: Some(1),
            ..marker
        }
        .encode_to_vec();
        assert_eq!(codec.decode_market(&marker).expect("marker decodes"), None);

        let mut anonymous = trade_message();
        anonymous.symbol = None;
        anonymous.trade_price = None;
        anonymous.trade_size = None;
        assert!(matches!(
            codec.decode_market(&anonymous.encode_to_vec()),
            Err(ProtocolError::MissingField("symbol"))
        ));
    }

    #[test]
    fn partial_trade_content_still_fails_closed() {
        let codec = RithmicProtocolCodec;
        let mut price_only = trade_message();
        price_only.trade_size = None;
        assert!(matches!(
            codec.decode_market(&price_only.encode_to_vec()),
            Err(ProtocolError::MissingField("trade_size"))
        ));

        let mut size_only = trade_message();
        size_only.trade_price = None;
        assert!(matches!(
            codec.decode_market(&size_only.encode_to_vec()),
            Err(ProtocolError::MissingField("trade_price"))
        ));
    }

    fn trade_message() -> rti::LastTrade {
        rti::LastTrade {
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
    }

    fn order_book(bid_price: Vec<f64>, bid_size: Vec<i32>) -> Vec<u8> {
        order_book_message(bid_price, bid_size).encode_to_vec()
    }

    fn quote_message() -> rti::BestBidOffer {
        rti::BestBidOffer {
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
    }

    fn order_book_message(bid_price: Vec<f64>, bid_size: Vec<i32>) -> rti::OrderBook {
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
    }
}
