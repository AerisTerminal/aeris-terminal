use crate::ProtocolError;

#[cfg(rithmic_kit)]
const MAX_FRAME_BYTES: usize = 1024 * 1024;
#[cfg(rithmic_kit)]
const MAX_FIELD_BYTES: usize = 256;
#[cfg(rithmic_kit)]
const MAX_DEPTH_LEVELS_PER_SIDE: usize = 4_096;
#[cfg(rithmic_kit)]
const MAX_DEPTH_BY_ORDER_MUTATIONS: usize = 4_096;

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

/// Side of one order-level depth record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthByOrderSide {
    Bid,
    Ask,
}

/// Mutation semantics of one order-level depth record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthByOrderMutationKind {
    New,
    Change,
    Delete,
}

/// One bounded market-by-order mutation keyed by the exchange order id.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthByOrderMutation {
    pub kind: DepthByOrderMutationKind,
    pub side: DepthByOrderSide,
    pub price: f64,
    pub previous_price: Option<f64>,
    pub size: u32,
    pub priority: u64,
    pub exchange_order_id: String,
}

/// One Rithmic depth-by-order update frame.
///
/// Rithmic omits `sequence_number` on some frames (observed on live deletes),
/// so it is absent rather than invented.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthByOrderUpdate {
    pub identity: MarketIdentity,
    pub sequence_number: Option<u64>,
    pub mutations: Vec<DepthByOrderMutation>,
    pub timestamp: Option<ProviderTimestamp>,
}

/// One order contained in a covering DBO snapshot level.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthByOrderSnapshotOrder {
    pub size: u32,
    pub priority: u64,
    pub exchange_order_id: String,
}

/// One price level from a covering DBO snapshot response.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthByOrderSnapshotLevel {
    pub identity: MarketIdentity,
    /// Absent on some observed snapshot levels.
    pub sequence_number: Option<u64>,
    pub side: DepthByOrderSide,
    pub price: f64,
    pub orders: Vec<DepthByOrderSnapshotOrder>,
}

/// Multi-frame covering DBO snapshot response.
#[derive(Clone, Debug, PartialEq)]
pub enum DepthByOrderSnapshotMessage {
    Level(DepthByOrderSnapshotLevel),
    Complete {
        accepted: bool,
        identity: Option<MarketIdentity>,
        sequence_number: Option<u64>,
    },
}

/// Marker terminating the initial DBO image for one or more instruments.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthByOrderEndEvent {
    pub identities: Vec<MarketIdentity>,
    pub sequence_number: Option<u64>,
    pub timestamp: Option<ProviderTimestamp>,
}

/// Sanitized market-data message decoded from one binary WebSocket message.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedMarketMessage {
    Trade(TradeUpdate),
    Quote(QuoteUpdate),
    OrderBook(OrderBookUpdate),
    DepthByOrderSnapshot(DepthByOrderSnapshotMessage),
    DepthByOrder(DepthByOrderUpdate),
    DepthByOrderEnd(DepthByOrderEndEvent),
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
        116 => decode_depth_by_order_snapshot(frame),
        160 => decode_depth_by_order(frame).map(Some),
        161 => decode_depth_by_order_end(frame).map(Some),
        template => Err(ProtocolError::UnsupportedTemplate(template)),
    }
}

#[cfg(rithmic_kit)]
#[allow(clippy::too_many_lines)]
fn decode_depth_by_order_snapshot(
    frame: &[u8],
) -> Result<Option<DecodedMarketMessage>, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message =
        rti::ResponseDepthByOrderSnapshot::decode(frame).map_err(|_| ProtocolError::Decode)?;
    validate_snapshot_codes(
        &message.user_msg,
        &message.rq_handler_rp_code,
        &message.rp_code,
    )?;
    match (
        message.rq_handler_rp_code.is_empty(),
        message.rp_code.is_empty(),
    ) {
        (false, true) => {
            if is_empty_book_frame(&message) {
                // Observed on Rithmic Test with a closed market: the handler
                // reports `["7", "no data"]` with no level before the terminal
                // frame. That is an empty book, not a rejected request.
                return Ok(None);
            }
            if !accepted_snapshot_code(&message.rq_handler_rp_code) {
                return Err(ProtocolError::RejectedDataFrame);
            }
            let identity = identity(message.symbol, message.exchange)?;
            let sequence_number = message.sequence_number.filter(|sequence| *sequence != 0);
            let side = match message.depth_side.and_then(|side| {
                rti::response_depth_by_order_snapshot::TransactionType::try_from(side).ok()
            }) {
                Some(rti::response_depth_by_order_snapshot::TransactionType::Buy) => {
                    DepthByOrderSide::Bid
                }
                Some(rti::response_depth_by_order_snapshot::TransactionType::Sell) => {
                    DepthByOrderSide::Ask
                }
                None => {
                    return Err(ProtocolError::UnknownEnum(
                        "depth_by_order_snapshot.depth_side",
                    ));
                }
            };
            let price =
                finite_required("depth_by_order_snapshot.depth_price", message.depth_price)?;
            if price <= 0.0 {
                return Err(ProtocolError::InvalidNumber(
                    "depth_by_order_snapshot.depth_price",
                ));
            }
            let count = message.depth_size.len();
            if count > MAX_DEPTH_BY_ORDER_MUTATIONS {
                return Err(ProtocolError::RepeatedFieldLimitExceeded {
                    field: "depth_by_order_snapshot.depth_size",
                    maximum: MAX_DEPTH_BY_ORDER_MUTATIONS,
                });
            }
            if message.depth_order_priority.len() != count
                || message.exchange_order_id.len() != count
            {
                return Err(ProtocolError::ParallelFieldLength(
                    "depth_by_order_snapshot",
                ));
            }
            let orders = message
                .depth_size
                .into_iter()
                .zip(message.depth_order_priority)
                .zip(message.exchange_order_id)
                .map(|((size, priority), exchange_order_id)| {
                    Ok(DepthByOrderSnapshotOrder {
                        size: u32::try_from(size).map_err(|_| {
                            ProtocolError::InvalidNumber("depth_by_order_snapshot.depth_size")
                        })?,
                        priority,
                        exchange_order_id: bounded_string(
                            "depth_by_order_snapshot.exchange_order_id",
                            exchange_order_id,
                        )?,
                    })
                })
                .collect::<Result<Vec<_>, ProtocolError>>()?;
            Ok(Some(DecodedMarketMessage::DepthByOrderSnapshot(
                DepthByOrderSnapshotMessage::Level(DepthByOrderSnapshotLevel {
                    identity,
                    sequence_number,
                    side,
                    price,
                    orders,
                }),
            )))
        }
        (true, false) => {
            let accepted = accepted_snapshot_code(&message.rp_code);
            if message.depth_side.is_some()
                || message.depth_price.is_some()
                || !message.depth_size.is_empty()
                || !message.depth_order_priority.is_empty()
                || !message.exchange_order_id.is_empty()
            {
                return Err(ProtocolError::InconsistentFields(
                    "depth_by_order_snapshot.complete",
                ));
            }
            let identity = match (message.symbol, message.exchange) {
                (Some(symbol), Some(exchange)) => Some(MarketIdentity {
                    symbol: bounded_string("depth_by_order_snapshot.symbol", symbol)?,
                    exchange: bounded_string("depth_by_order_snapshot.exchange", exchange)?,
                }),
                (None, None) => None,
                _ => {
                    return Err(ProtocolError::InconsistentFields(
                        "depth_by_order_snapshot.complete_identity",
                    ));
                }
            };
            Ok(Some(DecodedMarketMessage::DepthByOrderSnapshot(
                DepthByOrderSnapshotMessage::Complete {
                    accepted,
                    identity,
                    sequence_number: message.sequence_number.filter(|sequence| *sequence != 0),
                },
            )))
        }
        _ => Err(ProtocolError::ResponseCodeShape),
    }
}

#[cfg(rithmic_kit)]
fn validate_snapshot_codes(
    user_messages: &[String],
    handler_codes: &[String],
    terminal_codes: &[String],
) -> Result<(), ProtocolError> {
    if user_messages.len() > 2 {
        return Err(ProtocolError::RepeatedFieldLimitExceeded {
            field: "depth_by_order_snapshot.user_msg",
            maximum: 2,
        });
    }
    for value in user_messages {
        bounded_string("depth_by_order_snapshot.user_msg", value.clone())?;
    }
    validate_snapshot_code_field("depth_by_order_snapshot.rq_handler_rp_code", handler_codes)?;
    validate_snapshot_code_field("depth_by_order_snapshot.rp_code", terminal_codes)
}

#[cfg(rithmic_kit)]
fn validate_snapshot_code_field(
    field: &'static str,
    codes: &[String],
) -> Result<(), ProtocolError> {
    match codes {
        [] => Ok(()),
        [code] if code == "0" => Ok(()),
        [code, detail] if code.parse::<u32>().is_ok_and(|value| value > 0) => {
            bounded_string(field, code.clone())?;
            bounded_string(field, detail.clone()).map(|_| ())
        }
        _ => Err(ProtocolError::ResponseCodeShape),
    }
}

#[cfg(rithmic_kit)]
fn accepted_snapshot_code(codes: &[String]) -> bool {
    codes.len() == 1 && codes[0] == "0"
}

#[cfg(rithmic_kit)]
const NO_DATA_RESPONSE_CODE: &str = "7";

#[cfg(rithmic_kit)]
fn is_empty_book_frame(message: &crate::generated::rti::ResponseDepthByOrderSnapshot) -> bool {
    message
        .rq_handler_rp_code
        .first()
        .is_some_and(|code| code == NO_DATA_RESPONSE_CODE)
        && message.depth_side.is_none()
        && message.depth_price.is_none()
        && message.depth_size.is_empty()
        && message.depth_order_priority.is_empty()
        && message.exchange_order_id.is_empty()
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
fn decode_depth_by_order(frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::DepthByOrder::decode(frame).map_err(|_| ProtocolError::Decode)?;
    let identity = identity(message.symbol, message.exchange)?;
    let sequence_number = message.sequence_number.filter(|sequence| *sequence != 0);
    let count = message.update_type.len();
    if count == 0 {
        return Err(ProtocolError::MissingField("depth_by_order.update_type"));
    }
    if count > MAX_DEPTH_BY_ORDER_MUTATIONS {
        return Err(ProtocolError::RepeatedFieldLimitExceeded {
            field: "depth_by_order.update_type",
            maximum: MAX_DEPTH_BY_ORDER_MUTATIONS,
        });
    }
    if message.transaction_type.len() != count
        || message.depth_price.len() != count
        || message.depth_size.len() != count
        || message.depth_order_priority.len() != count
        || message.exchange_order_id.len() != count
    {
        return Err(ProtocolError::ParallelFieldLength("depth_by_order"));
    }
    let previous_prices = previous_depth_prices(
        count,
        &message.prev_depth_price,
        &message.prev_depth_price_flag,
    )?;
    let mut mutations = Vec::with_capacity(count);
    for (index, previous_price) in previous_prices.iter().copied().enumerate().take(count) {
        let kind = match rti::depth_by_order::UpdateType::try_from(message.update_type[index]) {
            Ok(rti::depth_by_order::UpdateType::New) => DepthByOrderMutationKind::New,
            Ok(rti::depth_by_order::UpdateType::Change) => DepthByOrderMutationKind::Change,
            Ok(rti::depth_by_order::UpdateType::Delete) => DepthByOrderMutationKind::Delete,
            Err(_) => return Err(ProtocolError::UnknownEnum("depth_by_order.update_type")),
        };
        let book_side =
            match rti::depth_by_order::TransactionType::try_from(message.transaction_type[index]) {
                Ok(rti::depth_by_order::TransactionType::Buy) => DepthByOrderSide::Bid,
                Ok(rti::depth_by_order::TransactionType::Sell) => DepthByOrderSide::Ask,
                Err(_) => {
                    return Err(ProtocolError::UnknownEnum(
                        "depth_by_order.transaction_type",
                    ));
                }
            };
        let price = message.depth_price[index];
        if !price.is_finite() || price <= 0.0 {
            return Err(ProtocolError::InvalidNumber("depth_by_order.depth_price"));
        }
        if previous_price.is_some_and(|price| !price.is_finite() || price <= 0.0) {
            return Err(ProtocolError::InvalidNumber(
                "depth_by_order.prev_depth_price",
            ));
        }
        let order_size = u32::try_from(message.depth_size[index])
            .map_err(|_| ProtocolError::InvalidNumber("depth_by_order.depth_size"))?;
        let exchange_order_id = bounded_string(
            "depth_by_order.exchange_order_id",
            message.exchange_order_id[index].clone(),
        )?;
        mutations.push(DepthByOrderMutation {
            kind,
            side: book_side,
            price,
            previous_price,
            size: order_size,
            priority: message.depth_order_priority[index],
            exchange_order_id,
        });
    }
    Ok(DecodedMarketMessage::DepthByOrder(DepthByOrderUpdate {
        identity,
        sequence_number,
        mutations,
        timestamp: optional_timestamp(message.ssboe, message.usecs)?,
    }))
}

#[cfg(rithmic_kit)]
fn decode_depth_by_order_end(frame: &[u8]) -> Result<DecodedMarketMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::DepthByOrderEndEvent::decode(frame).map_err(|_| ProtocolError::Decode)?;
    if message.symbol.is_empty()
        || message.symbol.len() != message.exchange.len()
        || message.symbol.len() > MAX_DEPTH_BY_ORDER_MUTATIONS
    {
        return Err(ProtocolError::ParallelFieldLength("depth_by_order_end"));
    }
    let identities = message
        .symbol
        .into_iter()
        .zip(message.exchange)
        .map(|(symbol, exchange)| {
            Ok(MarketIdentity {
                symbol: bounded_string("depth_by_order_end.symbol", symbol)?,
                exchange: bounded_string("depth_by_order_end.exchange", exchange)?,
            })
        })
        .collect::<Result<Vec<_>, ProtocolError>>()?;
    let sequence_number = message.sequence_number.filter(|sequence| *sequence != 0);
    Ok(DecodedMarketMessage::DepthByOrderEnd(
        DepthByOrderEndEvent {
            identities,
            sequence_number,
            timestamp: optional_timestamp(message.ssboe, message.usecs)?,
        },
    ))
}

#[cfg(rithmic_kit)]
fn previous_depth_prices(
    count: usize,
    prices: &[f64],
    flags: &[bool],
) -> Result<Vec<Option<f64>>, ProtocolError> {
    if flags.is_empty() {
        if prices.is_empty() {
            return Ok(vec![None; count]);
        }
        if prices.len() == count {
            return Ok(prices.iter().copied().map(Some).collect());
        }
        return Err(ProtocolError::ParallelFieldLength(
            "depth_by_order.prev_depth_price",
        ));
    }
    if flags.len() != count {
        return Err(ProtocolError::ParallelFieldLength(
            "depth_by_order.prev_depth_price_flag",
        ));
    }
    if prices.len() == count {
        return Ok(flags
            .iter()
            .zip(prices)
            .map(|(present, price)| present.then_some(*price))
            .collect());
    }
    let expected = flags.iter().filter(|present| **present).count();
    if prices.len() != expected {
        return Err(ProtocolError::ParallelFieldLength(
            "depth_by_order.prev_depth_price",
        ));
    }
    let mut prices = prices.iter().copied();
    Ok(flags
        .iter()
        .map(|present| present.then(|| prices.next()).flatten())
        .collect())
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
    bounded_string(field, value)
}

#[cfg(rithmic_kit)]
fn bounded_string(field: &'static str, value: String) -> Result<String, ProtocolError> {
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
    fn decodes_depth_by_order_updates_and_snapshot_end_marker() {
        let codec = RithmicProtocolCodec;
        let update = rti::DepthByOrder {
            template_id: 160,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            sequence_number: Some(41),
            update_type: vec![
                rti::depth_by_order::UpdateType::New.into(),
                rti::depth_by_order::UpdateType::Change.into(),
            ],
            transaction_type: vec![
                rti::depth_by_order::TransactionType::Buy.into(),
                rti::depth_by_order::TransactionType::Sell.into(),
            ],
            depth_price: vec![5_100.0, 5_100.25],
            prev_depth_price: vec![5_100.5],
            prev_depth_price_flag: vec![false, true],
            depth_size: vec![4, 7],
            depth_order_priority: vec![11, 12],
            exchange_order_id: vec!["bid-1".to_string(), "ask-1".to_string()],
            ssboe: Some(1_800_000_000),
            usecs: Some(123_459),
            source_ssboe: None,
            source_usecs: None,
            source_nsecs: None,
            jop_ssboe: None,
            jop_nsecs: None,
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_market(&update).expect("DBO update decodes"),
            Some(DecodedMarketMessage::DepthByOrder(DepthByOrderUpdate {
                sequence_number: Some(41),
                mutations,
                ..
            })) if mutations.len() == 2
                && mutations[0].kind == DepthByOrderMutationKind::New
                && mutations[0].side == DepthByOrderSide::Bid
                && mutations[0].previous_price.is_none()
                && mutations[1].previous_price == Some(5_100.5)
        ));

        let end = rti::DepthByOrderEndEvent {
            template_id: 161,
            symbol: vec!["ESM7".to_string()],
            exchange: vec!["CME".to_string()],
            sequence_number: Some(41),
            ssboe: Some(1_800_000_000),
            usecs: Some(123_460),
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_market(&end).expect("DBO end marker decodes"),
            Some(DecodedMarketMessage::DepthByOrderEnd(DepthByOrderEndEvent {
                sequence_number: Some(41),
                identities,
                ..
            })) if identities == vec![MarketIdentity {
                symbol: "ESM7".to_string(),
                exchange: "CME".to_string(),
            }]
        ));
    }

    #[test]
    fn decodes_covering_depth_by_order_snapshot_frames_and_completion() {
        let codec = RithmicProtocolCodec;
        let level = rti::ResponseDepthByOrderSnapshot {
            template_id: 116,
            user_msg: Vec::new(),
            rq_handler_rp_code: vec!["0".to_string()],
            rp_code: Vec::new(),
            exchange: Some("CME".to_string()),
            symbol: Some("ESM7".to_string()),
            sequence_number: Some(40),
            depth_side: Some(rti::response_depth_by_order_snapshot::TransactionType::Buy.into()),
            depth_price: Some(5_100.0),
            depth_size: vec![2, 3],
            depth_order_priority: vec![11, 12],
            exchange_order_id: vec!["bid-1".to_string(), "bid-2".to_string()],
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_market(&level).expect("DBO snapshot level decodes"),
            Some(DecodedMarketMessage::DepthByOrderSnapshot(
                DepthByOrderSnapshotMessage::Level(DepthByOrderSnapshotLevel {
                    sequence_number: Some(40),
                    side: DepthByOrderSide::Bid,
                    price,
                    orders,
                    ..
                })
            )) if (price - 5_100.0).abs() < f64::EPSILON
                && orders.len() == 2
                && orders[0].size == 2
                && orders[1].exchange_order_id == "bid-2"
        ));

        let complete = rti::ResponseDepthByOrderSnapshot {
            template_id: 116,
            user_msg: Vec::new(),
            rq_handler_rp_code: Vec::new(),
            rp_code: vec!["0".to_string()],
            exchange: Some("CME".to_string()),
            symbol: Some("ESM7".to_string()),
            sequence_number: Some(40),
            depth_side: None,
            depth_price: None,
            depth_size: Vec::new(),
            depth_order_priority: Vec::new(),
            exchange_order_id: Vec::new(),
        }
        .encode_to_vec();
        assert!(matches!(
            codec
                .decode_market(&complete)
                .expect("DBO snapshot completion decodes"),
            Some(DecodedMarketMessage::DepthByOrderSnapshot(
                DepthByOrderSnapshotMessage::Complete {
                    accepted: true,
                    identity: Some(MarketIdentity { symbol, exchange }),
                    sequence_number: Some(40),
                }
            )) if symbol == "ESM7" && exchange == "CME"
        ));
    }

    #[test]
    fn empty_book_depth_by_order_snapshot_frame_is_skipped_not_rejected() {
        let empty = rti::ResponseDepthByOrderSnapshot {
            template_id: 116,
            user_msg: Vec::new(),
            rq_handler_rp_code: vec!["7".to_string(), "no data".to_string()],
            rp_code: Vec::new(),
            exchange: None,
            symbol: None,
            sequence_number: None,
            depth_side: None,
            depth_price: None,
            depth_size: Vec::new(),
            depth_order_priority: Vec::new(),
            exchange_order_id: Vec::new(),
        }
        .encode_to_vec();
        assert_eq!(
            RithmicProtocolCodec
                .decode_market(&empty)
                .expect("empty-book frame decodes"),
            None
        );

        let rejected_level = rti::ResponseDepthByOrderSnapshot {
            template_id: 116,
            user_msg: Vec::new(),
            rq_handler_rp_code: vec!["7".to_string(), "no data".to_string()],
            rp_code: Vec::new(),
            exchange: Some("CME".to_string()),
            symbol: Some("MNQZ6".to_string()),
            sequence_number: None,
            depth_side: Some(rti::response_depth_by_order_snapshot::TransactionType::Buy.into()),
            depth_price: Some(20_000.0),
            depth_size: vec![1],
            depth_order_priority: vec![1],
            exchange_order_id: vec!["bid-1".to_string()],
        }
        .encode_to_vec();
        assert!(matches!(
            RithmicProtocolCodec.decode_market(&rejected_level),
            Err(ProtocolError::RejectedDataFrame)
        ));
    }

    #[test]
    fn malformed_depth_by_order_snapshot_parallel_vectors_fail_closed() {
        let malformed = rti::ResponseDepthByOrderSnapshot {
            template_id: 116,
            user_msg: Vec::new(),
            rq_handler_rp_code: vec!["0".to_string()],
            rp_code: Vec::new(),
            exchange: Some("CME".to_string()),
            symbol: Some("ESM7".to_string()),
            sequence_number: Some(40),
            depth_side: Some(rti::response_depth_by_order_snapshot::TransactionType::Sell.into()),
            depth_price: Some(5_100.25),
            depth_size: vec![2, 3],
            depth_order_priority: vec![11],
            exchange_order_id: vec!["ask-1".to_string(), "ask-2".to_string()],
        }
        .encode_to_vec();
        assert!(matches!(
            RithmicProtocolCodec.decode_market(&malformed),
            Err(ProtocolError::ParallelFieldLength(
                "depth_by_order_snapshot"
            ))
        ));
    }

    #[test]
    fn depth_by_order_delete_without_sequence_number_decodes_unsequenced() {
        let update = rti::DepthByOrder {
            template_id: 160,
            symbol: Some("MNQZ6".to_string()),
            exchange: Some("CME".to_string()),
            sequence_number: None,
            update_type: vec![rti::depth_by_order::UpdateType::Delete.into()],
            transaction_type: vec![rti::depth_by_order::TransactionType::Buy.into()],
            depth_price: vec![27_600.0],
            prev_depth_price: Vec::new(),
            prev_depth_price_flag: Vec::new(),
            depth_size: vec![1],
            depth_order_priority: vec![1],
            exchange_order_id: vec!["bid-1".to_string()],
            ssboe: Some(1_790_314_093),
            usecs: None,
            source_ssboe: None,
            source_usecs: None,
            source_nsecs: None,
            jop_ssboe: None,
            jop_nsecs: None,
        }
        .encode_to_vec();
        assert!(matches!(
            RithmicProtocolCodec.decode_market(&update),
            Ok(Some(DecodedMarketMessage::DepthByOrder(
                DepthByOrderUpdate {
                    sequence_number: None,
                    ..
                }
            )))
        ));
    }

    #[test]
    fn malformed_depth_by_order_parallel_vectors_fail_closed() {
        let update = rti::DepthByOrder {
            template_id: 160,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            sequence_number: Some(9),
            update_type: vec![rti::depth_by_order::UpdateType::New.into()],
            transaction_type: Vec::new(),
            depth_price: vec![5_100.0],
            prev_depth_price: Vec::new(),
            prev_depth_price_flag: Vec::new(),
            depth_size: vec![1],
            depth_order_priority: vec![1],
            exchange_order_id: vec!["bid-1".to_string()],
            ssboe: None,
            usecs: None,
            source_ssboe: None,
            source_usecs: None,
            source_nsecs: None,
            jop_ssboe: None,
            jop_nsecs: None,
        }
        .encode_to_vec();
        assert!(matches!(
            RithmicProtocolCodec.decode_market(&update),
            Err(ProtocolError::ParallelFieldLength("depth_by_order"))
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
