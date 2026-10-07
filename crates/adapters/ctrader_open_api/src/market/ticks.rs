use super::{MarketDecodeError, MarketRequest, PriceScale, QuoteSide, check_account};
use crate::{
    ProtoMessage,
    codec::{self, require_nested_fields},
    generated::ProtoOaGetTickDataRes,
};

/// Demo pages were observed to stop at 10,000 ticks with `hasMore`.
pub const MAXIMUM_TICKS_PER_PAGE: usize = 16_384;
/// Upper bound on pages one paginator will request.
pub const MAXIMUM_TICK_PAGES: u32 = 64;

/// One historical tick at the symbol price scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoricalTick {
    pub timestamp_unix_ms: i64,
    pub price: i64,
}

/// One page in ascending time order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickPage {
    pub side: QuoteSide,
    pub ticks: Vec<HistoricalTick>,
    pub has_more: bool,
}

/// Decode a `ProtoOAGetTickDataRes` (2146). The wire list is newest first; the
/// first entry holds an absolute millisecond time and price, and every later
/// entry holds the (non-positive) time and signed price difference from the
/// previous entry.
///
/// # Errors
/// Rejects missing fields, another account, time running forward, non-positive
/// or inexact prices and oversized pages.
pub fn decode_tick_page(
    frame: &ProtoMessage,
    ctid: u64,
    side: QuoteSide,
    scale: PriceScale,
) -> Result<TickPage, MarketDecodeError> {
    let response: ProtoOaGetTickDataRes = codec::decode_typed(
        frame,
        2146,
        &[(2, "ctidTraderAccountId"), (4, "hasMore")],
        |_| Ok(()),
    )?;
    require_nested_fields(
        frame.payload.as_deref().unwrap_or_default(),
        3,
        &[(1, "timestamp"), (2, "tick")],
    )?;
    check_account(ctid, response.ctid_trader_account_id)?;
    if response.tick_data.len() > MAXIMUM_TICKS_PER_PAGE {
        return Err(MarketDecodeError::LimitExceeded("tick page"));
    }
    let mut ticks = Vec::with_capacity(response.tick_data.len());
    let mut previous: Option<(i64, i64)> = None;
    for entry in &response.tick_data {
        let (timestamp, price) = match previous {
            None => (entry.timestamp, entry.tick),
            Some((timestamp, price)) => {
                if entry.timestamp > 0 {
                    return Err(MarketDecodeError::InvalidField("tick timestamp order"));
                }
                (
                    timestamp
                        .checked_add(entry.timestamp)
                        .ok_or(MarketDecodeError::InvalidField("timestamp"))?,
                    price
                        .checked_add(entry.tick)
                        .ok_or(MarketDecodeError::InvalidField("tick"))?,
                )
            }
        };
        if timestamp < 0 {
            return Err(MarketDecodeError::InvalidField("timestamp"));
        }
        if price <= 0 {
            return Err(MarketDecodeError::InvalidField("tick"));
        }
        ticks.push(HistoricalTick {
            timestamp_unix_ms: timestamp,
            price: scale.from_wire(price)?,
        });
        previous = Some((timestamp, price));
    }
    ticks.reverse();
    Ok(TickPage {
        side,
        ticks,
        has_more: response.has_more,
    })
}

/// Walks one tick range backwards from its end, one bounded page at a time.
///
/// `toTimestamp` is exclusive for tick data on the demo server. Several ticks
/// can share a millisecond, so a page that has more drops its oldest
/// millisecond and the next page ends just after it, fetching that whole
/// millisecond again. Pages are returned newest first; each page is ascending.
#[derive(Debug)]
pub struct TickHistoryPaginator {
    ctid: u64,
    symbol: u64,
    side: QuoteSide,
    scale: PriceScale,
    from_ms: i64,
    next_to_ms: Option<i64>,
    pages_left: u32,
}

impl TickHistoryPaginator {
    /// # Errors
    /// Rejects an empty range or a page budget outside `1..=MAXIMUM_TICK_PAGES`.
    pub fn new(
        ctid: u64,
        symbol: u64,
        side: QuoteSide,
        scale: PriceScale,
        from_ms: i64,
        to_ms: i64,
        page_budget: u32,
    ) -> Result<Self, MarketDecodeError> {
        if page_budget == 0 || page_budget > MAXIMUM_TICK_PAGES {
            return Err(MarketDecodeError::LimitExceeded("tick page budget"));
        }
        MarketRequest::tick_data(ctid, symbol, side, from_ms, to_ms)?;
        Ok(Self {
            ctid,
            symbol,
            side,
            scale,
            from_ms,
            next_to_ms: Some(to_ms),
            pages_left: page_budget,
        })
    }

    /// The next request, or `None` once the range or page budget is exhausted.
    ///
    /// # Errors
    /// Propagates request validation errors.
    pub fn next_request(&self) -> Result<Option<MarketRequest>, MarketDecodeError> {
        match self.next_to_ms {
            Some(to_ms) if self.pages_left > 0 && to_ms > self.from_ms => Ok(Some(
                MarketRequest::tick_data(self.ctid, self.symbol, self.side, self.from_ms, to_ms)?,
            )),
            _ => Ok(None),
        }
    }

    /// Whether the range ended before the page budget did.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.next_to_ms.is_none()
    }

    /// Accept the response to the last request.
    ///
    /// # Errors
    /// Rejects decode failures, ticks outside the requested range, and a full
    /// page that cannot advance because every tick shares one millisecond.
    pub fn accept(&mut self, frame: &ProtoMessage) -> Result<TickPage, MarketDecodeError> {
        let to_ms = self
            .next_to_ms
            .filter(|_| self.pages_left > 0)
            .ok_or(MarketDecodeError::InvalidField("unexpected tick page"))?;
        let mut page = decode_tick_page(frame, self.ctid, self.side, self.scale)?;
        if page
            .ticks
            .iter()
            .any(|tick| tick.timestamp_unix_ms < self.from_ms || tick.timestamp_unix_ms >= to_ms)
        {
            return Err(MarketDecodeError::InvalidField(
                "tick outside requested range",
            ));
        }
        self.pages_left -= 1;
        let oldest = page.ticks.first().map(|tick| tick.timestamp_unix_ms);
        match oldest {
            Some(oldest) if page.has_more => {
                page.ticks.retain(|tick| tick.timestamp_unix_ms > oldest);
                if page.ticks.is_empty() {
                    return Err(MarketDecodeError::LimitExceeded(
                        "ticks sharing one millisecond",
                    ));
                }
                self.next_to_ms = Some(
                    oldest
                        .checked_add(1)
                        .ok_or(MarketDecodeError::InvalidField("timestamp"))?,
                );
            }
            _ => self.next_to_ms = None,
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generated::ProtoOaTickData,
        market::fixtures::{CTID, CTID_WIRE, bytes_frame, frame, strip, strip_nested},
    };
    use prost::Message;

    const NEWEST_MS: i64 = 1_791_410_105_078;

    fn response(entries: &[(i64, i64)], has_more: bool) -> ProtoOaGetTickDataRes {
        ProtoOaGetTickDataRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            tick_data: entries
                .iter()
                .map(|&(timestamp, tick)| ProtoOaTickData { timestamp, tick })
                .collect(),
            has_more,
        }
    }

    fn observed() -> ProtoOaGetTickDataRes {
        response(
            &[(NEWEST_MS, 111_943), (-300, -4), (-26_134, 2), (-4_475, 3)],
            false,
        )
    }

    fn five() -> PriceScale {
        PriceScale::new(5).expect("5")
    }

    #[test]
    fn tick_pages_undo_delta_encoding_into_ascending_exact_ticks() {
        let page = decode_tick_page(&frame(2146, &observed()), CTID, QuoteSide::Bid, five())
            .expect("page");
        assert!(!page.has_more);
        assert_eq!(page.side, QuoteSide::Bid);
        assert_eq!(
            page.ticks,
            vec![
                HistoricalTick {
                    timestamp_unix_ms: NEWEST_MS - 30_909,
                    price: 111_944
                },
                HistoricalTick {
                    timestamp_unix_ms: NEWEST_MS - 26_434,
                    price: 111_941
                },
                HistoricalTick {
                    timestamp_unix_ms: NEWEST_MS - 300,
                    price: 111_939
                },
                HistoricalTick {
                    timestamp_unix_ms: NEWEST_MS,
                    price: 111_943
                },
            ]
        );
        let gold = response(&[(NEWEST_MS, 401_234_000), (-10, -1_000)], false);
        let decoded = decode_tick_page(
            &frame(2146, &gold),
            CTID,
            QuoteSide::Ask,
            PriceScale::new(2).expect("2"),
        )
        .expect("gold");
        assert_eq!(decoded.ticks[0].price, 401_233);
        assert_eq!(decoded.ticks[1].price, 401_234);
    }

    #[test]
    fn tick_pages_reject_missing_fields_and_invalid_sequences() {
        let decode = |payload: Vec<u8>| {
            decode_tick_page(&bytes_frame(2146, payload), CTID, QuoteSide::Bid, five())
        };
        let payload = observed().encode_to_vec();
        assert!(decode(strip(&payload, 2)).is_err());
        assert!(decode(strip(&payload, 4)).is_err());
        assert!(decode(strip_nested(&payload, 3, 1)).is_err());
        assert!(decode(strip_nested(&payload, 3, 2)).is_err());
        assert!(decode(response(&[(NEWEST_MS, 111_943), (5, 1)], false).encode_to_vec()).is_err());
        assert!(decode(response(&[(NEWEST_MS, 3), (-1, -3)], false).encode_to_vec()).is_err());
        assert!(matches!(
            decode_tick_page(
                &frame(2146, &observed()),
                CTID,
                QuoteSide::Bid,
                PriceScale::new(4).expect("4")
            ),
            Err(MarketDecodeError::InexactPrice)
        ));
        assert!(matches!(
            decode_tick_page(&frame(2146, &observed()), CTID + 1, QuoteSide::Bid, five()),
            Err(MarketDecodeError::AccountMismatch)
        ));
    }

    #[test]
    fn paginator_walks_backwards_without_losing_shared_milliseconds() {
        let from = NEWEST_MS - 60_000;
        let mut paginator =
            TickHistoryPaginator::new(CTID, 1, QuoteSide::Bid, five(), from, NEWEST_MS + 1, 3)
                .expect("paginator");
        let first = paginator.next_request().expect("request").expect("first");
        assert_eq!(first.payload_type, 2145);
        let page = paginator
            .accept(&frame(
                2146,
                &response(&[(NEWEST_MS, 111_943), (-10, 1), (0, 1)], true),
            ))
            .expect("first page");
        assert_eq!(
            page.ticks,
            vec![HistoricalTick {
                timestamp_unix_ms: NEWEST_MS,
                price: 111_943
            }]
        );
        let second = paginator.next_request().expect("request").expect("second");
        let request = crate::generated::ProtoOaGetTickDataReq::decode(second.payload.as_slice())
            .expect("decode");
        assert_eq!(
            request.to_timestamp,
            Some(NEWEST_MS - 9),
            "exclusive end refetches the whole oldest millisecond"
        );
        assert_eq!(request.from_timestamp, Some(from));
        let page = paginator
            .accept(&frame(
                2146,
                &response(&[(NEWEST_MS - 10, 111_945), (0, -1), (-5, 0)], false),
            ))
            .expect("second page");
        assert_eq!(page.ticks.len(), 3);
        assert!(paginator.is_complete());
        assert!(paginator.next_request().expect("request").is_none());
        assert!(paginator.accept(&frame(2146, &observed())).is_err());
    }

    #[test]
    fn paginator_is_bounded_and_rejects_stalled_or_out_of_range_pages() {
        assert!(TickHistoryPaginator::new(CTID, 1, QuoteSide::Bid, five(), 0, 10, 0).is_err());
        assert!(TickHistoryPaginator::new(CTID, 1, QuoteSide::Bid, five(), 0, 10, 65).is_err());
        let mut budget =
            TickHistoryPaginator::new(CTID, 1, QuoteSide::Bid, five(), 0, NEWEST_MS + 1, 1)
                .expect("p");
        budget
            .accept(&frame(
                2146,
                &response(&[(NEWEST_MS, 111_943), (-10, 1)], true),
            ))
            .expect("page");
        assert!(budget.next_request().expect("request").is_none());
        assert!(!budget.is_complete());
        let mut stalled =
            TickHistoryPaginator::new(CTID, 1, QuoteSide::Bid, five(), 0, NEWEST_MS + 1, 2)
                .expect("p");
        assert!(
            stalled
                .accept(&frame(
                    2146,
                    &response(&[(NEWEST_MS, 111_943), (0, 1)], true)
                ))
                .is_err()
        );
        let mut range = TickHistoryPaginator::new(
            CTID,
            1,
            QuoteSide::Bid,
            five(),
            NEWEST_MS - 5,
            NEWEST_MS + 1,
            2,
        )
        .expect("p");
        assert!(range.accept(&frame(2146, &observed())).is_err());
        let mut at_end =
            TickHistoryPaginator::new(CTID, 1, QuoteSide::Bid, five(), 0, NEWEST_MS, 2).expect("p");
        assert!(
            at_end
                .accept(&frame(2146, &response(&[(NEWEST_MS, 111_943)], false)))
                .is_err(),
            "the end of the range is exclusive"
        );
    }
}
