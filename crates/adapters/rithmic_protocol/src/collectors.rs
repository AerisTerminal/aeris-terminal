use crate::{
    DecodedCatalogMessage, DecodedHistoryMessage, DecodedTickBar, DecodedTimeBar,
    DecodedTimeBarType, HistorySource, ProductCodeResult, ReplayKind, SymbolSearchResult,
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

/// Widely traded futures products, most active first, keyed by product code and the exchange
/// Rithmic lists them on. The plant returns hundreds of codes in no useful order (many of them
/// numeric spread or option codes), so a bounded listing keeps these ahead of the rest.
const FEATURED_PRODUCTS: &[(&str, &str)] = &[
    ("ES", "CME"),
    ("NQ", "CME"),
    ("MES", "CME"),
    ("MNQ", "CME"),
    ("YM", "CBOT"),
    ("MYM", "CBOT"),
    ("RTY", "CME"),
    ("M2K", "CME"),
    ("CL", "NYMEX"),
    ("MCL", "NYMEX"),
    ("GC", "COMEX"),
    ("MGC", "COMEX"),
    ("SI", "COMEX"),
    ("SIL", "COMEX"),
    ("HG", "COMEX"),
    ("NG", "NYMEX"),
    ("RB", "NYMEX"),
    ("HO", "NYMEX"),
    ("PL", "NYMEX"),
    ("ZN", "CBOT"),
    ("ZB", "CBOT"),
    ("ZF", "CBOT"),
    ("ZT", "CBOT"),
    ("UB", "CBOT"),
    ("SR3", "CME"),
    ("6E", "CME"),
    ("6J", "CME"),
    ("6B", "CME"),
    ("6A", "CME"),
    ("6C", "CME"),
    ("6S", "CME"),
    ("M6E", "CME"),
    ("ZC", "CBOT"),
    ("ZS", "CBOT"),
    ("ZW", "CBOT"),
    ("ZL", "CBOT"),
    ("ZM", "CBOT"),
    ("LE", "CME"),
    ("HE", "CME"),
    ("BTC", "CME"),
    ("MBT", "CME"),
    ("ETH", "CME"),
    ("MET", "CME"),
];

/// Listing order of one product: featured products by activity, then lettered codes, then
/// codes without letters.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ListingRank {
    Featured(usize),
    Lettered,
    Unlettered,
}

impl ListingRank {
    fn of(product_code: &str, exchange: &str) -> Self {
        if let Some(position) = FEATURED_PRODUCTS
            .iter()
            .position(|&(code, venue)| code == product_code && venue == exchange)
        {
            Self::Featured(position)
        } else if product_code.bytes().any(|byte| byte.is_ascii_alphabetic()) {
            Self::Lettered
        } else {
            Self::Unlettered
        }
    }
}

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
///
/// An instrument-type filter is applied by the provider only: responses spell
/// the type differently from the request enumeration (`Future`, not `FUTURE`),
/// so it is not echo-checked here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolSearchCollectionRequest {
    pub exchange: Option<String>,
    pub product_code: Option<String>,
    pub maximum_results: NonZeroUsize,
}

impl SymbolSearchCollectionRequest {
    fn validate(&self) -> Result<(), CollectorError> {
        if self.maximum_results.get() > MAX_SEARCH_RESULTS
            || [&self.exchange, &self.product_code]
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
    /// A broad query can match more instruments than one search may return,
    /// so only the lowest `maximum_results` exchange/symbol keys are kept,
    /// independent of arrival order. A repeated key keeps its first result.
    ///
    /// # Errors
    ///
    /// Returns an error for rejection, mismatched filters, or any frame after
    /// a terminal outcome.
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
                if self.results.contains_key(&key) {
                    self.duplicate_count = self.duplicate_count.saturating_add(1);
                    return Ok(CollectionProgress::Pending);
                }
                self.results.insert(key, result);
                if self.results.len() > self.request.maximum_results.get() {
                    self.results.pop_last();
                }
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
            DecodedCatalogMessage::ProductCode(_)
            | DecodedCatalogMessage::ProductCodesComplete { .. } => {
                Err(CollectorError::UnexpectedMessage)
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
        ] {
            if expected.is_some() && expected != actual {
                return Err(CollectorError::IdentityMismatch);
            }
        }
        Ok(())
    }
}

/// Completed product listing ordered by [`ListingRank`], then product code and exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectedProducts {
    pub products: Vec<ProductCodeResult>,
    pub duplicate_count: usize,
}

/// Bounded, deduplicating product-code response collector.
pub struct ProductCodeCollector {
    maximum_results: NonZeroUsize,
    products: BTreeMap<(ListingRank, String, String), ProductCodeResult>,
    duplicate_count: usize,
    state: CollectorState,
}

impl ProductCodeCollector {
    /// Creates a collector that keeps at most `maximum_results` products.
    ///
    /// # Errors
    ///
    /// Returns an error for a result bound above 4,096.
    pub fn try_new(maximum_results: NonZeroUsize) -> Result<Self, CollectorError> {
        if maximum_results.get() > MAX_SEARCH_RESULTS {
            return Err(CollectorError::InvalidLimit);
        }
        Ok(Self {
            maximum_results,
            products: BTreeMap::new(),
            duplicate_count: 0,
            state: CollectorState::Collecting,
        })
    }

    /// Accepts one product or terminal frame.
    ///
    /// Only the first `maximum_results` products in listing order are kept,
    /// independent of arrival order. A repeated key keeps its first product.
    ///
    /// # Errors
    ///
    /// Returns an error for rejection, a symbol-search frame, or any frame
    /// after a terminal outcome.
    pub fn accept(
        &mut self,
        message: DecodedCatalogMessage,
    ) -> Result<CollectionProgress<CollectedProducts, DecodedCatalogMessage>, CollectorError> {
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
    ) -> Result<CollectionProgress<CollectedProducts, DecodedCatalogMessage>, CollectorError> {
        match message {
            DecodedCatalogMessage::ProductCode(product) => {
                let key = (
                    ListingRank::of(&product.product_code, &product.exchange),
                    product.product_code.clone(),
                    product.exchange.clone(),
                );
                if self.products.contains_key(&key) {
                    self.duplicate_count = self.duplicate_count.saturating_add(1);
                    return Ok(CollectionProgress::Pending);
                }
                self.products.insert(key, product);
                if self.products.len() > self.maximum_results.get() {
                    self.products.pop_last();
                }
                Ok(CollectionProgress::Pending)
            }
            DecodedCatalogMessage::ProductCodesComplete { accepted: false } => {
                Err(CollectorError::Rejected)
            }
            DecodedCatalogMessage::ProductCodesComplete { accepted: true } => {
                self.state = CollectorState::Complete;
                Ok(CollectionProgress::Complete(CollectedProducts {
                    products: self.products.values().cloned().collect(),
                    duplicate_count: self.duplicate_count,
                }))
            }
            DecodedCatalogMessage::SearchResult(_)
            | DecodedCatalogMessage::SearchComplete { .. } => {
                Err(CollectorError::UnexpectedMessage)
            }
            message @ DecodedCatalogMessage::InstrumentReference(_) => {
                Ok(CollectionProgress::Unhandled(message))
            }
        }
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
    /// Replay start index: Unix seconds, or the YYYYMMDD trading date for daily and weekly bars.
    pub start_seconds: i32,
    /// Inclusive replay finish index, in the same units as `start_seconds`.
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
            // Rithmic ignores the period of a daily or weekly replay and
            // always answers with single-day or single-week bars.
            HistorySeries::Time {
                bar_type: DecodedTimeBarType::Daily | DecodedTimeBarType::Weekly,
                period,
            } => period != 1,
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

/// The period text Rithmic puts on a replayed time bar: seconds for second and
/// minute bars, and the unit name for day and week bars (`daily` as observed
/// on Rithmic Test).
pub(crate) fn rithmic_wire_period(bar_type: DecodedTimeBarType, period: i32) -> Option<String> {
    match bar_type {
        DecodedTimeBarType::Second => Some(period.to_string()),
        DecodedTimeBarType::Minute => period.checked_mul(60).map(|seconds| seconds.to_string()),
        DecodedTimeBarType::Daily => Some("daily".to_string()),
        DecodedTimeBarType::Weekly => Some("weekly".to_string()),
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
        if bar.bar_type != bar_type
            || rithmic_wire_period(bar_type, period).as_deref() != Some(bar.period.as_str())
        {
            return Err(CollectorError::SeriesMismatch);
        }
        // Rithmic stamps a second or minute bar with the time its period
        // closes, and a minute replay returns every bar whose period overlaps
        // the requested seconds, so the last one closes up to one period
        // after the finish. Day and week markers are YYYYMMDD trading dates,
        // inside the YYYYMMDD request range.
        let latest_marker = match bar_type {
            DecodedTimeBarType::Second => self.request.finish_seconds.saturating_add(period),
            DecodedTimeBarType::Minute => self
                .request
                .finish_seconds
                .saturating_add(period.saturating_mul(60)),
            DecodedTimeBarType::Daily | DecodedTimeBarType::Weekly => self.request.finish_seconds,
        };
        if !(self.request.start_seconds..=latest_marker).contains(&bar.marker_seconds) {
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
        // Trade-count bars are indivisible and may straddle either requested
        // time boundary. Retain a bar only when its key envelope overlaps the
        // requested envelope; completely outside bars still fail closed.
        if first.0 > self.request.finish_seconds || last.0 < self.request.start_seconds {
            return Err(CollectorError::OutsideRequestedRange);
        }
        // A tick bar carries the keys of its first and last trades, the same
        // key twice for a one-trade bar. Their sequence text is opaque, so only
        // their times must be in order.
        if !keys
            .windows(2)
            .all(|pair| (pair[0].0, pair[0].1) <= (pair[1].0, pair[1].1))
        {
            return Err(CollectorError::NonMonotonic);
        }
        keys.dedup();
        if keys.iter().any(|key| self.tick_keys.contains(key)) {
            return Err(CollectorError::TickKeyOverlap);
        }
        self.ensure_capacity(self.tick_bars.len())?;
        if self.tick_keys.len().saturating_add(keys.len()) > MAX_REPLAY_TICK_KEYS {
            return Err(CollectorError::TickKeyLimitExceeded {
                maximum: MAX_REPLAY_TICK_KEYS,
            });
        }
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
    fn symbol_search_keeps_the_lowest_bounded_keys_and_first_duplicate() {
        let mut collector =
            SymbolSearchCollector::try_new(search_request(2)).expect("valid search");
        let first = search_result("NQM7");
        let mut conflicting = first.clone();
        conflicting.name = Some("renamed".to_string());
        for result in [
            first,
            conflicting,
            search_result("YMM7"),
            search_result("ESM7"),
            search_result("CLM7"),
        ] {
            assert_eq!(
                collector
                    .accept(DecodedCatalogMessage::SearchResult(result))
                    .expect("search result stays within the session"),
                CollectionProgress::Pending
            );
        }
        let complete = collector
            .accept(DecodedCatalogMessage::SearchComplete { accepted: true })
            .expect("excess results still complete");
        assert!(matches!(
            complete,
            CollectionProgress::Complete(CollectedSymbols { results, duplicate_count: 1 })
                if results.iter().map(|result| result.symbol.as_str()).collect::<Vec<_>>() == ["CLM7", "ESM7"]
        ));
    }

    #[test]
    fn symbol_search_exchange_mismatch_latches_failure() {
        let mut collector =
            SymbolSearchCollector::try_new(search_request(2)).expect("valid search");
        let mut other_exchange = search_result("ESM7");
        other_exchange.exchange = "NYMEX".to_string();
        assert_eq!(
            collector.accept(DecodedCatalogMessage::SearchResult(other_exchange)),
            Err(CollectorError::IdentityMismatch)
        );
        assert_eq!(
            collector.accept(DecodedCatalogMessage::SearchComplete { accepted: true }),
            Err(CollectorError::AlreadyTerminal)
        );
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
    fn product_listing_dedupes_ranks_featured_first_and_bounds() {
        let mut collector = ProductCodeCollector::try_new(NonZeroUsize::new(5).expect("bound"))
            .expect("valid listing");
        for (code, exchange, name) in [
            ("0X", "CME", None),
            ("1012", "CBOT", None),
            ("AB", "CME", None),
            ("NQ", "CME", Some("E-Mini NASDAQ 100")),
            ("ZN", "CBOT", None),
            ("ES", "CME", Some("E-Mini S&P 500")),
            ("NQ", "CME", Some("duplicate keeps the first")),
            ("ES", "CBOT", None),
            ("999", "CME", None),
        ] {
            assert_eq!(
                collector
                    .accept(DecodedCatalogMessage::ProductCode(product(
                        code, exchange, name
                    )))
                    .expect("product stays within the listing"),
                CollectionProgress::Pending
            );
        }
        let progress = collector
            .accept(DecodedCatalogMessage::ProductCodesComplete { accepted: true })
            .expect("listing completes");
        let CollectionProgress::Complete(CollectedProducts {
            products,
            duplicate_count,
        }) = progress
        else {
            panic!("listing completes");
        };
        assert_eq!(duplicate_count, 1);
        assert_eq!(
            products,
            vec![
                product("ES", "CME", Some("E-Mini S&P 500")),
                product("NQ", "CME", Some("E-Mini NASDAQ 100")),
                product("ZN", "CBOT", None),
                product("0X", "CME", None),
                product("AB", "CME", None),
            ]
        );
        assert_eq!(
            collector.accept(DecodedCatalogMessage::ProductCodesComplete { accepted: true }),
            Err(CollectorError::AlreadyTerminal)
        );
    }

    #[test]
    fn product_listing_and_symbol_search_reject_each_others_frames() {
        let mut listing = ProductCodeCollector::try_new(NonZeroUsize::MIN).expect("valid listing");
        assert_eq!(
            listing.accept(DecodedCatalogMessage::SearchComplete { accepted: true }),
            Err(CollectorError::UnexpectedMessage)
        );
        let mut rejected = ProductCodeCollector::try_new(NonZeroUsize::MIN).expect("valid listing");
        assert_eq!(
            rejected.accept(DecodedCatalogMessage::ProductCodesComplete { accepted: false }),
            Err(CollectorError::Rejected)
        );
        assert_eq!(
            ProductCodeCollector::try_new(
                NonZeroUsize::new(MAX_SEARCH_RESULTS + 1).expect("bound")
            )
            .err(),
            Some(CollectorError::InvalidLimit)
        );
        let mut search = SymbolSearchCollector::try_new(search_request(2)).expect("valid search");
        assert_eq!(
            search.accept(DecodedCatalogMessage::ProductCodesComplete { accepted: true }),
            Err(CollectorError::UnexpectedMessage)
        );
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
    fn minute_replay_accepts_the_bar_closing_one_period_after_the_finish() {
        let mut collector = HistoryCollector::try_new(time_request(1)).expect("valid collector");
        collector
            .accept(replay_time(time_bar(360)))
            .expect("the bar open at the finish closes one period later");
        let mut later = HistoryCollector::try_new(time_request(1)).expect("valid collector");
        assert!(matches!(
            later.accept(replay_time(time_bar(361))),
            Err(CollectorError::OutsideRequestedRange)
        ));
    }

    #[test]
    fn daily_replay_is_one_day_bars_indexed_by_trading_date() {
        let request = |period| HistoryCollectionRequest {
            series: HistorySeries::Time {
                bar_type: DecodedTimeBarType::Daily,
                period,
            },
            start_seconds: 20_261_015,
            finish_seconds: 20_261_021,
            ..time_request(4)
        };
        assert_eq!(
            HistoryCollector::try_new(request(3)).err(),
            Some(CollectorError::InvalidLimit)
        );
        let daily = |marker_seconds, period: &str| DecodedTimeBar {
            bar_type: DecodedTimeBarType::Daily,
            period: period.to_string(),
            ..time_bar(marker_seconds)
        };
        let mut collector = HistoryCollector::try_new(request(1)).expect("valid daily collector");
        collector
            .accept(replay_time(daily(20_261_020, "daily")))
            .expect("a closed trading date inside the range");
        assert!(matches!(
            collector.accept(replay_time(daily(20_261_022, "daily"))),
            Err(CollectorError::OutsideRequestedRange)
        ));
        let mut numbered = HistoryCollector::try_new(request(1)).expect("valid daily collector");
        assert!(matches!(
            numbered.accept(replay_time(daily(20_261_020, "1"))),
            Err(CollectorError::SeriesMismatch)
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
    fn tick_history_orders_trades_by_time_not_by_opaque_sequence_text() {
        let mut collector = HistoryCollector::try_new(tick_request(3)).expect("valid collector");
        collector
            .accept(replay_tick(tick_bar(&[(120, 5, "14"), (120, 5, "14")])))
            .expect("a one-trade bar repeats its only key");
        collector
            .accept(replay_tick(tick_bar(&[(121, 1, "14"), (121, 2, "13")])))
            .expect("sequence text carries no order");
        assert!(matches!(
            collector.accept(replay_tick(tick_bar(&[(123, 1, "20"), (122, 1, "21")]))),
            Err(CollectorError::NonMonotonic)
        ));
    }

    #[test]
    fn tick_history_accepts_indivisible_bars_crossing_time_boundaries() {
        let mut collector = HistoryCollector::try_new(tick_request(3)).expect("valid collector");
        collector
            .accept(replay_tick(tick_bar(&[(99, 900_000, "1"), (100, 1, "2")])))
            .expect("provider-completed leading bar is retained");
        collector
            .accept(replay_tick(tick_bar(&[(101, 1, "3"), (101, 2, "4")])))
            .expect("in-range bar is retained");
        collector
            .accept(replay_tick(tick_bar(&[(300, 1, "5"), (301, 2, "6")])))
            .expect("provider-completed trailing bar is retained");

        let mut outside = HistoryCollector::try_new(tick_request(2)).expect("valid collector");
        assert!(matches!(
            outside.accept(replay_tick(tick_bar(&[(98, 1, "1"), (99, 2, "2")]))),
            Err(CollectorError::OutsideRequestedRange)
        ));
    }

    #[test]
    fn tick_history_sorts_nonoverlapping_provider_frames_by_exact_key() {
        let mut collector = HistoryCollector::try_new(tick_request(2)).expect("valid collector");
        collector
            .accept(replay_tick(tick_bar(&[(121, 1, "3"), (121, 2, "4")])))
            .expect("later frame is retained");
        collector
            .accept(replay_tick(tick_bar(&[(120, 1, "1"), (120, 2, "2")])))
            .expect("earlier frame is retained");
        let complete = collector
            .accept(DecodedHistoryMessage::ReplayComplete {
                kind: ReplayKind::Tick,
                accepted: true,
            })
            .expect("replay completes");
        assert!(matches!(
            complete,
            CollectionProgress::Complete(CollectedHistory {
                bars: HistoryBars::Tick(bars),
                ..
            }) if bars[0].keys[0].seconds == 120 && bars[1].keys[0].seconds == 121
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

    fn product(code: &str, exchange: &str, name: Option<&str>) -> ProductCodeResult {
        ProductCodeResult {
            product_code: code.to_string(),
            exchange: exchange.to_string(),
            name: name.map(str::to_string),
        }
    }

    fn search_request(maximum: usize) -> SymbolSearchCollectionRequest {
        SymbolSearchCollectionRequest {
            exchange: Some("CME".to_string()),
            product_code: None,
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
            instrument_type: Some("Future".to_string()),
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
            period: "60".to_string(),
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
