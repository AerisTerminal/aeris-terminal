use super::{
    CTRADER_PROVIDER_ID, CtraderBar, EventStamp, MarketDecodeError, PriceScale, TrendbarPeriod,
    check_account, trendbar::decode_bar,
};
use crate::{
    ProtoMessage,
    codec::{self, require_nested_fields},
    generated::{ProtoOaDepthEvent, ProtoOaSpotEvent},
};
use aeris_market_data::{
    BookSide, DepthLevel, DepthSnapshot, EventMetadata, MarketDataValidationError,
    QualifiedTimestamp, QuoteLevel, TopOfBookQuote,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Symbols with live state on one account stream.
pub const MAXIMUM_STREAM_SYMBOLS: usize = 256;
/// Individual depth quotes retained per symbol (about ten observed on demo EURUSD).
pub const MAXIMUM_DEPTH_QUOTES: usize = 1_024;
/// Every level holds at least one quote, so a full book never needs truncation.
pub const MAXIMUM_DEPTH_LEVELS: usize = MAXIMUM_DEPTH_QUOTES;

/// Canonical identity and price scale for one subscribed symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolStream {
    pub instrument_id: String,
    pub entitlement_id: String,
    pub session_generation: u64,
    pub scale: PriceScale,
}

#[derive(Clone, Copy, Debug)]
struct DepthQuote {
    side: BookSide,
    price: i64,
    size: i64,
}

#[derive(Debug)]
struct SymbolState {
    stream: SymbolStream,
    bid: Option<i64>,
    ask: Option<i64>,
    depth: HashMap<u64, DepthQuote>,
}

/// The result of one spot event: a quote when either side changed, and any
/// live trendbars it carried. Demo sessions regularly publish a side that
/// locks or crosses the retained other side (each side updates separately);
/// such a BBO cannot be a canonical quote, so `quote` is `None` and `crossed`
/// is set while the retained state still follows the provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpotUpdate {
    pub symbol_id: u64,
    pub quote: Option<TopOfBookQuote>,
    pub crossed: bool,
    pub live_bars: Vec<CtraderBar>,
}

/// The result of one depth event. Aggregated liquidity-provider depth can be
/// locked or crossed (observed on demo EURUSD); that book is retained but has
/// no canonical snapshot until a later event uncrosses it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DepthUpdate {
    Snapshot(DepthSnapshot),
    Crossed,
}

/// Retained best bid/offer and depth book per subscribed symbol of one
/// account. Spot events carry only the side that changed, so the other side is
/// retained; depth events add, replace and delete individual quotes by id.
#[derive(Debug)]
pub struct MarketStreams {
    ctid: u64,
    symbols: HashMap<u64, SymbolState>,
}

impl MarketStreams {
    #[must_use]
    pub fn new(ctid: u64) -> Self {
        Self {
            ctid,
            symbols: HashMap::new(),
        }
    }

    /// Register or replace a symbol. Replacing discards retained quotes and
    /// depth, because they belong to the previous subscription.
    ///
    /// # Errors
    /// Rejects a zero generation, invalid ids, or more than
    /// `MAXIMUM_STREAM_SYMBOLS` symbols.
    pub fn register(
        &mut self,
        symbol_id: u64,
        stream: SymbolStream,
    ) -> Result<(), MarketDecodeError> {
        if symbol_id == 0 || i64::try_from(symbol_id).is_err() {
            return Err(MarketDecodeError::InvalidField("symbolId"));
        }
        if stream.session_generation == 0 {
            return Err(MarketDecodeError::InvalidField("session generation"));
        }
        if !self.symbols.contains_key(&symbol_id) && self.symbols.len() >= MAXIMUM_STREAM_SYMBOLS {
            return Err(MarketDecodeError::LimitExceeded("stream symbols"));
        }
        self.symbols.insert(
            symbol_id,
            SymbolState {
                stream,
                bid: None,
                ask: None,
                depth: HashMap::new(),
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, symbol_id: u64) {
        self.symbols.remove(&symbol_id);
    }

    /// Discard the depth book before resubscribing depth for a symbol.
    pub fn reset_depth(&mut self, symbol_id: u64) {
        if let Some(state) = self.symbols.get_mut(&symbol_id) {
            state.depth.clear();
        }
    }

    fn metadata(
        stream: &SymbolStream,
        stamp: EventStamp,
        provider_unix_nanos: Option<i64>,
    ) -> EventMetadata {
        EventMetadata {
            provider_id: CTRADER_PROVIDER_ID.to_string(),
            instrument_id: stream.instrument_id.clone(),
            entitlement_id: stream.entitlement_id.clone(),
            source_sequence: stamp.source_sequence,
            session_generation: stream.session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: None,
                provider_unix_nanos,
                received_unix_nanos: stamp.received_unix_nanos,
            },
        }
    }

    fn state(&mut self, symbol_id: i64) -> Result<(u64, &mut SymbolState), MarketDecodeError> {
        let symbol = u64::try_from(symbol_id).map_err(|_| MarketDecodeError::UnknownSymbol)?;
        self.symbols
            .get_mut(&symbol)
            .map(|state| (symbol, state))
            .ok_or(MarketDecodeError::UnknownSymbol)
    }

    /// Apply a `ProtoOASpotEvent` (2131). Live trendbars inside the event have
    /// no `deltaClose`; their close is the current bid, because cTrader builds
    /// bars from bid prices (observed: each live high and low equal a bid).
    ///
    /// # Errors
    /// Rejects missing fields, another account, unknown symbols, inexact
    /// prices and invalid bars. State is unchanged on error.
    pub fn apply_spot(
        &mut self,
        frame: &ProtoMessage,
        stamp: EventStamp,
    ) -> Result<SpotUpdate, MarketDecodeError> {
        let event: ProtoOaSpotEvent = codec::decode_typed(
            frame,
            2131,
            &[(2, "ctidTraderAccountId"), (3, "symbolId")],
            |_| Ok(()),
        )?;
        require_nested_fields(
            frame.payload.as_deref().unwrap_or_default(),
            6,
            &[(3, "volume")],
        )?;
        check_account(self.ctid, event.ctid_trader_account_id)?;
        let (symbol_id, state) = self.state(event.symbol_id)?;
        let scale = state.stream.scale;
        let wire = |value: Option<u64>| {
            value
                .map(|price| {
                    scale.from_wire_positive(price)?;
                    i64::try_from(price).map_err(|_| MarketDecodeError::InvalidField("price"))
                })
                .transpose()
        };
        let bid = wire(event.bid)?.or(state.bid);
        let ask = wire(event.ask)?.or(state.ask);
        let provider_unix_nanos = event
            .timestamp
            .map(|millis| {
                millis
                    .checked_mul(1_000_000)
                    .filter(|nanos| *nanos >= 0)
                    .ok_or(MarketDecodeError::InvalidField("timestamp"))
            })
            .transpose()?;
        // Spot events carry prices only; sizes come from depth events.
        let level = |price: Option<i64>| {
            price
                .map(|price| {
                    Ok::<_, MarketDecodeError>(QuoteLevel {
                        price: scale.from_wire(price)?,
                        quantity: None,
                        order_count: None,
                    })
                })
                .transpose()
        };
        let (quote, crossed) = if event.bid.is_some() || event.ask.is_some() {
            let quote = TopOfBookQuote {
                metadata: Self::metadata(&state.stream, stamp, provider_unix_nanos),
                bid: level(bid)?,
                ask: level(ask)?,
            };
            match quote.validate() {
                Ok(()) => (Some(quote), false),
                Err(MarketDataValidationError::InvalidQuote) => (None, true),
                Err(error) => return Err(error.into()),
            }
        } else {
            (None, false)
        };
        let live_bars = event
            .trendbar
            .iter()
            .map(|trendbar| {
                let period = TrendbarPeriod::from_wire(
                    trendbar
                        .period
                        .ok_or(MarketDecodeError::MissingField("period"))?,
                )?;
                decode_bar(trendbar, period, scale, stamp.source_sequence, bid)
            })
            .collect::<Result<Vec<_>, _>>()?;
        state.bid = bid;
        state.ask = ask;
        Ok(SpotUpdate {
            symbol_id,
            quote,
            crossed,
            live_bars,
        })
    }

    /// Apply a `ProtoOADepthEvent` (2155) and return the full book. The first
    /// event after subscribing carries the whole book as new quotes; a quote id
    /// that already exists is replaced. Deleting an unknown id means the book
    /// lost continuity: it is cleared and the caller must resubscribe depth.
    ///
    /// # Errors
    /// Rejects missing fields, another account, unknown symbols, malformed
    /// quotes, oversized books and unknown deletions.
    pub fn apply_depth(
        &mut self,
        frame: &ProtoMessage,
        stamp: EventStamp,
    ) -> Result<DepthUpdate, MarketDecodeError> {
        let event: ProtoOaDepthEvent = codec::decode_typed(
            frame,
            2155,
            &[(2, "ctidTraderAccountId"), (3, "symbolId")],
            |_| Ok(()),
        )?;
        require_nested_fields(
            frame.payload.as_deref().unwrap_or_default(),
            4,
            &[(1, "id"), (3, "size")],
        )?;
        check_account(self.ctid, event.ctid_trader_account_id)?;
        let symbol =
            i64::try_from(event.symbol_id).map_err(|_| MarketDecodeError::UnknownSymbol)?;
        let (_, state) = self.state(symbol)?;
        let scale = state.stream.scale;
        let mut added = Vec::with_capacity(event.new_quotes.len());
        for quote in &event.new_quotes {
            let (book_side, wire_price) = match (quote.bid, quote.ask) {
                (Some(bid), None) => (BookSide::Bid, bid),
                (None, Some(ask)) => (BookSide::Ask, ask),
                _ => return Err(MarketDecodeError::InvalidField("depth quote side")),
            };
            let quantity = i64::try_from(quote.size)
                .ok()
                .filter(|quantity| *quantity > 0)
                .ok_or(MarketDecodeError::InvalidField("size"))?;
            added.push((
                quote.id,
                DepthQuote {
                    side: book_side,
                    price: scale.from_wire_positive(wire_price)?,
                    size: quantity,
                },
            ));
        }
        let deleted: HashSet<u64> = event.deleted_quotes.iter().copied().collect();
        if deleted.iter().any(|id| !state.depth.contains_key(id)) {
            state.depth.clear();
            return Err(MarketDecodeError::UnknownDepthQuote);
        }
        let added_ids: HashSet<u64> = added.iter().map(|(id, _)| *id).collect();
        let retained = state
            .depth
            .keys()
            .filter(|id| !deleted.contains(id) && !added_ids.contains(id))
            .count();
        if retained + added_ids.len() > MAXIMUM_DEPTH_QUOTES {
            state.depth.clear();
            return Err(MarketDecodeError::LimitExceeded("depth book"));
        }
        for id in &deleted {
            state.depth.remove(id);
        }
        state.depth.extend(added);
        let mut bids = BTreeMap::<i64, (i64, u32)>::new();
        let mut asks = BTreeMap::<i64, (i64, u32)>::new();
        for quote in state.depth.values() {
            let side = match quote.side {
                BookSide::Bid => &mut bids,
                BookSide::Ask => &mut asks,
            };
            let level = side.entry(quote.price).or_insert((0, 0));
            level.0 = level
                .0
                .checked_add(quote.size)
                .ok_or(MarketDecodeError::InvalidField("size"))?;
            level.1 += 1;
        }
        let levels = |entries: Vec<(i64, (i64, u32))>| {
            entries
                .into_iter()
                .map(|(price, (quantity, count))| DepthLevel {
                    price,
                    quantity,
                    order_count: Some(count),
                })
                .collect::<Vec<_>>()
        };
        let snapshot = DepthSnapshot {
            metadata: Self::metadata(&state.stream, stamp, None),
            bids: levels(bids.into_iter().rev().collect()),
            asks: levels(asks.into_iter().collect()),
        };
        // Levels are built sorted and unique, so `InvalidDepth` here means
        // only that the best bid is at or above the best ask.
        match snapshot.validate(MAXIMUM_DEPTH_LEVELS) {
            Ok(()) => Ok(DepthUpdate::Snapshot(snapshot)),
            Err(MarketDataValidationError::InvalidDepth) => Ok(DepthUpdate::Crossed),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generated::{ProtoOaDepthQuote, ProtoOaTrendbar},
        market::fixtures::{
            CTID, CTID_WIRE, EURUSD, USDJPY, bytes_frame, frame, strip, strip_nested,
        },
    };
    use prost::Message;

    const SPOT_MS: i64 = 1_791_410_109_650;

    fn stream(digits: i32) -> SymbolStream {
        SymbolStream {
            instrument_id: format!("ctrader:demo:ctid:{digits}"),
            entitlement_id: "ctrader:demo".into(),
            session_generation: 3,
            scale: PriceScale::new(digits).expect("digits"),
        }
    }

    fn streams() -> MarketStreams {
        let mut streams = MarketStreams::new(CTID);
        streams.register(1, stream(5)).expect("eurusd");
        streams.register(4, stream(3)).expect("usdjpy");
        streams
    }

    fn stamp(sequence: u64) -> EventStamp {
        EventStamp {
            source_sequence: sequence,
            received_unix_nanos: 1_791_410_110_000_000_000,
        }
    }

    fn spot(
        symbol: i64,
        bid: Option<u64>,
        ask: Option<u64>,
        trendbar: Vec<ProtoOaTrendbar>,
    ) -> ProtoOaSpotEvent {
        ProtoOaSpotEvent {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            symbol_id: symbol,
            bid,
            ask,
            trendbar,
            session_close: None,
            timestamp: Some(SPOT_MS),
        }
    }

    fn live_m1(volume: i64, low: i64, open: u64, high: u64) -> ProtoOaTrendbar {
        ProtoOaTrendbar {
            volume,
            period: Some(1),
            low: Some(low),
            delta_open: Some(open),
            delta_close: None,
            delta_high: Some(high),
            utc_timestamp_in_minutes: Some(29_856_835),
        }
    }

    #[test]
    fn spot_events_build_exact_bbo_and_retain_the_unchanged_side() {
        let mut streams = streams();
        let first = streams
            .apply_spot(
                &frame(
                    2131,
                    &spot(EURUSD, Some(111_945), Some(111_997), Vec::new()),
                ),
                stamp(1),
            )
            .expect("first");
        let quote = first.quote.expect("quote");
        assert_eq!(quote.bid.map(|level| level.price), Some(111_945));
        assert_eq!(quote.ask.map(|level| level.price), Some(111_997));
        assert_eq!(quote.bid.and_then(|level| level.quantity), None);
        assert_eq!(quote.ask.and_then(|level| level.quantity), None);
        assert_eq!(quote.metadata.provider_id, "ctrader");
        assert_eq!(quote.metadata.session_generation, 3);
        assert_eq!(
            quote.metadata.timestamps.provider_unix_nanos,
            Some(SPOT_MS * 1_000_000)
        );
        let bid_only = streams
            .apply_spot(
                &frame(2131, &spot(EURUSD, Some(111_930), None, Vec::new())),
                stamp(2),
            )
            .expect("bid only")
            .quote
            .expect("quote");
        assert_eq!(bid_only.bid.map(|level| level.price), Some(111_930));
        assert_eq!(bid_only.ask.map(|level| level.price), Some(111_997));
        let ask_only = streams
            .apply_spot(
                &frame(2131, &spot(EURUSD, None, Some(111_990), Vec::new())),
                stamp(3),
            )
            .expect("ask only")
            .quote
            .expect("quote");
        assert_eq!(ask_only.bid.map(|level| level.price), Some(111_930));
        assert_eq!(ask_only.ask.map(|level| level.price), Some(111_990));
        let jpy = streams
            .apply_spot(
                &frame(
                    2131,
                    &spot(USDJPY, Some(15_802_300), Some(15_810_200), Vec::new()),
                ),
                stamp(4),
            )
            .expect("jpy")
            .quote
            .expect("quote");
        assert_eq!(jpy.bid.map(|level| level.price), Some(158_023));
        assert_eq!(jpy.ask.map(|level| level.price), Some(158_102));
    }

    #[test]
    fn spot_events_reject_unknown_symbols_and_bad_prices_without_mutation() {
        let mut streams = streams();
        assert!(matches!(
            streams.apply_spot(&frame(2131, &spot(41, Some(1), None, Vec::new())), stamp(1)),
            Err(MarketDecodeError::UnknownSymbol)
        ));
        streams
            .apply_spot(
                &frame(
                    2131,
                    &spot(USDJPY, Some(15_802_300), Some(15_810_200), Vec::new()),
                ),
                stamp(1),
            )
            .expect("jpy");
        assert!(matches!(
            streams.apply_spot(
                &frame(2131, &spot(USDJPY, Some(15_802_301), None, Vec::new())),
                stamp(2)
            ),
            Err(MarketDecodeError::InexactPrice)
        ));
        let retained = streams
            .apply_spot(
                &frame(2131, &spot(USDJPY, None, Some(15_810_100), Vec::new())),
                stamp(4),
            )
            .expect("retained")
            .quote
            .expect("quote");
        assert_eq!(retained.bid.map(|level| level.price), Some(158_023));
        let mut other = spot(EURUSD, Some(111_945), None, Vec::new());
        other.ctid_trader_account_id = CTID_WIRE + 1;
        assert!(matches!(
            streams.apply_spot(&frame(2131, &other), stamp(5)),
            Err(MarketDecodeError::AccountMismatch)
        ));
    }

    #[test]
    fn locked_or_crossed_spots_are_flagged_and_still_tracked() {
        let mut streams = streams();
        streams
            .apply_spot(
                &frame(
                    2131,
                    &spot(USDJPY, Some(15_801_200), Some(15_801_300), Vec::new()),
                ),
                stamp(1),
            )
            .expect("initial");
        // Observed: the bid moved onto the retained ask before the ask update.
        let locked = streams
            .apply_spot(
                &frame(2131, &spot(USDJPY, Some(15_801_300), None, Vec::new())),
                stamp(2),
            )
            .expect("locked is not a decode error");
        assert!(locked.crossed);
        assert_eq!(locked.quote, None);
        let crossed = streams
            .apply_spot(
                &frame(2131, &spot(USDJPY, Some(15_801_500), None, Vec::new())),
                stamp(3),
            )
            .expect("crossed");
        assert!(crossed.crossed);
        let recovered = streams
            .apply_spot(
                &frame(2131, &spot(USDJPY, None, Some(15_801_600), Vec::new())),
                stamp(4),
            )
            .expect("recovered");
        assert!(!recovered.crossed);
        let quote = recovered.quote.expect("quote");
        assert_eq!(quote.bid.map(|level| level.price), Some(158_015));
        assert_eq!(quote.ask.map(|level| level.price), Some(158_016));
    }

    #[test]
    fn spot_events_reject_missing_required_fields() {
        let mut streams = streams();
        let payload = spot(
            EURUSD,
            Some(111_945),
            Some(111_997),
            vec![live_m1(3, 111_939, 0, 6)],
        )
        .encode_to_vec();
        for stripped in [
            strip(&payload, 2),
            strip(&payload, 3),
            strip_nested(&payload, 6, 3),
            strip_nested(&payload, 6, 4),
        ] {
            assert!(
                streams
                    .apply_spot(&bytes_frame(2131, stripped), stamp(1))
                    .is_err()
            );
        }
    }

    #[test]
    fn live_trendbars_close_at_the_current_bid() {
        let mut streams = streams();
        let update = streams
            .apply_spot(
                &frame(
                    2131,
                    &spot(
                        EURUSD,
                        Some(111_945),
                        Some(111_997),
                        vec![live_m1(3, 111_939, 0, 6)],
                    ),
                ),
                stamp(9),
            )
            .expect("live");
        assert_eq!(update.symbol_id, 1);
        let bar = update.live_bars[0];
        assert_eq!(bar.period, TrendbarPeriod::M1);
        assert_eq!(bar.trade_count, None);
        assert_eq!(bar.bar.source_sequence, 9);
        assert_eq!(bar.bar.exchange_timestamp_seconds, 29_856_835 * 60);
        assert_eq!(
            (
                bar.bar.open,
                bar.bar.high,
                bar.bar.low,
                bar.bar.close,
                bar.bar.volume
            ),
            (111_939, 111_945, 111_939, 111_945, 3)
        );
        let trendbar_only = streams
            .apply_spot(
                &frame(
                    2131,
                    &spot(EURUSD, None, None, vec![live_m1(4, 111_930, 9, 15)]),
                ),
                stamp(10),
            )
            .expect("trendbar only");
        assert_eq!(trendbar_only.quote, None);
        assert_eq!(
            trendbar_only.live_bars[0].bar.close, 111_945,
            "close is the retained bid"
        );
        assert!(
            streams
                .apply_spot(
                    &frame(
                        2131,
                        &spot(EURUSD, None, None, vec![live_m1(4, 111_930, 9, 14)])
                    ),
                    stamp(11)
                )
                .is_err(),
            "a retained bid above the reported high is inconsistent"
        );
        let next = streams
            .apply_spot(
                &frame(
                    2131,
                    &spot(
                        EURUSD,
                        Some(111_930),
                        None,
                        vec![live_m1(4, 111_930, 9, 15)],
                    ),
                ),
                stamp(11),
            )
            .expect("next");
        assert!(next.quote.is_some());
        assert_eq!(next.live_bars[0].bar.close, 111_930);
        assert_eq!(next.live_bars[0].bar.open, 111_939);
        let mut fresh = MarketStreams::new(CTID);
        fresh.register(1, stream(5)).expect("register");
        assert!(matches!(
            fresh.apply_spot(
                &frame(
                    2131,
                    &spot(EURUSD, None, None, vec![live_m1(1, 111_930, 0, 0)])
                ),
                stamp(1)
            ),
            Err(MarketDecodeError::MissingField("deltaClose"))
        ));
    }

    fn book(update: DepthUpdate) -> DepthSnapshot {
        match update {
            DepthUpdate::Snapshot(snapshot) => snapshot,
            DepthUpdate::Crossed => panic!("book is unexpectedly crossed"),
        }
    }

    fn quote(id: u64, size: u64, bid: Option<u64>, ask: Option<u64>) -> ProtoOaDepthQuote {
        ProtoOaDepthQuote { id, size, bid, ask }
    }

    fn depth(new_quotes: Vec<ProtoOaDepthQuote>, deleted_quotes: Vec<u64>) -> ProtoOaDepthEvent {
        ProtoOaDepthEvent {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            symbol_id: 1,
            new_quotes,
            deleted_quotes,
        }
    }

    fn initial_book() -> ProtoOaDepthEvent {
        depth(
            vec![
                quote(585_046_564, 3_000_000, None, Some(112_007)),
                quote(585_046_563, 100_000, None, Some(111_997)),
                quote(585_046_562, 5_000_000, None, Some(112_010)),
                quote(585_046_561, 500_000, None, Some(111_999)),
                quote(585_046_560, 400_000, None, Some(111_999)),
                quote(585_154_848, 100_000, Some(111_945), None),
                quote(585_154_849, 1_000_000, Some(111_939), None),
                quote(585_154_850, 3_000_000, Some(111_926), None),
            ],
            Vec::new(),
        )
    }

    #[test]
    fn depth_events_rebuild_a_sorted_aggregated_snapshot() {
        let mut streams = streams();
        let snapshot = book(
            streams
                .apply_depth(&frame(2155, &initial_book()), stamp(1))
                .expect("book"),
        );
        let prices = |levels: &[DepthLevel]| {
            levels
                .iter()
                .map(|level| (level.price, level.quantity, level.order_count))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            prices(&snapshot.bids),
            vec![
                (111_945, 100_000, Some(1)),
                (111_939, 1_000_000, Some(1)),
                (111_926, 3_000_000, Some(1))
            ]
        );
        assert_eq!(
            prices(&snapshot.asks),
            vec![
                (111_997, 100_000, Some(1)),
                (111_999, 900_000, Some(2)),
                (112_007, 3_000_000, Some(1)),
                (112_010, 5_000_000, Some(1))
            ]
        );
        assert_eq!(snapshot.metadata.source_sequence, 1);
        let next = book(
            streams
                .apply_depth(
                    &frame(
                        2155,
                        &depth(
                            vec![
                                quote(585_160_373, 100_000, Some(111_930), None),
                                quote(585_046_562, 1_000_000, None, Some(112_010)),
                            ],
                            vec![585_154_848, 585_046_561],
                        ),
                    ),
                    stamp(2),
                )
                .expect("delta"),
        );
        assert_eq!(prices(&next.bids)[0], (111_939, 1_000_000, Some(1)));
        assert_eq!(prices(&next.bids).len(), 3);
        assert_eq!(
            prices(&next.asks),
            vec![
                (111_997, 100_000, Some(1)),
                (111_999, 400_000, Some(1)),
                (112_007, 3_000_000, Some(1)),
                (112_010, 1_000_000, Some(1))
            ]
        );
    }

    #[test]
    fn depth_unknown_deletes_clear_the_book_and_require_resubscription() {
        let mut streams = streams();
        streams
            .apply_depth(&frame(2155, &initial_book()), stamp(1))
            .expect("book");
        assert!(matches!(
            streams.apply_depth(&frame(2155, &depth(Vec::new(), vec![7])), stamp(2)),
            Err(MarketDecodeError::UnknownDepthQuote)
        ));
        let rebuilt = book(
            streams
                .apply_depth(
                    &frame(
                        2155,
                        &depth(vec![quote(1, 100_000, Some(111_940), None)], Vec::new()),
                    ),
                    stamp(3),
                )
                .expect("after reset"),
        );
        assert_eq!(rebuilt.bids.len(), 1);
        assert_eq!(rebuilt.asks, [] as [DepthLevel; 0]);
        streams.reset_depth(1);
        assert!(matches!(
            streams.apply_depth(&frame(2155, &depth(Vec::new(), vec![1])), stamp(4)),
            Err(MarketDecodeError::UnknownDepthQuote)
        ));
    }

    #[test]
    fn depth_book_size_is_bounded() {
        let mut streams = streams();
        let quotes = (1..=MAXIMUM_DEPTH_QUOTES as u64)
            .map(|id| quote(id, 100_000, Some(100_000 + id), None))
            .collect();
        let full = book(
            streams
                .apply_depth(&frame(2155, &depth(quotes, Vec::new())), stamp(1))
                .expect("full"),
        );
        assert_eq!(full.bids.len(), MAXIMUM_DEPTH_QUOTES);
        assert!(matches!(
            streams.apply_depth(
                &frame(
                    2155,
                    &depth(vec![quote(5_000, 1, Some(90_000), None)], Vec::new())
                ),
                stamp(2)
            ),
            Err(MarketDecodeError::LimitExceeded("depth book"))
        ));
        let replaced = book(
            streams
                .apply_depth(
                    &frame(
                        2155,
                        &depth(vec![quote(9, 1, Some(90_000), None)], Vec::new()),
                    ),
                    stamp(3),
                )
                .expect("cleared book accepts new quotes"),
        );
        assert_eq!(replaced.bids.len(), 1);
    }

    #[test]
    fn locked_depth_is_retained_and_reported_until_uncrossed() {
        let mut streams = streams();
        book(
            streams
                .apply_depth(&frame(2155, &initial_book()), stamp(1))
                .expect("book"),
        );
        // Observed shape: a liquidity provider's bid lands on another's ask.
        let locked = streams
            .apply_depth(
                &frame(
                    2155,
                    &depth(vec![quote(10, 100_000, Some(111_997), None)], Vec::new()),
                ),
                stamp(2),
            )
            .expect("locked is not a decode error");
        assert_eq!(locked, DepthUpdate::Crossed);
        let uncrossed = book(
            streams
                .apply_depth(
                    &frame(2155, &depth(Vec::new(), vec![585_046_563])),
                    stamp(3),
                )
                .expect("uncrossed"),
        );
        assert_eq!(uncrossed.bids[0].price, 111_997);
        assert_eq!(uncrossed.asks[0].price, 111_999);
    }

    #[test]
    fn depth_events_reject_malformed_quotes_and_missing_fields() {
        let mut streams = streams();
        let payload = initial_book().encode_to_vec();
        for stripped in [
            strip(&payload, 2),
            strip(&payload, 3),
            strip_nested(&payload, 4, 1),
            strip_nested(&payload, 4, 3),
        ] {
            assert!(
                streams
                    .apply_depth(&bytes_frame(2155, stripped), stamp(1))
                    .is_err()
            );
        }
        for bad in [
            quote(1, 100_000, Some(111_940), Some(111_950)),
            quote(1, 100_000, None, None),
            quote(1, 0, Some(111_940), None),
        ] {
            assert!(
                streams
                    .apply_depth(&frame(2155, &depth(vec![bad], Vec::new())), stamp(1))
                    .is_err()
            );
        }
        let mut jpy = depth(vec![quote(1, 100_000, Some(15_802_301), None)], Vec::new());
        jpy.symbol_id = 4;
        assert!(matches!(
            streams.apply_depth(&frame(2155, &jpy), stamp(1)),
            Err(MarketDecodeError::InexactPrice)
        ));
        let mut unknown = initial_book();
        unknown.symbol_id = 41;
        assert!(matches!(
            streams.apply_depth(&frame(2155, &unknown), stamp(1)),
            Err(MarketDecodeError::UnknownSymbol)
        ));
    }

    #[test]
    fn registration_is_bounded_and_validated() {
        let mut streams = MarketStreams::new(CTID);
        for id in 1..=MAXIMUM_STREAM_SYMBOLS as u64 {
            streams.register(id, stream(5)).expect("register");
        }
        assert!(streams.register(10_000, stream(5)).is_err());
        streams
            .register(1, stream(5))
            .expect("replacing an existing symbol is allowed");
        streams.remove(1);
        streams
            .register(10_000, stream(5))
            .expect("space after removal");
        let mut zero = stream(5);
        zero.session_generation = 0;
        assert!(MarketStreams::new(CTID).register(1, zero).is_err());
        assert!(MarketStreams::new(CTID).register(0, stream(5)).is_err());
    }
}
