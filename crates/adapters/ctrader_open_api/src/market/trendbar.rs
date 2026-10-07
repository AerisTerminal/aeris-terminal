use super::{MarketDecodeError, PriceScale, check_account};
use crate::{
    ProtoMessage,
    codec::{self, require_nested_fields},
    generated::{ProtoOaGetTrendbarsRes, ProtoOaTrendbar},
};
use aeris_market_data::{BarPeriod, MarketBar};

/// A demo M1 request over 30 days returned 13,999 bars with `hasMore`.
pub const MAXIMUM_TRENDBARS_PER_PAGE: usize = 16_384;

/// `ProtoOATrendbarPeriod`, M1 through MN1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrendbarPeriod {
    M1,
    M2,
    M3,
    M4,
    M5,
    M10,
    M15,
    M30,
    H1,
    H4,
    H12,
    D1,
    W1,
    MN1,
}

impl TrendbarPeriod {
    pub const ALL: [Self; 14] = [
        Self::M1,
        Self::M2,
        Self::M3,
        Self::M4,
        Self::M5,
        Self::M10,
        Self::M15,
        Self::M30,
        Self::H1,
        Self::H4,
        Self::H12,
        Self::D1,
        Self::W1,
        Self::MN1,
    ];

    #[must_use]
    pub const fn wire(self) -> i32 {
        match self {
            Self::M1 => 1,
            Self::M2 => 2,
            Self::M3 => 3,
            Self::M4 => 4,
            Self::M5 => 5,
            Self::M10 => 6,
            Self::M15 => 7,
            Self::M30 => 8,
            Self::H1 => 9,
            Self::H4 => 10,
            Self::H12 => 11,
            Self::D1 => 12,
            Self::W1 => 13,
            Self::MN1 => 14,
        }
    }

    /// # Errors
    /// Rejects values outside the enumeration.
    pub fn from_wire(value: i32) -> Result<Self, MarketDecodeError> {
        Self::ALL
            .into_iter()
            .find(|period| period.wire() == value)
            .ok_or(MarketDecodeError::InvalidField("period"))
    }

    /// The canonical period, where one exists. Observed demo FX H4, H12, D1,
    /// W1 and MN1 bars are anchored to 21:00 UTC (17:00 New York), so H4 and
    /// H12 are not UTC-epoch buckets and have no canonical time period, while
    /// D1 is a session day. M2, M4 and M10 have no canonical period either.
    #[must_use]
    pub const fn canonical(self) -> Option<BarPeriod> {
        match self {
            Self::M1 => Some(BarPeriod::Time { seconds: 60 }),
            Self::M3 => Some(BarPeriod::Time { seconds: 180 }),
            Self::M5 => Some(BarPeriod::Time { seconds: 300 }),
            Self::M15 => Some(BarPeriod::Time { seconds: 900 }),
            Self::M30 => Some(BarPeriod::Time { seconds: 1_800 }),
            Self::H1 => Some(BarPeriod::Time { seconds: 3_600 }),
            Self::D1 => Some(BarPeriod::Session { days: 1 }),
            Self::W1 => Some(BarPeriod::Week { weeks: 1 }),
            Self::MN1 => Some(BarPeriod::Month { months: 1 }),
            Self::M2 | Self::M4 | Self::M10 | Self::H4 | Self::H12 => None,
        }
    }

    #[must_use]
    pub fn for_canonical(period: BarPeriod) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.canonical() == Some(period))
    }
}

/// One decoded bar. Volume is the tick count (scale 0); cTrader reports no
/// trade count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CtraderBar {
    pub period: TrendbarPeriod,
    pub bar: MarketBar,
    pub trade_count: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrendbarPage {
    pub bars: Vec<CtraderBar>,
    pub has_more: bool,
}

/// Decode one trendbar. `close_wire` supplies the close for live bars, whose
/// `deltaClose` is absent; history bars must carry every delta.
pub(super) fn decode_bar(
    trendbar: &ProtoOaTrendbar,
    period: TrendbarPeriod,
    scale: PriceScale,
    source_sequence: u64,
    close_wire: Option<i64>,
) -> Result<CtraderBar, MarketDecodeError> {
    let low = trendbar.low.ok_or(MarketDecodeError::MissingField("low"))?;
    if low <= 0 {
        return Err(MarketDecodeError::InvalidField("low"));
    }
    let offset = |delta: Option<u64>, field| {
        delta
            .ok_or(MarketDecodeError::MissingField(field))
            .and_then(|delta| {
                i64::try_from(delta)
                    .ok()
                    .and_then(|delta| low.checked_add(delta))
                    .ok_or(MarketDecodeError::InvalidField(field))
            })
    };
    let open = offset(trendbar.delta_open, "deltaOpen")?;
    let high = offset(trendbar.delta_high, "deltaHigh")?;
    let close = match (trendbar.delta_close, close_wire) {
        (Some(delta), _) => offset(Some(delta), "deltaClose")?,
        (None, Some(close)) => close,
        (None, None) => return Err(MarketDecodeError::MissingField("deltaClose")),
    };
    let minutes = trendbar
        .utc_timestamp_in_minutes
        .ok_or(MarketDecodeError::MissingField("utcTimestampInMinutes"))?;
    if trendbar.volume < 0 {
        return Err(MarketDecodeError::InvalidField("volume"));
    }
    let seconds = i64::from(minutes) * 60;
    let bar = MarketBar {
        source_sequence,
        exchange_timestamp_seconds: seconds,
        exchange_timestamp_unix_nanos: seconds * 1_000_000_000,
        open: scale.from_wire(open)?,
        high: scale.from_wire(high)?,
        low: scale.from_wire(low)?,
        close: scale.from_wire(close)?,
        volume: trendbar.volume,
    };
    bar.validate()?;
    Ok(CtraderBar {
        period,
        bar,
        trade_count: None,
    })
}

/// Decode a `ProtoOAGetTrendbarsRes` (2138). Timestamps are bar open times;
/// bars arrive in ascending open time and `first_sequence` numbers them in
/// local ingestion order.
///
/// # Errors
/// Rejects missing fields, another account, symbol or period, inexact
/// prices, unordered bars and oversized pages.
pub fn decode_trendbar_page(
    frame: &ProtoMessage,
    ctid: u64,
    symbol: u64,
    period: TrendbarPeriod,
    scale: PriceScale,
    first_sequence: u64,
) -> Result<TrendbarPage, MarketDecodeError> {
    let response: ProtoOaGetTrendbarsRes = codec::decode_typed(
        frame,
        2138,
        &[(2, "ctidTraderAccountId"), (3, "period")],
        |_| Ok(()),
    )?;
    require_nested_fields(
        frame.payload.as_deref().unwrap_or_default(),
        5,
        &[(3, "volume")],
    )?;
    check_account(ctid, response.ctid_trader_account_id)?;
    if TrendbarPeriod::from_wire(response.period)? != period {
        return Err(MarketDecodeError::InvalidField("period"));
    }
    if response
        .symbol_id
        .is_some_and(|id| u64::try_from(id).ok() != Some(symbol))
    {
        return Err(MarketDecodeError::SymbolMismatch);
    }
    if response.trendbar.len() > MAXIMUM_TRENDBARS_PER_PAGE {
        return Err(MarketDecodeError::LimitExceeded("trendbar page"));
    }
    let has_more = response
        .has_more
        .ok_or(MarketDecodeError::MissingField("hasMore"))?;
    let mut bars = Vec::with_capacity(response.trendbar.len());
    let mut sequence = first_sequence;
    for trendbar in &response.trendbar {
        if sequence == 0 {
            return Err(MarketDecodeError::InvalidField("source sequence"));
        }
        let bar = decode_bar(trendbar, period, scale, sequence, None)?;
        if bars.last().is_some_and(|previous: &CtraderBar| {
            previous.bar.exchange_timestamp_seconds >= bar.bar.exchange_timestamp_seconds
        }) {
            return Err(MarketDecodeError::InvalidField("trendbar order"));
        }
        bars.push(bar);
        sequence = sequence
            .checked_add(1)
            .ok_or(MarketDecodeError::InvalidField("source sequence"))?;
    }
    Ok(TrendbarPage { bars, has_more })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::fixtures::{
        CTID, CTID_WIRE, EURUSD, USDJPY, bytes_frame, frame, strip, strip_nested,
    };
    use prost::Message;

    fn trendbar(
        low: i64,
        open: u64,
        high: u64,
        close: u64,
        volume: i64,
        minutes: u32,
    ) -> ProtoOaTrendbar {
        ProtoOaTrendbar {
            volume,
            period: None,
            low: Some(low),
            delta_open: Some(open),
            delta_close: Some(close),
            delta_high: Some(high),
            utc_timestamp_in_minutes: Some(minutes),
        }
    }

    fn page(
        period: TrendbarPeriod,
        symbol: i64,
        bars: Vec<ProtoOaTrendbar>,
    ) -> ProtoOaGetTrendbarsRes {
        ProtoOaGetTrendbarsRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            period: period.wire(),
            trendbar: bars,
            symbol_id: Some(symbol),
            has_more: Some(true),
            ..ProtoOaGetTrendbarsRes::default()
        }
    }

    fn eurusd_m1() -> ProtoOaGetTrendbarsRes {
        page(
            TrendbarPeriod::M1,
            EURUSD,
            vec![
                trendbar(111_931, 0, 13, 13, 5, 29_856_831),
                trendbar(111_941, 7, 8, 0, 12, 29_856_834),
            ],
        )
    }

    #[test]
    fn trendbars_decode_low_plus_deltas_at_symbol_digits() {
        let scale = PriceScale::new(5).expect("5");
        let page = decode_trendbar_page(
            &frame(2138, &eurusd_m1()),
            CTID,
            1,
            TrendbarPeriod::M1,
            scale,
            7,
        )
        .expect("page");
        assert!(page.has_more);
        let first = page.bars[0];
        assert_eq!(first.period, TrendbarPeriod::M1);
        assert_eq!(first.trade_count, None);
        assert_eq!(first.bar.source_sequence, 7);
        assert_eq!(first.bar.exchange_timestamp_seconds, 29_856_831 * 60);
        assert_eq!(
            first.bar.exchange_timestamp_unix_nanos,
            29_856_831 * 60 * 1_000_000_000
        );
        assert_eq!(
            (
                first.bar.open,
                first.bar.high,
                first.bar.low,
                first.bar.close
            ),
            (111_931, 111_944, 111_931, 111_944)
        );
        assert_eq!(first.bar.volume, 5);
        let second = page.bars[1];
        assert_eq!(second.bar.source_sequence, 8);
        assert_eq!(
            (
                second.bar.open,
                second.bar.high,
                second.bar.low,
                second.bar.close
            ),
            (111_948, 111_949, 111_941, 111_941)
        );
    }

    #[test]
    fn every_period_from_m1_to_mn1_decodes() {
        // Observed open-minute stamps: intraday bars align to their period,
        // D1/W1/MN1 open at 21:00 UTC.
        let minutes = |period| match period {
            TrendbarPeriod::D1 => 29_852_460,
            TrendbarPeriod::W1 => 29_832_300,
            TrendbarPeriod::MN1 => 29_758_860,
            _ => 29_856_600,
        };
        let jpy = PriceScale::new(3).expect("3");
        for period in TrendbarPeriod::ALL {
            let bars = vec![trendbar(
                15_790_100,
                5_300,
                8_200,
                5_500,
                4_662,
                minutes(period),
            )];
            let decoded = decode_trendbar_page(
                &frame(2138, &page(period, USDJPY, bars)),
                CTID,
                4,
                period,
                jpy,
                1,
            )
            .expect("period decodes");
            let bar = decoded.bars[0];
            assert_eq!(bar.period, period);
            assert_eq!(bar.trade_count, None);
            assert_eq!(
                (
                    bar.bar.open,
                    bar.bar.high,
                    bar.bar.low,
                    bar.bar.close,
                    bar.bar.volume
                ),
                (157_954, 157_983, 157_901, 157_956, 4_662)
            );
            assert_eq!(
                bar.bar.exchange_timestamp_seconds,
                i64::from(minutes(period)) * 60
            );
            assert_eq!(
                TrendbarPeriod::from_wire(period.wire()).expect("wire"),
                period
            );
            if let Some(canonical) = period.canonical() {
                canonical.validate().expect("canonical period is supported");
                assert_eq!(TrendbarPeriod::for_canonical(canonical), Some(period));
            }
        }
        for unanchored in [
            TrendbarPeriod::M2,
            TrendbarPeriod::M4,
            TrendbarPeriod::M10,
            TrendbarPeriod::H4,
            TrendbarPeriod::H12,
        ] {
            assert_eq!(unanchored.canonical(), None);
        }
        assert_eq!(
            TrendbarPeriod::for_canonical(BarPeriod::Time { seconds: 14_400 }),
            None
        );
        assert!(TrendbarPeriod::from_wire(15).is_err());
    }

    #[test]
    fn trendbar_pages_reject_missing_fields_and_mismatches() {
        let scale = PriceScale::new(5).expect("5");
        let decode = |payload: Vec<u8>| {
            decode_trendbar_page(
                &bytes_frame(2138, payload),
                CTID,
                1,
                TrendbarPeriod::M1,
                scale,
                1,
            )
        };
        let payload = eurusd_m1().encode_to_vec();
        assert!(decode(strip(&payload, 2)).is_err());
        assert!(decode(strip(&payload, 3)).is_err());
        assert!(matches!(
            decode(strip(&payload, 7)),
            Err(MarketDecodeError::MissingField("hasMore"))
        ));
        for inner in [3, 5, 6, 7, 8, 9] {
            assert!(
                decode(strip_nested(&payload, 5, inner)).is_err(),
                "field {inner}"
            );
        }
        assert!(matches!(
            decode_trendbar_page(
                &frame(2138, &eurusd_m1()),
                CTID,
                4,
                TrendbarPeriod::M1,
                scale,
                1
            ),
            Err(MarketDecodeError::SymbolMismatch)
        ));
        assert!(
            decode_trendbar_page(
                &frame(2138, &eurusd_m1()),
                CTID,
                1,
                TrendbarPeriod::M5,
                scale,
                1
            )
            .is_err()
        );
        let mut unordered = eurusd_m1();
        unordered.trendbar.reverse();
        assert!(decode(unordered.encode_to_vec()).is_err());
        assert!(matches!(
            decode_trendbar_page(
                &frame(2138, &eurusd_m1()),
                CTID,
                1,
                TrendbarPeriod::M1,
                PriceScale::new(4).expect("4"),
                1
            ),
            Err(MarketDecodeError::InexactPrice)
        ));
    }
}
