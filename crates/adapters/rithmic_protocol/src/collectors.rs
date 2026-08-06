use crate::{
    DecodedCatalogMessage, DecodedHistoryMessage, DecodedTickBar, DecodedTimeBar,
    DecodedTimeBarType, HistorySource, ReplayKind, SymbolSearchResult,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    num::NonZeroUsize,
};

const MAX_SEARCH_RESULTS: usize = 4_096;
const MAX_REPLAY_BARS: usize = 10_000;
const MAX_REPLAY_TICK_KEYS: usize = 100_000;

/// Fail-closed validation errors for bounded multi-response collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectorError {
    InvalidLimit,
    ResultLimitExceeded { maximum: usize },
    TickKeyLimitExceeded { maximum: usize },
    UnexpectedMessage,
    Rejected,
    AlreadyTerminal,
    IdentityMismatch,
    SeriesMismatch,
    ReplayKindMismatch,
    OutsideRequestedRange,
    NonMonotonic,
    ConflictingDuplicate,
    TickKeyOverlap,
}

impl fmt::Display for CollectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic response collection failed: {self:?}")
    }
}

impl Error for CollectorError {}

/// Result of accepting one provider multi-response frame.
#[derive(Clone, Debug, PartialEq)]
pub enum CollectionProgress<T, U = ()> {
    Pending,
    Complete(T),
    Unhandled(U),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CollectorState {
    Collecting,
    Complete,
    Failed,
}

/// Exact optional filters and memory bound for one symbol search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolSearchCollectionRequest {
    pub exchange: Option<String>,
    pub product_code: Option<String>,
    pub instrument_type: Option<String>,
    pub maximum_results: NonZeroUsize,
}

impl SymbolSearchCollectionRequest {
    fn validate(&self) -> Result<(), CollectorError> {
        if self.maximum_results.get() > MAX_SEARCH_RESULTS
            || [&self.exchange, &self.product_code, &self.instrument_type]
                .into_iter()
                .flatten()
                .any(String::is_empty)
        {
            return Err(CollectorError::InvalidLimit);
        }
        Ok(())
    }
}

/// Completed, deterministically ordered symbol-search result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectedSymbols {
    pub results: Vec<SymbolSearchResult>,
    pub duplicate_count: usize,
}

/// Bounded, deduplicating symbol-search response collector.
pub struct SymbolSearchCollector {
    request: SymbolSearchCollectionRequest,
    results: BTreeMap<(String, String), SymbolSearchResult>,
    duplicate_count: usize,
    state: CollectorState,
}

impl SymbolSearchCollector {
    /// Creates a collector for one validated search request.
    ///
    /// # Errors
    ///
    /// Returns an error for empty filters or a result bound above 4,096.
    pub fn try_new(request: SymbolSearchCollectionRequest) -> Result<Self, CollectorError> {
        request.validate()?;
        Ok(Self {
            request,
            results: BTreeMap::new(),
            duplicate_count: 0,
            state: CollectorState::Collecting,
        })
    }

    /// Accepts one search result or terminal frame.
    ///
    /// # Errors
    ///
    /// Returns an error for rejection, mismatched filters, conflicting
    /// duplicates, excess results, or any frame after a terminal outcome.
    pub fn accept(
        &mut self,
        message: DecodedCatalogMessage,
    ) -> Result<CollectionProgress<CollectedSymbols, DecodedCatalogMessage>, CollectorError> {
        if self.state != CollectorState::Collecting {
            return Err(CollectorError::AlreadyTerminal);
        }
        let result = self.accept_inner(message);
        if result.is_err() {
            self.state = CollectorState::Failed;
        }
        result
    }

    fn accept_inner(
        &mut self,
        message: DecodedCatalogMessage,
    ) -> Result<CollectionProgress<CollectedSymbols, DecodedCatalogMessage>, CollectorError> {
        match message {
            DecodedCatalogMessage::SearchResult(result) => {
                self.validate_result(&result)?;
                let key = (result.exchange.clone(), result.symbol.clone());
                if let Some(existing) = self.results.get(&key) {
                    if existing != &result {
                        return Err(CollectorError::ConflictingDuplicate);
                    }
                    self.duplicate_count = self.duplicate_count.saturating_add(1);
                    return Ok(CollectionProgress::Pending);
                }
                if self.results.len() >= self.request.maximum_results.get() {
                    return Err(CollectorError::ResultLimitExceeded {
                        maximum: self.request.maximum_results.get(),
                    });
                }
                self.results.insert(key, result);
                Ok(CollectionProgress::Pending)
            }
            DecodedCatalogMessage::SearchComplete { accepted: false } => {
                Err(CollectorError::Rejected)
            }
            DecodedCatalogMessage::SearchComplete { accepted: true } => {
                self.state = CollectorState::Complete;
                Ok(CollectionProgress::Complete(CollectedSymbols {
                    results: self.results.values().cloned().collect(),
                    duplicate_count: self.duplicate_count,
                }))
            }
            message @ DecodedCatalogMessage::InstrumentReference(_) => {
                Ok(CollectionProgress::Unhandled(message))
            }
        }
    }

    fn validate_result(&self, result: &SymbolSearchResult) -> Result<(), CollectorError> {
        for (expected, actual) in [
            (self.request.exchange.as_ref(), Some(&result.exchange)),
            (
                self.request.product_code.as_ref(),
                result.product_code.as_ref(),
            ),
            (
                self.request.instrument_type.as_ref(),
                result.instrument_type.as_ref(),
            ),
        ] {
            if expected.is_some() && expected != actual {
                return Err(CollectorError::IdentityMismatch);
            }
        }
        Ok(())
    }
}

/// Exact bar series expected from one replay request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistorySeries {
    Time {
        bar_type: DecodedTimeBarType,
        period: i32,
    },
    Tick {
        trades_per_bar: u16,
    },
}

impl HistorySeries {
    const fn replay_kind(&self) -> ReplayKind {
        match self {
            Self::Time { .. } => ReplayKind::Time,
            Self::Tick { .. } => ReplayKind::Tick,
        }
    }
}

/// Identity, exact series, and inclusive range for one forward replay request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryCollectionRequest {
    pub symbol: String,
    pub exchange: String,
    pub series: HistorySeries,
    pub start_seconds: i32,
    pub finish_seconds: i32,
    pub maximum_bars: NonZeroUsize,
}

impl HistoryCollectionRequest {
    /// Validates an adapter-issued replay collection request.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty identity, invalid series/range, or a limit
    /// above the protocol request maximum.
    pub fn validate(&self) -> Result<(), CollectorError> {
        if self.symbol.is_empty() || self.exchange.is_empty() {
            return Err(CollectorError::IdentityMismatch);
        }
        let invalid_series = match self.series {
            HistorySeries::Time { period, .. } => period <= 0,
            HistorySeries::Tick { trades_per_bar } => trades_per_bar == 0,
        };
        if invalid_series || self.maximum_bars.get() > MAX_REPLAY_BARS {
            return Err(CollectorError::InvalidLimit);
        }
        if self.start_seconds < 0 || self.start_seconds > self.finish_seconds {
            return Err(CollectorError::OutsideRequestedRange);
        }
        Ok(())
    }
}

/// Completed replay bars.
#[derive(Clone, Debug, PartialEq)]
pub enum HistoryBars {
    Time(Vec<DecodedTimeBar>),
    Tick(Vec<DecodedTickBar>),
}

/// Inclusive provider-second envelope actually observed in a replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedHistoryRange {
    pub first_seconds: i32,
    pub last_seconds: i32,
}

/// Completed bounded replay result. This does not imply gap-free coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectedHistory {
    pub request: HistoryCollectionRequest,
    pub bars: HistoryBars,
    pub observed_range: Option<ObservedHistoryRange>,
    pub duplicate_count: usize,
}

type TickKey = (i32, i32, String);

/// Bounded, identity-fenced, monotonic replay collector.
pub struct HistoryCollector {
    request: HistoryCollectionRequest,
    time_bars: BTreeMap<i32, DecodedTimeBar>,
    tick_bars: BTreeMap<TickKey, DecodedTickBar>,
    tick_keys: BTreeSet<TickKey>,
    last_time_marker: Option<i32>,
    last_tick_key: Option<TickKey>,
    duplicate_count: usize,
    state: CollectorState,
}

impl HistoryCollector {
    /// Creates a collector for one validated replay request.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, series, range, or result bounds.
    pub fn try_new(request: HistoryCollectionRequest) -> Result<Self, CollectorError> {
        request.validate()?;
        Ok(Self {
            request,
            time_bars: BTreeMap::new(),
            tick_bars: BTreeMap::new(),
            tick_keys: BTreeSet::new(),
            last_time_marker: None,
            last_tick_key: None,
            duplicate_count: 0,
            state: CollectorState::Collecting,
        })
    }

    /// Accepts one replay data or terminal frame.
    ///
    /// Live subscription bars are returned untouched to the caller.
    ///
    /// # Errors
    ///
    /// Returns an error for rejection, identity/series/range mismatch,
    /// nonmonotonic data, key overlap, conflicts, excess results, or any frame
    /// after a terminal outcome.
    pub fn accept(
        &mut self,
        message: DecodedHistoryMessage,
    ) -> Result<CollectionProgress<CollectedHistory, DecodedHistoryMessage>, CollectorError> {
        if self.state != CollectorState::Collecting {
            return Err(CollectorError::AlreadyTerminal);
        }
        let result = self.accept_inner(message);
        if result.is_err() {
            self.state = CollectorState::Failed;
        }
        result
    }

    fn accept_inner(
        &mut self,
        message: DecodedHistoryMessage,
    ) -> Result<CollectionProgress<CollectedHistory, DecodedHistoryMessage>, CollectorError> {
        match message {
            DecodedHistoryMessage::TimeBar {
                source: HistorySource::Replay,
                bar,
            } => self.accept_time(bar),
            DecodedHistoryMessage::TickBar {
                source: HistorySource::Replay,
                bar,
            } => self.accept_tick(bar),
            message @ (DecodedHistoryMessage::TimeBar {
                source: HistorySource::Live,
                ..
            }
            | DecodedHistoryMessage::TickBar {
                source: HistorySource::Live,
                ..
            }) => Ok(CollectionProgress::Unhandled(message)),
            DecodedHistoryMessage::ReplayComplete {
                kind,
                accepted: false,
            } if kind == self.request.series.replay_kind() => Err(CollectorError::Rejected),
            DecodedHistoryMessage::ReplayComplete { kind, .. }
                if kind != self.request.series.replay_kind() =>
            {
                Err(CollectorError::ReplayKindMismatch)
            }
            DecodedHistoryMessage::ReplayComplete { .. } => Ok(self.finish()),
        }
    }

    fn accept_time(
        &mut self,
        bar: DecodedTimeBar,
    ) -> Result<CollectionProgress<CollectedHistory, DecodedHistoryMessage>, CollectorError> {
        let HistorySeries::Time { bar_type, period } = self.request.series else {
            return Err(CollectorError::ReplayKindMismatch);
        };
        self.validate_identity(&bar.identity.symbol, &bar.identity.exchange)?;
        if bar.bar_type != bar_type || bar.period != period.to_string() {
            return Err(CollectorError::SeriesMismatch);
        }
        if !(self.request.start_seconds..=self.request.finish_seconds).contains(&bar.marker_seconds)
        {
            return Err(CollectorError::OutsideRequestedRange);
        }
        if let Some(existing) = self.time_bars.get(&bar.marker_seconds) {
            if existing != &bar {
                return Err(CollectorError::ConflictingDuplicate);
            }
            self.duplicate_count = self.duplicate_count.saturating_add(1);
            return Ok(CollectionProgress::Pending);
        }
        if self
            .last_time_marker
            .is_some_and(|previous| bar.marker_seconds < previous)
        {
            return Err(CollectorError::NonMonotonic);
        }
        self.ensure_capacity(self.time_bars.len())?;
        self.last_time_marker = Some(bar.marker_seconds);
        self.time_bars.insert(bar.marker_seconds, bar);
        Ok(CollectionProgress::Pending)
    }

    fn accept_tick(
        &mut self,
        bar: DecodedTickBar,
    ) -> Result<CollectionProgress<CollectedHistory, DecodedHistoryMessage>, CollectorError> {
        let HistorySeries::Tick { trades_per_bar } = self.request.series else {
            return Err(CollectorError::ReplayKindMismatch);
        };
        self.validate_identity(&bar.identity.symbol, &bar.identity.exchange)?;
        if bar.trades_per_bar != trades_per_bar.to_string() {
            return Err(CollectorError::SeriesMismatch);
        }
        let keys = bar
            .keys
            .iter()
            .map(|key| (key.seconds, key.microseconds, key.sequence.clone()));
        let mut keys = keys.collect::<Vec<_>>();
        let first = keys
            .first()
            .cloned()
            .ok_or(CollectorError::UnexpectedMessage)?;
        let last = keys
            .last()
            .cloned()
            .ok_or(CollectorError::UnexpectedMessage)?;
        if let Some(existing) = self.tick_bars.get(&first) {
            if existing != &bar {
                return Err(CollectorError::ConflictingDuplicate);
            }
            self.duplicate_count = self.duplicate_count.saturating_add(1);
            return Ok(CollectionProgress::Pending);
        }
        if keys
            .iter()
            .any(|key| !(self.request.start_seconds..=self.request.finish_seconds).contains(&key.0))
        {
            return Err(CollectorError::OutsideRequestedRange);
        }
        if !keys.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(CollectorError::NonMonotonic);
        }
        if keys.iter().any(|key| self.tick_keys.contains(key)) {
            return Err(CollectorError::TickKeyOverlap);
        }
        if self
            .last_tick_key
            .as_ref()
            .is_some_and(|previous| first <= *previous)
        {
            return Err(CollectorError::NonMonotonic);
        }
        self.ensure_capacity(self.tick_bars.len())?;
        if self.tick_keys.len().saturating_add(keys.len()) > MAX_REPLAY_TICK_KEYS {
            return Err(CollectorError::TickKeyLimitExceeded {
                maximum: MAX_REPLAY_TICK_KEYS,
            });
        }
        self.last_tick_key = Some(last);
        self.tick_keys.extend(keys.drain(..));
        self.tick_bars.insert(first, bar);
        Ok(CollectionProgress::Pending)
    }

    fn finish(&mut self) -> CollectionProgress<CollectedHistory, DecodedHistoryMessage> {
        self.state = CollectorState::Complete;
        let (bars, observed_range) = match self.request.series {
            HistorySeries::Time { .. } => {
                let observed_range = self.time_bars.first_key_value().and_then(|(first, _)| {
                    self.time_bars
                        .last_key_value()
                        .map(|(last, _)| ObservedHistoryRange {
                            first_seconds: *first,
                            last_seconds: *last,
                        })
                });
                (
                    HistoryBars::Time(self.time_bars.values().cloned().collect()),
                    observed_range,
                )
            }
            HistorySeries::Tick { .. } => {
                let observed_range = self.tick_keys.first().and_then(|first| {
                    self.tick_keys.last().map(|last| ObservedHistoryRange {
                        first_seconds: first.0,
                        last_seconds: last.0,
                    })
                });
                (
                    HistoryBars::Tick(self.tick_bars.values().cloned().collect()),
                    observed_range,
                )
            }
        };
        CollectionProgress::Complete(CollectedHistory {
            request: self.request.clone(),
            bars,
            observed_range,
            duplicate_count: self.duplicate_count,
        })
    }

    fn validate_identity(&self, symbol: &str, exchange: &str) -> Result<(), CollectorError> {
        if symbol != self.request.symbol || exchange != self.request.exchange {
            return Err(CollectorError::IdentityMismatch);
        }
        Ok(())
    }

    fn ensure_capacity(&self, length: usize) -> Result<(), CollectorError> {
        if length >= self.request.maximum_bars.get() {
            return Err(CollectorError::ResultLimitExceeded {
                maximum: self.request.maximum_bars.get(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BarIdentity, Ohlc, TickBarKey};

    #[test]
    fn symbol_search_filters_deduplicates_bounds_and_latches_failures() {
        let mut collector =
            SymbolSearchCollector::try_new(search_request(2)).expect("valid search");
        let first = search_result("ESM7");
        collector
            .accept(DecodedCatalogMessage::SearchResult(first.clone()))
            .expect("accept result");
        collector
            .accept(DecodedCatalogMessage::SearchResult(first))
            .expect("deduplicate identical result");
        collector
            .accept(DecodedCatalogMessage::SearchResult(search_result("NQM7")))
            .expect("accept second result");
        assert!(matches!(
            collector.accept(DecodedCatalogMessage::SearchResult(search_result("YMM7"))),
            Err(CollectorError::ResultLimitExceeded { maximum: 2 })
        ));
        assert!(matches!(
            collector.accept(DecodedCatalogMessage::SearchComplete { accepted: true }),
            Err(CollectorError::AlreadyTerminal)
        ));
    }

    #[test]
    fn symbol_search_completes_sorted_and_preserves_unhandled_reference() {
        let mut collector =
            SymbolSearchCollector::try_new(search_request(2)).expect("valid search");
        let reference = DecodedCatalogMessage::InstrumentReference(None);
        assert_eq!(
            collector
                .accept(reference.clone())
                .expect("preserve reference"),
            CollectionProgress::Unhandled(reference)
        );
        collector
            .accept(DecodedCatalogMessage::SearchResult(search_result("NQM7")))
            .expect("accept second result");
        collector
            .accept(DecodedCatalogMessage::SearchResult(search_result("ESM7")))
            .expect("accept first result");
        let complete = collector
            .accept(DecodedCatalogMessage::SearchComplete { accepted: true })
            .expect("accept terminal");
        assert!(matches!(
            complete,
            CollectionProgress::Complete(CollectedSymbols { results, duplicate_count: 0 })
                if results.iter().map(|result| result.symbol.as_str()).collect::<Vec<_>>() == ["ESM7", "NQM7"]
        ));
    }

    #[test]
    fn history_validates_exact_time_series_and_reports_envelope() {
        let mut collector = HistoryCollector::try_new(time_request(2)).expect("valid collector");
        let first = time_bar(120);
        collector
            .accept(replay_time(first.clone()))
            .expect("accept first");
        collector
            .accept(replay_time(first))
            .expect("deduplicate first");
        collector
            .accept(replay_time(time_bar(180)))
            .expect("accept second");
        let complete = collector
            .accept(DecodedHistoryMessage::ReplayComplete {
                kind: ReplayKind::Time,
                accepted: true,
            })
            .expect("accept terminal");
        assert!(matches!(
            complete,
            CollectionProgress::Complete(CollectedHistory {
                bars: HistoryBars::Time(values),
                observed_range: Some(ObservedHistoryRange { first_seconds: 120, last_seconds: 180 }),
                duplicate_count: 1,
                ..
            }) if values.len() == 2
        ));
    }

    #[test]
    fn history_preserves_live_bars_and_latches_conflicting_replay() {
        let mut collector = HistoryCollector::try_new(time_request(1)).expect("valid collector");
        let live = DecodedHistoryMessage::TimeBar {
            source: HistorySource::Live,
            bar: time_bar(110),
        };
        assert_eq!(
            collector.accept(live.clone()).expect("preserve live bar"),
            CollectionProgress::Unhandled(live)
        );
        collector
            .accept(replay_time(time_bar(120)))
            .expect("accept replay");
        let mut conflict = time_bar(120);
        conflict.ohlc.close = 10.5;
        assert!(matches!(
            collector.accept(replay_time(conflict)),
            Err(CollectorError::ConflictingDuplicate)
        ));
        assert!(matches!(
            collector.accept(DecodedHistoryMessage::ReplayComplete {
                kind: ReplayKind::Time,
                accepted: true,
            }),
            Err(CollectorError::AlreadyTerminal)
        ));
    }

    #[test]
    fn tick_history_rejects_series_mismatch_and_cross_bar_overlap() {
        let mut collector = HistoryCollector::try_new(tick_request(2)).expect("valid collector");
        let first = tick_bar(&[(120, 1, "1"), (120, 2, "2")]);
        collector
            .accept(replay_tick(first))
            .expect("accept first tick bar");
        let overlap = tick_bar(&[(120, 2, "2"), (120, 3, "3")]);
        assert!(matches!(
            collector.accept(replay_tick(overlap)),
            Err(CollectorError::TickKeyOverlap)
        ));

        let mut mismatched = HistoryCollector::try_new(tick_request(1)).expect("valid collector");
        let mut bar = tick_bar(&[(120, 1, "1")]);
        bar.trades_per_bar = "8".to_string();
        assert!(matches!(
            mismatched.accept(replay_tick(bar)),
            Err(CollectorError::SeriesMismatch)
        ));
    }

    #[test]
    fn accepted_empty_history_is_valid_without_observed_coverage() {
        let mut collector = HistoryCollector::try_new(time_request(1)).expect("valid collector");
        let result = collector
            .accept(DecodedHistoryMessage::ReplayComplete {
                kind: ReplayKind::Time,
                accepted: true,
            })
            .expect("empty replay completes");
        assert!(matches!(
            result,
            CollectionProgress::Complete(CollectedHistory {
                bars: HistoryBars::Time(values),
                observed_range: None,
                ..
            }) if values.is_empty()
        ));
    }

    fn search_request(maximum: usize) -> SymbolSearchCollectionRequest {
        SymbolSearchCollectionRequest {
            exchange: Some("CME".to_string()),
            product_code: None,
            instrument_type: Some("FUTURE".to_string()),
            maximum_results: NonZeroUsize::new(maximum).expect("nonzero"),
        }
    }

    fn time_request(maximum: usize) -> HistoryCollectionRequest {
        HistoryCollectionRequest {
            symbol: "ESM7".to_string(),
            exchange: "CME".to_string(),
            series: HistorySeries::Time {
                bar_type: DecodedTimeBarType::Minute,
                period: 1,
            },
            start_seconds: 100,
            finish_seconds: 300,
            maximum_bars: NonZeroUsize::new(maximum).expect("nonzero"),
        }
    }

    fn tick_request(maximum: usize) -> HistoryCollectionRequest {
        HistoryCollectionRequest {
            symbol: "ESM7".to_string(),
            exchange: "CME".to_string(),
            series: HistorySeries::Tick { trades_per_bar: 10 },
            start_seconds: 100,
            finish_seconds: 300,
            maximum_bars: NonZeroUsize::new(maximum).expect("nonzero"),
        }
    }

    fn search_result(symbol: &str) -> SymbolSearchResult {
        SymbolSearchResult {
            symbol: symbol.to_string(),
            exchange: "CME".to_string(),
            name: None,
            product_code: None,
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: None,
        }
    }

    fn time_bar(marker_seconds: i32) -> DecodedTimeBar {
        DecodedTimeBar {
            identity: BarIdentity {
                symbol: "ESM7".to_string(),
                exchange: "CME".to_string(),
            },
            bar_type: DecodedTimeBarType::Minute,
            period: "1".to_string(),
            marker_seconds,
            ohlc: Ohlc {
                open: 10.0,
                high: 12.0,
                low: 9.0,
                close: 11.0,
            },
            trades: Some(1),
            volume: Some(1),
            bid_volume: None,
            ask_volume: None,
        }
    }

    fn tick_bar(keys: &[(i32, i32, &str)]) -> DecodedTickBar {
        DecodedTickBar {
            identity: BarIdentity {
                symbol: "ESM7".to_string(),
                exchange: "CME".to_string(),
            },
            trades_per_bar: "10".to_string(),
            keys: keys
                .iter()
                .map(|(seconds, microseconds, sequence)| TickBarKey {
                    sequence: (*sequence).to_string(),
                    seconds: *seconds,
                    microseconds: *microseconds,
                })
                .collect(),
            ohlc: Ohlc {
                open: 10.0,
                high: 12.0,
                low: 9.0,
                close: 11.0,
            },
            trades: Some(10),
            volume: Some(10),
            bid_volume: None,
            ask_volume: None,
        }
    }

    fn replay_time(bar: DecodedTimeBar) -> DecodedHistoryMessage {
        DecodedHistoryMessage::TimeBar {
            source: HistorySource::Replay,
            bar,
        }
    }

    fn replay_tick(bar: DecodedTickBar) -> DecodedHistoryMessage {
        DecodedHistoryMessage::TickBar {
            source: HistorySource::Replay,
            bar,
        }
    }
}
