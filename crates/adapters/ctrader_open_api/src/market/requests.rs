use super::{MarketDecodeError, TrendbarPeriod, account_id, check_account};
use crate::{
    ProtoMessage, codec,
    generated::{
        ProtoOaAssetClassListReq, ProtoOaAssetListReq, ProtoOaGetTickDataReq,
        ProtoOaGetTrendbarsReq, ProtoOaSubscribeDepthQuotesReq, ProtoOaSubscribeLiveTrendbarReq,
        ProtoOaSubscribeSpotsReq, ProtoOaSymbolByIdReq, ProtoOaSymbolCategoryListReq,
        ProtoOaSymbolsListReq, ProtoOaUnsubscribeDepthQuotesReq, ProtoOaUnsubscribeLiveTrendbarReq,
        ProtoOaUnsubscribeSpotsReq,
    },
    transport::Bucket,
};
use prost::Message;

/// Request symbols per subscription call stay small so one rejection is cheap.
const MAXIMUM_SYMBOLS_PER_REQUEST: usize = 64;

/// One encoded market request with its expected response and rate bucket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketRequest {
    pub payload_type: u32,
    pub response_type: u32,
    pub bucket: Bucket,
    pub payload: Vec<u8>,
}

/// Tick history side (`ProtoOAQuoteType`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QuoteSide {
    Bid,
    Ask,
}

fn symbol_ids(symbols: &[u64]) -> Result<Vec<i64>, MarketDecodeError> {
    if symbols.is_empty() || symbols.len() > MAXIMUM_SYMBOLS_PER_REQUEST {
        return Err(MarketDecodeError::LimitExceeded("symbols per request"));
    }
    symbols.iter().map(|id| symbol_id(*id)).collect()
}

fn symbol_id(symbol: u64) -> Result<i64, MarketDecodeError> {
    i64::try_from(symbol)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(MarketDecodeError::InvalidField("symbolId"))
}

fn timestamp_range(from_ms: i64, to_ms: i64) -> Result<(), MarketDecodeError> {
    // The schema bounds timestamps to 1970..=2038-01-19 in milliseconds.
    if from_ms < 0 || to_ms <= from_ms || to_ms > 2_147_483_646_000 {
        return Err(MarketDecodeError::InvalidField("timestamp range"));
    }
    Ok(())
}

impl MarketRequest {
    fn general(payload_type: u32, response_type: u32, message: &impl Message) -> Self {
        Self {
            payload_type,
            response_type,
            bucket: Bucket::General,
            payload: message.encode_to_vec(),
        }
    }

    /// # Errors
    /// Rejects an account id outside the wire range.
    pub fn symbols_list(ctid: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2114,
            2115,
            &ProtoOaSymbolsListReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                include_archived_symbols: Some(false),
            },
        ))
    }

    /// # Errors
    /// Rejects an account id outside the wire range.
    pub fn asset_list(ctid: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2112,
            2113,
            &ProtoOaAssetListReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
            },
        ))
    }

    /// `ProtoOAAssetClassListReq` (2153): the broker's asset classes.
    ///
    /// # Errors
    /// Rejects an account id outside the wire range.
    pub fn asset_classes(ctid: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2153,
            2154,
            &ProtoOaAssetClassListReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
            },
        ))
    }

    /// `ProtoOASymbolCategoryListReq` (2160): the broker's symbol categories.
    ///
    /// # Errors
    /// Rejects an account id outside the wire range.
    pub fn symbol_categories(ctid: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2160,
            2161,
            &ProtoOaSymbolCategoryListReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
            },
        ))
    }

    /// # Errors
    /// Rejects an empty or oversized symbol list.
    pub fn symbol_by_id(ctid: u64, symbols: &[u64]) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2116,
            2117,
            &ProtoOaSymbolByIdReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                symbol_id: symbol_ids(symbols)?,
            },
        ))
    }

    /// Spot timestamps are requested so quotes carry provider time.
    ///
    /// # Errors
    /// Rejects an empty or oversized symbol list.
    pub fn subscribe_spots(ctid: u64, symbols: &[u64]) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2127,
            2128,
            &ProtoOaSubscribeSpotsReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                symbol_id: symbol_ids(symbols)?,
                subscribe_to_spot_timestamp: Some(true),
            },
        ))
    }

    /// # Errors
    /// Rejects an empty or oversized symbol list.
    pub fn unsubscribe_spots(ctid: u64, symbols: &[u64]) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2129,
            2130,
            &ProtoOaUnsubscribeSpotsReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                symbol_id: symbol_ids(symbols)?,
            },
        ))
    }

    /// The server rejects this unless spots for the symbol are already subscribed.
    ///
    /// # Errors
    /// Rejects an invalid symbol id.
    pub fn subscribe_live_trendbar(
        ctid: u64,
        symbol: u64,
        period: TrendbarPeriod,
    ) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2135,
            2165,
            &ProtoOaSubscribeLiveTrendbarReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                period: period.wire(),
                symbol_id: symbol_id(symbol)?,
            },
        ))
    }

    /// # Errors
    /// Rejects an invalid symbol id.
    pub fn unsubscribe_live_trendbar(
        ctid: u64,
        symbol: u64,
        period: TrendbarPeriod,
    ) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2136,
            2166,
            &ProtoOaUnsubscribeLiveTrendbarReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                period: period.wire(),
                symbol_id: symbol_id(symbol)?,
            },
        ))
    }

    /// # Errors
    /// Rejects an empty or oversized symbol list.
    pub fn subscribe_depth(ctid: u64, symbols: &[u64]) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2156,
            2157,
            &ProtoOaSubscribeDepthQuotesReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                symbol_id: symbol_ids(symbols)?,
            },
        ))
    }

    /// # Errors
    /// Rejects an empty or oversized symbol list.
    pub fn unsubscribe_depth(ctid: u64, symbols: &[u64]) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2158,
            2159,
            &ProtoOaUnsubscribeDepthQuotesReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                symbol_id: symbol_ids(symbols)?,
            },
        ))
    }

    /// The response holds the newest bars whose open time is at or before
    /// `to_ms` (inclusive), in ascending open time; `hasMore` means older bars
    /// remain in the range. On demo, `count = n` returned `n - 1` bars.
    ///
    /// # Errors
    /// Rejects an invalid range, symbol id or zero count.
    pub fn trendbars(
        ctid: u64,
        symbol: u64,
        period: TrendbarPeriod,
        from_ms: i64,
        to_ms: i64,
        count: Option<u32>,
    ) -> Result<Self, MarketDecodeError> {
        timestamp_range(from_ms, to_ms)?;
        if count == Some(0) {
            return Err(MarketDecodeError::InvalidField("count"));
        }
        Ok(Self {
            payload_type: 2137,
            response_type: 2138,
            bucket: Bucket::Historical,
            payload: ProtoOaGetTrendbarsReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                from_timestamp: Some(from_ms),
                to_timestamp: Some(to_ms),
                period: period.wire(),
                symbol_id: symbol_id(symbol)?,
                count,
            }
            .encode_to_vec(),
        })
    }

    /// # Errors
    /// Rejects an invalid range or symbol id.
    pub fn tick_data(
        ctid: u64,
        symbol: u64,
        side: QuoteSide,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Self, MarketDecodeError> {
        timestamp_range(from_ms, to_ms)?;
        Ok(Self {
            payload_type: 2145,
            response_type: 2146,
            bucket: Bucket::Historical,
            payload: ProtoOaGetTickDataReq {
                payload_type: None,
                ctid_trader_account_id: account_id(ctid)?,
                symbol_id: symbol_id(symbol)?,
                r#type: match side {
                    QuoteSide::Bid => 1,
                    QuoteSide::Ask => 2,
                },
                from_timestamp: Some(from_ms),
                to_timestamp: Some(to_ms),
            }
            .encode_to_vec(),
        })
    }
}

/// Subscription responses (2128, 2130, 2157, 2159, 2165, 2166) carry only the
/// account id as field 2.
///
/// # Errors
/// Rejects an unexpected type, missing account id or another account.
pub fn decode_subscription_ack(
    frame: &ProtoMessage,
    expected: u32,
    ctid: u64,
) -> Result<(), MarketDecodeError> {
    if !matches!(expected, 2128 | 2130 | 2157 | 2159 | 2165 | 2166) {
        return Err(MarketDecodeError::InvalidField(
            "subscription response type",
        ));
    }
    let ack: crate::generated::ProtoOaSubscribeSpotsRes =
        codec::decode_typed(frame, expected, &[(2, "ctidTraderAccountId")], |_| Ok(()))?;
    check_account(ctid, ack.ctid_trader_account_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::fixtures::{CTID, CTID_WIRE, bytes_frame, frame};

    #[test]
    fn requests_carry_types_buckets_and_validated_arguments() {
        let spots = MarketRequest::subscribe_spots(CTID, &[1, 4]).expect("spots");
        assert_eq!((spots.payload_type, spots.response_type), (2127, 2128));
        assert_eq!(spots.bucket, Bucket::General);
        let decoded = ProtoOaSubscribeSpotsReq::decode(spots.payload.as_slice()).expect("decode");
        assert_eq!(decoded.symbol_id, vec![1, 4]);
        assert_eq!(decoded.subscribe_to_spot_timestamp, Some(true));
        let bars = MarketRequest::trendbars(CTID, 1, TrendbarPeriod::H1, 0, 1_000, Some(10))
            .expect("bars");
        assert_eq!(bars.bucket, Bucket::Historical);
        assert_eq!(
            ProtoOaGetTrendbarsReq::decode(bars.payload.as_slice())
                .expect("decode")
                .period,
            9
        );
        let ticks = MarketRequest::tick_data(CTID, 1, QuoteSide::Ask, 0, 1).expect("ticks");
        assert_eq!(
            (ticks.payload_type, ticks.bucket),
            (2145, Bucket::Historical)
        );
        let assets = MarketRequest::asset_list(CTID).expect("assets");
        assert_eq!(
            (assets.payload_type, assets.response_type, assets.bucket),
            (2112, 2113, Bucket::General)
        );
        assert_eq!(
            ProtoOaAssetListReq::decode(assets.payload.as_slice())
                .expect("decode")
                .ctid_trader_account_id,
            CTID_WIRE
        );
        for (request, types) in [
            (MarketRequest::symbol_categories(CTID), (2160, 2161)),
            (MarketRequest::asset_classes(CTID), (2153, 2154)),
        ] {
            let request = request.expect("label request");
            assert_eq!((request.payload_type, request.response_type), types);
            assert_eq!(
                ProtoOaAssetClassListReq::decode(request.payload.as_slice())
                    .expect("decode")
                    .ctid_trader_account_id,
                CTID_WIRE,
                "both requests carry only the account"
            );
        }
        assert!(MarketRequest::subscribe_spots(CTID, &[]).is_err());
        assert!(MarketRequest::subscribe_depth(CTID, &[0]).is_err());
        assert!(MarketRequest::subscribe_depth(CTID, &[1; 65]).is_err());
        assert!(MarketRequest::trendbars(CTID, 1, TrendbarPeriod::M1, 5, 5, None).is_err());
        assert!(MarketRequest::trendbars(CTID, 1, TrendbarPeriod::M1, 0, 5, Some(0)).is_err());
        assert!(MarketRequest::tick_data(CTID, 1, QuoteSide::Bid, -1, 5).is_err());
        assert!(MarketRequest::symbols_list(u64::MAX).is_err());
    }

    #[test]
    fn subscription_acks_require_the_account() {
        let ack = crate::generated::ProtoOaSubscribeDepthQuotesRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
        };
        decode_subscription_ack(&frame(2157, &ack), 2157, CTID).expect("ack");
        assert!(decode_subscription_ack(&frame(2157, &ack), 2128, CTID).is_err());
        assert!(decode_subscription_ack(&frame(2157, &ack), 2157, CTID + 1).is_err());
        assert!(decode_subscription_ack(&bytes_frame(2157, Vec::new()), 2157, CTID).is_err());
        assert!(decode_subscription_ack(&frame(2157, &ack), 2131, CTID).is_err());
    }
}
