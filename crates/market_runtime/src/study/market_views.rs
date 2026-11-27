use crate::RetainedMarketTrade;
use aeris_market_data::{
    AggressorSide, BarSeriesKey, DepthLevel, OrderBook, OrderBookState, QuoteLevel, TopOfBookQuote,
};
use aeris_market_engine::{MarketStream, SeriesView, StreamRequirements};
use std::{collections::VecDeque, num::NonZeroU64};

/// Stable runtime identity for one study instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudyInstanceId(pub(super) NonZeroU64);

impl StudyInstanceId {
    /// Creates a runtime identity from a non-zero numeric form.
    #[must_use]
    pub fn try_from_u64(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    /// Returns the non-zero numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Returns one output identity for this study instance.
    #[must_use]
    pub const fn output(self, output_index: usize) -> StudyOutputId {
        StudyOutputId {
            study_id: self,
            output_index,
        }
    }
}

/// Stable output identity within one study instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudyOutputId {
    pub study_id: StudyInstanceId,
    pub output_index: usize,
}

/// One resolved canonical market input and its exact upstream stream requirements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyMarketInput {
    pub series: BarSeriesKey,
    pub streams: StreamRequirements,
}

/// Canonical OHLCV field exposed by a native study market-series view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyBarField {
    Open,
    High,
    Low,
    Close,
    Volume,
}

/// Segmented native study view over one canonical `MarketEngine` series.
///
/// Covering history and completed chunks are shared; only the bounded open
/// chunk is copied so a calculation can safely outlive an engine mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyMarketSeries {
    pub(super) view: SeriesView,
}

/// One retained provider-neutral aggressor trade exposed to native studies.
///
/// This is the same bounded live sample retained by `market_runtime` for the
/// canonical instrument state; the Study Runtime does not keep a second trade
/// buffer. Prices and quantities remain fixed-point and use the scales reported
/// by [`StudyTradeWindow`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyTradeSample {
    pub observed_unix_nanos: i64,
    pub price: i64,
    pub quantity: i64,
    pub aggressor: AggressorSide,
}

/// Borrowed bounded window over the runtime's retained recent trades.
#[derive(Clone, Copy)]
pub struct StudyTradeWindow<'a> {
    pub(super) trades: &'a VecDeque<RetainedMarketTrade>,
    pub(super) session_generation: u64,
    pub(super) source_watermark: u64,
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
}

impl<'a> StudyTradeWindow<'a> {
    pub(crate) const fn new(
        trades: &'a VecDeque<RetainedMarketTrade>,
        session_generation: u64,
        source_watermark: u64,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Self {
        Self {
            trades,
            session_generation,
            source_watermark,
            price_scale,
            quantity_scale,
        }
    }

    /// Returns the number of retained live trade samples.
    #[must_use]
    pub fn len(self) -> usize {
        self.trades.len()
    }

    /// Returns whether the runtime currently retains no live trade samples.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.trades.is_empty()
    }

    /// Returns the provider session generation owning the retained trade window.
    #[must_use]
    pub const fn session_generation(self) -> u64 {
        self.session_generation
    }

    /// Returns the latest accepted trade source sequence.
    #[must_use]
    pub const fn source_watermark(self) -> u64 {
        self.source_watermark
    }

    /// Returns the fixed-point decimal scale for trade prices.
    #[must_use]
    pub const fn price_scale(self) -> u8 {
        self.price_scale
    }

    /// Returns the fixed-point decimal scale for trade quantities.
    #[must_use]
    pub const fn quantity_scale(self) -> u8 {
        self.quantity_scale
    }

    /// Returns one retained trade sample without copying the retained window.
    #[must_use]
    pub fn get(self, index: usize) -> Option<StudyTradeSample> {
        self.trades.get(index).map(study_trade_sample)
    }

    /// Iterates retained trade samples from oldest to newest without allocating.
    pub fn iter(self) -> impl ExactSizeIterator<Item = StudyTradeSample> + 'a {
        self.trades.iter().map(study_trade_sample)
    }
}

fn study_trade_sample(trade: &RetainedMarketTrade) -> StudyTradeSample {
    StudyTradeSample {
        observed_unix_nanos: trade.observed_unix_nanos,
        price: trade.trade.price,
        quantity: trade.trade.quantity,
        aggressor: trade.trade.aggressor,
    }
}

/// Borrowed current top-of-book quote for one canonical instrument.
#[derive(Clone, Copy)]
pub struct StudyQuoteView<'a> {
    pub(super) quote: &'a TopOfBookQuote,
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
}

impl<'a> StudyQuoteView<'a> {
    pub(crate) const fn new(
        quote: &'a TopOfBookQuote,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Self {
        Self {
            quote,
            price_scale,
            quantity_scale,
        }
    }

    /// Returns the current bid, if the provider has not explicitly cleared it.
    #[must_use]
    pub const fn bid(self) -> Option<QuoteLevel> {
        self.quote.bid
    }

    /// Returns the current ask, if the provider has not explicitly cleared it.
    #[must_use]
    pub const fn ask(self) -> Option<QuoteLevel> {
        self.quote.ask
    }

    /// Returns the latest accepted quote source sequence.
    #[must_use]
    pub const fn source_sequence(self) -> u64 {
        self.quote.metadata.source_sequence
    }

    /// Returns the provider session generation owning this quote.
    #[must_use]
    pub const fn session_generation(self) -> u64 {
        self.quote.metadata.session_generation
    }

    /// Returns the best available event-time timestamp for this quote.
    #[must_use]
    pub fn observed_unix_nanos(self) -> i64 {
        self.quote
            .metadata
            .timestamps
            .exchange_unix_nanos
            .or(self.quote.metadata.timestamps.provider_unix_nanos)
            .unwrap_or(self.quote.metadata.timestamps.received_unix_nanos)
    }

    /// Returns the fixed-point decimal scale for quote prices.
    #[must_use]
    pub const fn price_scale(self) -> u8 {
        self.price_scale
    }

    /// Returns the fixed-point decimal scale for quote quantities.
    #[must_use]
    pub const fn quantity_scale(self) -> u8 {
        self.quantity_scale
    }
}

/// Borrowed canonical depth image for one instrument.
///
/// Level iteration reads an immutable book image. Worker execution owns a
/// bounded snapshot so calculations never borrow mutable canonical state.
#[derive(Clone, Copy)]
pub struct StudyDepthView<'a> {
    pub(super) book: &'a OrderBook,
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
}

impl<'a> StudyDepthView<'a> {
    pub(crate) const fn new(book: &'a OrderBook, price_scale: u8, quantity_scale: u8) -> Self {
        Self {
            book,
            price_scale,
            quantity_scale,
        }
    }

    /// Returns current canonical book readiness/recovery state.
    #[must_use]
    pub const fn state(self) -> OrderBookState {
        self.book.state()
    }

    /// Returns the current canonical book revision.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.book.revision()
    }

    /// Returns the latest accepted depth source sequence.
    #[must_use]
    pub const fn source_watermark(self) -> u64 {
        self.book.source_watermark()
    }

    /// Returns the provider session generation owning this depth image.
    #[must_use]
    pub fn session_generation(self) -> Option<u64> {
        self.book.session_generation()
    }

    /// Returns canonical bid levels from best to worst without allocating.
    pub fn bids(self) -> impl ExactSizeIterator<Item = DepthLevel> + 'a {
        self.book.bid_levels()
    }

    /// Returns canonical ask levels from best to worst without allocating.
    pub fn asks(self) -> impl ExactSizeIterator<Item = DepthLevel> + 'a {
        self.book.ask_levels()
    }

    /// Returns the fixed-point decimal scale for depth prices.
    #[must_use]
    pub const fn price_scale(self) -> u8 {
        self.price_scale
    }

    /// Returns the fixed-point decimal scale for depth quantities.
    #[must_use]
    pub const fn quantity_scale(self) -> u8 {
        self.quantity_scale
    }
}

#[derive(Clone, Copy)]
pub(crate) struct StudyNonBarChange<'a> {
    pub provider_id: &'a str,
    pub instrument_id: &'a str,
    pub entitlement_id: &'a str,
    pub stream: MarketStream,
    pub observed_unix_nanos: i64,
}

/// Borrowed non-bar market state aligned with one declared market dependency.
#[derive(Clone, Copy, Default)]
pub struct StudyLiveMarketData<'a> {
    pub(super) quote: Option<StudyQuoteView<'a>>,
    pub(super) trades: Option<StudyTradeWindow<'a>>,
    pub(super) depth: Option<StudyDepthView<'a>>,
}

impl<'a> StudyLiveMarketData<'a> {
    pub(crate) const fn new(
        quote: Option<StudyQuoteView<'a>>,
        trades: Option<StudyTradeWindow<'a>>,
        depth: Option<StudyDepthView<'a>>,
    ) -> Self {
        Self {
            quote,
            trades,
            depth,
        }
    }

    /// Returns the current canonical top-of-book quote, when requested and available.
    #[must_use]
    pub const fn quote(self) -> Option<StudyQuoteView<'a>> {
        self.quote
    }

    /// Returns the runtime's bounded recent-trade window, when requested.
    #[must_use]
    pub const fn trades(self) -> Option<StudyTradeWindow<'a>> {
        self.trades
    }

    /// Returns the current canonical depth image, when requested.
    #[must_use]
    pub const fn depth(self) -> Option<StudyDepthView<'a>> {
        self.depth
    }
}

impl StudyMarketSeries {
    pub(super) fn new(view: SeriesView) -> Self {
        Self { view }
    }

    /// Returns the canonical series identity.
    #[must_use]
    pub fn series(&self) -> &BarSeriesKey {
        self.view.series()
    }

    /// Returns the immutable canonical bars without copying them.
    #[must_use]
    pub fn bars(&self) -> &SeriesView {
        &self.view
    }

    /// Returns whether the last canonical bar is still forming.
    #[must_use]
    pub fn forming(&self) -> bool {
        self.view.forming
    }

    /// Returns the canonical publication generation observed by this view.
    #[must_use]
    pub fn publication_generation(&self) -> u64 {
        self.view.publication_generation
    }

    /// Returns one fixed-point OHLCV field view over the canonical bars.
    #[must_use]
    pub fn field(&self, field: StudyBarField) -> StudyBarFieldSeries<'_> {
        let scale = match field {
            StudyBarField::Open
            | StudyBarField::High
            | StudyBarField::Low
            | StudyBarField::Close => self.view.price_scale(),
            StudyBarField::Volume => self.view.quantity_scale(),
        };
        StudyBarFieldSeries {
            bars: &self.view,
            field,
            scale,
        }
    }
}

/// Non-owning fixed-point field projection over canonical bars.
#[derive(Clone, Copy, Debug)]
pub struct StudyBarFieldSeries<'a> {
    bars: &'a SeriesView,
    field: StudyBarField,
    scale: u8,
}

impl StudyBarFieldSeries<'_> {
    /// Returns the number of canonical rows.
    #[must_use]
    pub fn len(self) -> usize {
        self.bars.len()
    }

    /// Returns whether no canonical rows are available.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.bars.is_empty()
    }

    /// Returns the decimal scale for this fixed-point field.
    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }

    /// Returns one exact fixed-point value without allocating or converting.
    #[must_use]
    pub fn value(self, index: usize) -> Option<i64> {
        self.bars.get(index).map(|bar| match self.field {
            StudyBarField::Open => bar.open,
            StudyBarField::High => bar.high,
            StudyBarField::Low => bar.low,
            StudyBarField::Close => bar.close,
            StudyBarField::Volume => bar.volume,
        })
    }

    /// Returns one exact exchange timestamp for row alignment across inputs.
    #[must_use]
    pub fn exchange_timestamp_unix_nanos(self, index: usize) -> Option<i64> {
        self.bars
            .get(index)
            .map(|bar| bar.exchange_timestamp_unix_nanos)
    }
}

/// Hard retention bound for one pure non-bar study event timeline.
pub const MAXIMUM_STUDY_EVENT_TIMELINE_POINTS: usize = 8_192;

/// Non-bar microstructure event stream kind carried by a [`StudyEventPoint`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StudyEventKind {
    Trade,
    Quote,
    Depth,
}

/// Explicit coalescing policy for a pure non-bar [`StudyEventTimeline`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StudyEventCoalescePolicy {
    /// Retain every monotonically ordered `(timestamp_unix_nanos, source_sequence)` event.
    ExactSequence,
    /// Coalesce events sharing the same `timestamp_unix_nanos` into the newest event.
    LatestPerTimestamp,
    /// Bucket events into fixed nanosecond windows and retain the latest event per bucket.
    WindowBucketNanos(std::num::NonZeroI64),
}

/// One bounded non-bar event sample emitted onto a [`StudyEventTimeline`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StudyEventPoint {
    pub timestamp_unix_nanos: i64,
    pub source_sequence: u64,
    pub kind: StudyEventKind,
    pub value: f64,
}

/// Bounded, ordered pure non-bar event timeline for trade/quote/depth studies.
///
/// Provides explicit retention, coalescing, monotonic ordering, and deterministic
/// whole-second chart projection without requiring a bar primary dependency.
#[derive(Clone, Debug, PartialEq)]
pub struct StudyEventTimeline {
    capacity: usize,
    coalesce: StudyEventCoalescePolicy,
    points: VecDeque<StudyEventPoint>,
    evicted: u64,
}

impl StudyEventTimeline {
    /// Creates a bounded event timeline clamped to `1..=MAXIMUM_STUDY_EVENT_TIMELINE_POINTS`.
    #[must_use]
    pub fn new(capacity: usize, coalesce: StudyEventCoalescePolicy) -> Self {
        let capacity = capacity.clamp(1, MAXIMUM_STUDY_EVENT_TIMELINE_POINTS);
        Self {
            capacity,
            coalesce,
            points: VecDeque::new(),
            evicted: 0,
        }
    }

    /// Returns the number of retained event points.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Returns whether no event points are currently retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Returns the total number of oldest points evicted due to retention bounds.
    #[must_use]
    pub const fn evicted_count(&self) -> u64 {
        self.evicted
    }

    /// Iterates retained event points from oldest to newest.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &StudyEventPoint> {
        self.points.iter()
    }

    /// Clears all retained points without resetting capacity or coalescing policy.
    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// Appends or coalesces one event point under the configured ordering and retention policy.
    ///
    /// Out-of-order events older than the newest retained `(timestamp_unix_nanos, source_sequence)`
    /// are ignored (`Ok(false)`).
    ///
    /// # Errors
    /// Returns an error if `point.value` is non-finite.
    pub fn push(&mut self, mut point: StudyEventPoint) -> Result<bool, String> {
        if !point.value.is_finite() {
            return Err("study event timeline value must be finite".to_string());
        }
        if let StudyEventCoalescePolicy::WindowBucketNanos(bucket) = self.coalesce {
            let step = bucket.get();
            point.timestamp_unix_nanos = point.timestamp_unix_nanos.div_euclid(step) * step;
        }
        if let Some(last) = self.points.back_mut() {
            if point.timestamp_unix_nanos < last.timestamp_unix_nanos
                || (point.timestamp_unix_nanos == last.timestamp_unix_nanos
                    && point.source_sequence < last.source_sequence)
            {
                return Ok(false);
            }
            let same_bucket = match self.coalesce {
                StudyEventCoalescePolicy::ExactSequence => {
                    point.timestamp_unix_nanos == last.timestamp_unix_nanos
                        && point.source_sequence == last.source_sequence
                }
                StudyEventCoalescePolicy::LatestPerTimestamp
                | StudyEventCoalescePolicy::WindowBucketNanos(_) => {
                    point.timestamp_unix_nanos == last.timestamp_unix_nanos
                }
            };
            if same_bucket {
                *last = point;
                return Ok(true);
            }
        }
        if self.points.len() >= self.capacity {
            let _ = self.points.pop_front();
            self.evicted = self.evicted.saturating_add(1);
        }
        self.points.push_back(point);
        Ok(true)
    }

    /// Projects retained sub-second events deterministically onto strictly increasing whole-second
    /// chart timestamps (in Unix nanoseconds), keeping the latest value within each second.
    #[must_use]
    pub fn project_to_chart_seconds(&self) -> (Vec<i64>, Vec<Option<f64>>) {
        const NANOS_PER_SECOND: i64 = 1_000_000_000;
        let mut timestamps: Vec<i64> = Vec::new();
        let mut values: Vec<Option<f64>> = Vec::new();
        for point in &self.points {
            let sec_ns = point
                .timestamp_unix_nanos
                .div_euclid(NANOS_PER_SECOND)
                .saturating_mul(NANOS_PER_SECOND);
            if timestamps.last().copied() == Some(sec_ns) {
                if let Some(last) = values.last_mut() {
                    *last = Some(point.value);
                }
            } else {
                timestamps.push(sec_ns);
                values.push(Some(point.value));
            }
        }
        (timestamps, values)
    }
}
