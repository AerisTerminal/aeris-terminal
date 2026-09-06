use super::{
    ActiveViewport, ActiveWorkerGuard, Arc, AtomicBool, BTreeMap, BTreeSet, BarPeriod,
    BarSeriesKey, COINBASE_PROVIDER_GENERATION, COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseBarAggregator,
    CoinbaseBarAggregatorConfig, CoinbaseHistoryReadiness, CoinbaseInterval, Command, ConsumerId,
    Coordinator, CoverageSnapshot, DemandWaiter, EMPTY_REPAIR_RETRY_DELAY, FailureStage,
    FormingBar, GenerationId, HISTORY_BARS_PER_SERIES, HISTORY_CAPACITY_EXHAUSTED,
    HISTORY_RETRY_DELAY, HistoryRange, HistoryRequest, HistoryRequestKind, HistorySnapshot,
    HistorySource, HotSeries, HotSetManager, HotSetTier, InstallProviderInstrument, Instant,
    MAXIMUM_HISTORY_RETRIES, MAXIMUM_SERIES, MAXIMUM_VIEWPORT_HISTORY_RETRIES, MarketBar, Mutex,
    NonZeroU64, NonZeroUsize, Ordering, PendingViewportHistoryRetry, PersistenceState,
    ProviderCatalogCommand, ProviderConnectionState, ProviderGeneration, ProviderRequest,
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, Receiver, ResourceMode, RithmicHandoffSeed,
    SearchProviderInstruments, SeriesCadence, SeriesKey, SeriesLoadState, SeriesSnapshot,
    StorageRequest, StoredHistory, SyncSender, SystemTime, TrySendError, UNIX_EPOCH,
    VIEWPORT_BACKFILL_BARS, VIEWPORT_HISTORY_RETRY_DELAY, VIEWPORT_LIVE_TAIL_RESERVE, Viewport,
    WarmSeries, WorkspaceId, WorkspaceState, engine_install_failure_stage, fail_waiters,
    ipc_series, publish_state, series_state, thread, try_enqueue_history,
};

/// Merges a Coinbase repair page into the canonical series.
///
/// Every bar's `source_sequence` is derived from its bucket, so merging is a
/// union keyed on timestamp and nothing is ever renumbered: a bar the consumer
/// already holds keeps the identity it was published with. The result is the
/// contiguous run ending at the newest bar, because a series with a hole in it
/// fails the contiguity checks in `SeriesStore` and in the desktop's replay
/// model. Anything older than a hole is dropped rather than spliced, and the
/// coverage ledger refetches it.
/// Which side of a merge owns a bucket both sides carry.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum HistoryPrecedence {
    /// The incoming page came from the provider and is authoritative.
    Repair,
    /// The incoming page came from the local cache and may only fill gaps.
    Current,
}

pub(super) const COINBASE_HISTORY_PAGE_SOURCE_BARS: usize = 350;

const fn coinbase_source_bars_per_bucket(interval: CoinbaseInterval) -> usize {
    match interval {
        CoinbaseInterval::Minute1
        | CoinbaseInterval::Minute5
        | CoinbaseInterval::Minute15
        | CoinbaseInterval::Minute30
        | CoinbaseInterval::Hour1
        | CoinbaseInterval::Hour2
        | CoinbaseInterval::Day1 => 1,
        CoinbaseInterval::Hour4 | CoinbaseInterval::Hour12 => 2,
        CoinbaseInterval::Minute3 | CoinbaseInterval::Day3 => 3,
        CoinbaseInterval::Hour8 => 4,
        CoinbaseInterval::Week1 => 7,
        CoinbaseInterval::Month1 => 31,
    }
}

/// Keeps the initial live-edge request inside one Coinbase source page.
///
/// One whole target bucket is reserved for the candle currently forming. This
/// matters for calendar candles: requesting 350 monthly bars translates to
/// years of daily source pages even though the chart only needs its newest
/// screen before the user scrolls left.
pub(super) fn initial_coinbase_history_bars(
    series: &BarSeriesKey,
    requested_bars: usize,
) -> Result<usize, String> {
    let source_bars = coinbase_source_bars_per_bucket(coinbase_series_interval(series)?);
    let single_page_buckets = COINBASE_HISTORY_PAGE_SOURCE_BARS
        .checked_div(source_bars)
        .unwrap_or(1)
        .saturating_sub(1)
        .max(1);
    Ok(requested_bars.max(1).min(single_page_buckets))
}

pub(super) fn reconcile_interval_history(
    current: &SeriesSnapshot,
    repair: Vec<MarketBar>,
    maximum_bars: usize,
    interval: CoinbaseInterval,
    precedence: HistoryPrecedence,
) -> Result<Vec<MarketBar>, String> {
    let mut merged = BTreeMap::new();
    for bar in current.bars.iter().copied() {
        bar.validate().map_err(|error| error.to_string())?;
        if !coinbase_bar_is_aligned(interval, &bar) {
            return Err("Coinbase current history is not canonical".to_string());
        }
        merged.insert(bar.exchange_timestamp_seconds, bar);
    }
    for bar in repair {
        bar.validate().map_err(|error| error.to_string())?;
        if !coinbase_bar_is_aligned(interval, &bar) {
            return Err("Coinbase repair is not interval aligned".to_string());
        }
        match precedence {
            HistoryPrecedence::Repair => {
                merged.insert(bar.exchange_timestamp_seconds, bar);
            }
            HistoryPrecedence::Current => {
                merged.entry(bar.exchange_timestamp_seconds).or_insert(bar);
            }
        }
    }
    Ok(retain_contiguous_tail(
        &merged.into_values().collect::<Vec<_>>(),
        maximum_bars,
    ))
}

/// Retains the newest contiguous run, bounded to `maximum_bars`.
///
/// `bars` must be sorted by timestamp. The scan walks back from the newest bar
/// and stops at the first sequence break, so the retained window always reaches
/// the live edge and always satisfies the `+1` contract every consumer checks.
pub(super) fn retain_contiguous_tail(bars: &[MarketBar], maximum_bars: usize) -> Vec<MarketBar> {
    if bars.is_empty() || maximum_bars == 0 {
        return Vec::new();
    }
    let mut start = bars.len() - 1;
    while start > 0 && bars.len() - start < maximum_bars {
        if bars[start - 1].source_sequence.checked_add(1) != Some(bars[start].source_sequence) {
            break;
        }
        start -= 1;
    }
    bars[start..].to_vec()
}

/// Restates one stored Coinbase series in canonical form.
///
/// Local history outlives the process, and a store written before bar identity
/// was derived from the bucket holds positional sequences. Re-deriving them here
/// — at the one place stored bars enter the engine — means a restart cannot
/// reintroduce two numbering schemes into the same series. Rithmic history is
/// returned untouched; it owns its own provider sequences.
/// Restores bucket identity on a range read before anything installs it.
///
/// The store numbers what it hands back 1..N — it has no interval to derive
/// identity from — and the exact read canonicalises that away on arrival. The
/// range read did not, so a viewport repair republished the same bars under a
/// second identity and the client read the covering snapshot as a stale one.
pub(super) fn canonical_local_range(
    series: &BarSeriesKey,
    stored: Option<StoredHistory>,
) -> Result<Option<StoredHistory>, String> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    Ok(Some(StoredHistory {
        bars: canonicalize_coinbase_history(series, stored.bars)?,
        ..stored
    }))
}

pub(super) fn canonicalize_coinbase_history(
    series: &BarSeriesKey,
    bars: Vec<MarketBar>,
) -> Result<Vec<MarketBar>, String> {
    if series.provider_id != "coinbase" {
        return Ok(bars);
    }
    let interval = coinbase_series_interval(series)?;
    let mut canonical = BTreeMap::new();
    for mut bar in bars {
        if !coinbase_bar_is_aligned(interval, &bar) {
            continue;
        }
        bar.source_sequence = interval.bucket_sequence(bar.exchange_timestamp_seconds)?;
        if bar.validate().is_err() {
            continue;
        }
        canonical.insert(bar.exchange_timestamp_seconds, bar);
    }
    Ok(retain_contiguous_tail(
        &canonical.into_values().collect::<Vec<_>>(),
        HISTORY_BARS_PER_SERIES,
    ))
}

pub(super) fn reconcile_history_repair(
    current: &SeriesSnapshot,
    repair: Vec<MarketBar>,
    maximum_bars: usize,
    interval: Option<CoinbaseInterval>,
    precedence: HistoryPrecedence,
) -> Result<Vec<MarketBar>, String> {
    if maximum_bars == 0 || current.bars.is_empty() {
        return Err("covering history repair is empty".to_string());
    }
    if let Some(interval) = interval {
        return reconcile_interval_history(current, repair, maximum_bars, interval, precedence);
    }
    if repair.is_empty() {
        return Err("covering history repair is empty".to_string());
    }
    let mut merged = current.bars.to_vec();
    for mut repaired in repair {
        match merged.binary_search_by_key(&repaired.exchange_timestamp_unix_nanos, |bar| {
            bar.exchange_timestamp_unix_nanos
        }) {
            Ok(index) => {
                repaired.source_sequence = merged[index].source_sequence;
                merged[index] = repaired;
            }
            Err(0)
                if repaired.exchange_timestamp_unix_nanos
                    < merged[0].exchange_timestamp_unix_nanos => {}
            Err(index) if index == merged.len() => {
                repaired.source_sequence = merged
                    .last()
                    .and_then(|bar| bar.source_sequence.checked_add(1))
                    .ok_or_else(|| "covering history repair sequence overflowed".to_string())?;
                merged.push(repaired);
            }
            Err(_) => {
                return Err(
                    "covering history repair introduced interior sequence drift".to_string()
                );
            }
        }
    }
    let retained = if merged.len() > maximum_bars {
        merged[merged.len() - maximum_bars..].to_vec()
    } else {
        merged
    };
    for bar in &retained {
        bar.validate().map_err(|error| error.to_string())?;
    }
    if retained.windows(2).any(|pair| {
        pair[0].source_sequence.checked_add(1) != Some(pair[1].source_sequence)
            || pair[0].exchange_timestamp_unix_nanos >= pair[1].exchange_timestamp_unix_nanos
    }) {
        return Err("covering history repair is not canonical".to_string());
    }
    Ok(retained)
}

pub(super) fn recent_coinbase_history_range(
    series: &BarSeriesKey,
    maximum_bars: usize,
) -> Result<HistoryRange, String> {
    let interval = coinbase_series_interval(series)?;
    let now_seconds = current_unix_nanos()?.div_euclid(1_000_000_000);
    let end_seconds = interval.bucket_start(now_seconds)?;
    let start_seconds = interval.shift_bucket(
        end_seconds,
        -i64::try_from(maximum_bars)
            .map_err(|_| "Coinbase recent-history span overflowed".to_string())?,
    )?;
    // The page runs through the source candle the market is trading in right
    // now, not only to the boundary of the open bucket. Coinbase serves an
    // interval it does not publish natively as several source candles, and the
    // later ones are what the forming bar is made of; the end stays on a source
    // boundary because the provider rejects any other alignment.
    let source_seconds = interval.source().1;
    let open_source_end_seconds = (now_seconds - now_seconds.rem_euclid(source_seconds))
        .checked_add(source_seconds)
        .ok_or_else(|| "Coinbase recent-history end overflowed".to_string())?;
    Ok(HistoryRange {
        start_unix_nanos: start_seconds.max(0).saturating_mul(1_000_000_000),
        end_unix_nanos: end_seconds
            .max(open_source_end_seconds)
            .saturating_mul(1_000_000_000),
    })
}

pub(super) fn coinbase_live_edge_repair_range(
    series: &BarSeriesKey,
    completed: &MarketBar,
    current: &MarketBar,
) -> Result<HistoryRange, String> {
    let interval = coinbase_series_interval(series)?;
    if !coinbase_bar_is_aligned(interval, completed)
        || !coinbase_bar_is_aligned(interval, current)
        || completed.exchange_timestamp_seconds >= current.exchange_timestamp_seconds
    {
        return Err("Coinbase live-edge repair range is invalid".to_string());
    }
    Ok(HistoryRange {
        start_unix_nanos: completed.exchange_timestamp_unix_nanos,
        end_unix_nanos: current.exchange_timestamp_unix_nanos,
    })
}

pub(super) fn viewport_coinbase_history_range(
    series: &BarSeriesKey,
    viewport: Viewport,
) -> Result<HistoryRange, String> {
    let interval = coinbase_series_interval(series)?;
    let now_end = interval.bucket_start(current_unix_nanos()?.div_euclid(1_000_000_000))?;
    let visible_start =
        interval.bucket_start(viewport.start_unix_nanos.max(0).div_euclid(1_000_000_000))?;
    let viewport_end = viewport.end_unix_nanos.max(0).div_euclid(1_000_000_000);
    let mut visible_end = interval.bucket_start(viewport_end)?;
    if visible_end < viewport_end {
        visible_end = interval.shift_bucket(visible_end, 1)?;
    }
    visible_end = visible_end.min(now_end);
    if visible_start >= visible_end {
        return recent_coinbase_history_range(series, VIEWPORT_LIVE_TAIL_RESERVE);
    }
    let visible_bars = interval.buckets_between(visible_start, visible_end)?;
    let prefetched_start = interval.shift_bucket(visible_start, -visible_bars)?;
    let bounded_start = interval.shift_bucket(
        now_end,
        -i64::try_from(VIEWPORT_BACKFILL_BARS)
            .map_err(|_| "Coinbase viewport-history span overflowed".to_string())?,
    )?;
    // The request runs from the visible window all the way to the live edge, not
    // just across the window. Coverage pages it back from the edge one page at a
    // time, so what lands is always adjacent to what the series already holds.
    // Fetching the window alone produced an island the series could not join,
    // and splicing that island onto the live edge is what corrupted the series.
    Ok(HistoryRange {
        start_unix_nanos: prefetched_start
            .max(bounded_start)
            .max(0)
            .saturating_mul(1_000_000_000),
        end_unix_nanos: now_end.saturating_mul(1_000_000_000),
    })
}

pub(super) fn coinbase_history_page_range(
    series: &BarSeriesKey,
    missing: HistoryRange,
) -> Result<HistoryRange, String> {
    let interval = coinbase_series_interval(series)?;
    let source_bars_per_bucket = coinbase_source_bars_per_bucket(interval);
    let maximum_buckets = i64::try_from(
        COINBASE_HISTORY_PAGE_SOURCE_BARS
            .checked_div(source_bars_per_bucket)
            .unwrap_or(1),
    )
    .unwrap_or(1)
    .max(1);
    let end_seconds = missing.end_unix_nanos.div_euclid(1_000_000_000);
    let page_start = interval
        .shift_bucket(end_seconds, -maximum_buckets)?
        .saturating_mul(1_000_000_000)
        .max(missing.start_unix_nanos);
    Ok(HistoryRange {
        start_unix_nanos: page_start,
        end_unix_nanos: missing.end_unix_nanos,
    })
}

pub(super) fn current_unix_nanos() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .ok_or_else(|| "system clock is unavailable".to_string())
}

pub(super) fn coinbase_bar_coverage_ranges(
    series: &BarSeriesKey,
    bars: &[MarketBar],
) -> Result<Vec<HistoryRange>, String> {
    if series.provider_id != "coinbase" || bars.is_empty() {
        return Ok(Vec::new());
    }
    let interval = coinbase_series_interval(series)?;
    let mut ranges = Vec::new();
    let mut start = bars[0].exchange_timestamp_unix_nanos;
    let mut previous = start;
    if !coinbase_bar_is_aligned(interval, &bars[0]) {
        return Err("Coinbase history timestamp is not interval aligned".to_string());
    }
    for bar in &bars[1..] {
        let timestamp = bar.exchange_timestamp_unix_nanos;
        if timestamp <= previous || !coinbase_bar_is_aligned(interval, bar) {
            return Err("Coinbase history timestamps are not strictly increasing".to_string());
        }
        let next = coinbase_shift_nanos(interval, previous, 1)?;
        if next != timestamp {
            ranges.push(HistoryRange {
                start_unix_nanos: start,
                end_unix_nanos: next,
            });
            start = timestamp;
        }
        previous = timestamp;
    }
    ranges.push(HistoryRange {
        start_unix_nanos: start,
        end_unix_nanos: coinbase_shift_nanos(interval, previous, 1)?,
    });
    Ok(ranges)
}

pub(super) fn record_covered_range(
    coverage: &mut BTreeMap<BarSeriesKey, Vec<HistoryRange>>,
    series: &BarSeriesKey,
    range: HistoryRange,
) {
    let ranges = coverage.entry(series.clone()).or_default();
    ranges.push(range);
    ranges.sort_unstable();
    let mut merged: Vec<HistoryRange> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        if let Some(previous) = merged.last_mut()
            && range.start_unix_nanos <= previous.end_unix_nanos
        {
            previous.end_unix_nanos = previous.end_unix_nanos.max(range.end_unix_nanos);
        } else {
            merged.push(range);
        }
    }
    *ranges = merged;
}

pub(super) fn coinbase_aggregator(
    profile: CoinbaseSeriesProfile,
) -> Result<CoinbaseBarAggregator, String> {
    CoinbaseBarAggregatorConfig::try_new_period(
        profile.product_id,
        profile.price_scale,
        profile.quantity_scale,
        profile.interval,
        NonZeroUsize::new(HISTORY_BARS_PER_SERIES).unwrap_or(NonZeroUsize::MIN),
    )
    .map(CoinbaseBarAggregator::new)
    .map_err(|error| error.to_string())
}

#[derive(Clone)]
pub(super) struct CoinbaseSeriesProfile {
    pub(super) product_id: String,
    pub(super) resolution: &'static str,
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    pub(super) interval: CoinbaseInterval,
}

pub(super) fn coinbase_series_profile(
    series: &BarSeriesKey,
    installed: &InstallProviderInstrument,
) -> Result<CoinbaseSeriesProfile, String> {
    if series.provider_id != "coinbase" || series.definition_version != 1 {
        return Err("unsupported Coinbase engine series identity".to_string());
    }
    if installed.provider != "coinbase"
        || installed.instrument_id != series.instrument_id
        || installed.entitlement_id != series.entitlement_id
        || installed.venue_id != "coinbase"
        || installed.price_scale > 18
        || installed.quantity_scale > 18
    {
        return Err("Coinbase installed instrument does not match the series".to_string());
    }
    let interval = coinbase_series_interval(series)?;
    Ok(CoinbaseSeriesProfile {
        product_id: installed.provider_symbol.clone(),
        resolution: interval.id(),
        price_scale: u8::try_from(installed.price_scale)
            .map_err(|_| "Coinbase price scale is invalid".to_string())?,
        quantity_scale: u8::try_from(installed.quantity_scale)
            .map_err(|_| "Coinbase quantity scale is invalid".to_string())?,
        interval,
    })
}

pub(super) fn coinbase_interval(seconds: u32) -> Result<CoinbaseInterval, String> {
    match seconds {
        60 => Ok(CoinbaseInterval::Minute1),
        180 => Ok(CoinbaseInterval::Minute3),
        300 => Ok(CoinbaseInterval::Minute5),
        900 => Ok(CoinbaseInterval::Minute15),
        1_800 => Ok(CoinbaseInterval::Minute30),
        3_600 => Ok(CoinbaseInterval::Hour1),
        7_200 => Ok(CoinbaseInterval::Hour2),
        14_400 => Ok(CoinbaseInterval::Hour4),
        28_800 => Ok(CoinbaseInterval::Hour8),
        43_200 => Ok(CoinbaseInterval::Hour12),
        86_400 => Ok(CoinbaseInterval::Day1),
        _ => Err("unsupported Coinbase engine derivation interval".to_string()),
    }
}

pub(super) fn coinbase_series_interval(series: &BarSeriesKey) -> Result<CoinbaseInterval, String> {
    match series.period {
        BarPeriod::Time { seconds } => coinbase_interval(seconds),
        BarPeriod::Week { weeks: 1 } => Ok(CoinbaseInterval::Week1),
        BarPeriod::Month { months: 1 } => Ok(CoinbaseInterval::Month1),
        _ => Err("unsupported Coinbase engine interval".to_string()),
    }
}

pub(super) fn coinbase_bar_is_aligned(interval: CoinbaseInterval, bar: &MarketBar) -> bool {
    bar.exchange_timestamp_unix_nanos.rem_euclid(1_000_000_000) == 0
        && interval.bucket_start(bar.exchange_timestamp_seconds)
            == Ok(bar.exchange_timestamp_seconds)
}

pub(super) fn coinbase_shift_nanos(
    interval: CoinbaseInterval,
    timestamp_nanos: i64,
    buckets: i64,
) -> Result<i64, String> {
    interval
        .shift_bucket(timestamp_nanos.div_euclid(1_000_000_000), buckets)?
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase history coverage overflowed".to_string())
}

pub(super) fn internal_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    if series.provider.trim().is_empty()
        || series.instrument_id.trim().is_empty()
        || series.entitlement_id.trim().is_empty()
        || series.definition_revision == 0
    {
        return Err("market series identity is invalid".to_string());
    }
    let period = match SeriesCadence::try_from(series.cadence)
        .map_err(|_| "market series cadence is invalid".to_string())?
    {
        SeriesCadence::FixedSeconds => BarPeriod::time(series.cadence_value),
        SeriesCadence::Trades => BarPeriod::tick(series.cadence_value),
        SeriesCadence::SessionDays => BarPeriod::session(series.cadence_value),
        SeriesCadence::CalendarWeeks => BarPeriod::week(series.cadence_value),
        SeriesCadence::CalendarMonths => BarPeriod::month(series.cadence_value),
        SeriesCadence::Unspecified => {
            Err(axiusflow_market_data::MarketDataValidationError::InvalidPeriod)
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: series.entitlement_id.clone(),
        period,
        definition_version: series.definition_revision,
    })
}

pub(super) fn retained_hot_series(
    workspace: &WorkspaceState,
    available_memory_bytes: u64,
) -> Result<Vec<HotSeries>, String> {
    const MAXIMUM_HOT_SET_BYTES: u64 = 512 * 1024 * 1024;
    let estimated_entry_bytes = (HISTORY_BARS_PER_SERIES + 1)
        .saturating_mul(std::mem::size_of::<MarketBar>())
        .saturating_add(1_024);
    let memory_budget_bytes =
        usize::try_from((available_memory_bytes / 16).min(MAXIMUM_HOT_SET_BYTES))
            .unwrap_or(usize::MAX);
    let mut manager =
        HotSetManager::new(NonZeroUsize::new(MAXIMUM_SERIES).unwrap_or(NonZeroUsize::MIN));
    manager
        .restore(
            workspace
                .hot_series
                .iter()
                .map(crate::protocol_hot_entry)
                .collect::<Result<Vec<_>, _>>()?,
        )
        .map_err(|error| error.to_string())?;
    let active_workspaces = NonZeroU64::new(workspace.active_workspace_id)
        .map(WorkspaceId)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let watchlist = workspace
        .hot_series
        .iter()
        .filter(|series| {
            workspace
                .watchlist
                .iter()
                .any(|market| market == &series.market || market == &series.provider_symbol)
        })
        .map(|series| {
            internal_series(&SeriesKey {
                provider: series.provider.clone(),
                instrument_id: series.instrument_id.clone(),
                cadence_value: series.cadence_value,
                definition_revision: series.definition_revision,
                entitlement_id: series.entitlement_id.clone(),
                cadence: series.cadence,
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(manager
        .classify(
            &active_workspaces,
            &watchlist,
            memory_budget_bytes,
            estimated_entry_bytes,
        )
        .into_iter()
        .filter(|retention| retention.tier != HotSetTier::Cold)
        .map(|retention| crate::hot_entry_to_protocol(retention.entry))
        .collect())
}

pub(super) fn warm_series(series: &HotSeries) -> Result<WarmSeries, String> {
    let expected_account = match series.provider.as_str() {
        "coinbase" => COINBASE_PUBLIC_ACCOUNT_ID,
        "rithmic" => RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        _ => return Err("hot-set provider is unsupported".to_string()),
    };
    if series.account_id != expected_account
        || series.provider_symbol.trim().is_empty()
        || series.venue_id.trim().is_empty()
        || series.display_symbol.trim().is_empty()
        || series.price_scale > 18
        || series.quantity_scale > 18
    {
        return Err("hot-set instrument metadata is invalid".to_string());
    }
    let canonical = internal_series(&SeriesKey {
        provider: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        cadence_value: series.cadence_value,
        definition_revision: series.definition_revision,
        entitlement_id: series.entitlement_id.clone(),
        cadence: series.cadence,
    })?;
    Ok(WarmSeries {
        series: canonical,
        instrument: InstallProviderInstrument {
            provider: series.provider.clone(),
            session_generation: series.provider_watermark.max(1),
            selection_generation: 1,
            instrument_id: series.instrument_id.clone(),
            provider_symbol: series.provider_symbol.clone(),
            display_symbol: series.display_symbol.clone(),
            venue_id: series.venue_id.clone(),
            price_scale: series.price_scale,
            quantity_scale: series.quantity_scale,
            entitlement_id: series.entitlement_id.clone(),
        },
        provider_watermark: series.provider_watermark,
    })
}

pub(super) fn spawn_history_worker(
    name: &'static str,
    source: Box<dyn HistorySource>,
    requests: Receiver<HistoryRequest>,
    completions: SyncSender<Command>,
    shutdown: Arc<AtomicBool>,
    active_workers: Arc<Mutex<BTreeSet<String>>>,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _activity = ActiveWorkerGuard::register(name, active_workers);
            run_history_worker(source, &requests, &completions, &shutdown);
        })
        .map_err(|error| error.to_string())
}

pub(super) fn run_history_worker(
    mut source: Box<dyn HistorySource>,
    requests: &Receiver<HistoryRequest>,
    completions: &SyncSender<Command>,
    shutdown: &AtomicBool,
) {
    while let Ok(request) = requests.recv() {
        if shutdown.load(Ordering::Acquire) {
            request.stop.store(true, Ordering::Release);
            return;
        }
        let result = source.fetch(&request);
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        if completions
            .send(Command::HistoryCompleted(
                request.series,
                request.provider_generation,
                request.range,
                request.kind,
                result,
            ))
            .is_err()
        {
            return;
        }
    }
}

impl Coordinator<'_> {
    /// Enqueues history required to recover an already accepted live series.
    ///
    /// Queue pressure retains one coordinator-tick-driven retry per series. A
    /// reconnect or publication failure must not depend on a later provider
    /// heartbeat to retry the covering request, and repeated recovery signals
    /// must not grow duplicate work.
    pub(super) fn enqueue_history_recovery(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        match self.enqueue_history(series, generation) {
            Err(HISTORY_CAPACITY_EXHAUSTED) => {
                self.history_retries
                    .entry((series.clone(), generation))
                    .or_insert((Instant::now() + HISTORY_RETRY_DELAY, 0));
                Ok(())
            }
            result => result,
        }
    }

    pub(super) fn restore_hot_series(&mut self, series: Vec<WarmSeries>) {
        for warm in series
            .into_iter()
            .take(self.resource_policy.maximum_derived_series.max(1))
        {
            let generation = ProviderGeneration(
                NonZeroU64::new(warm.provider_watermark.max(1)).unwrap_or(NonZeroU64::MIN),
            );
            if warm.series.provider_id == "coinbase" {
                self.catalog.insert(
                    (
                        warm.instrument.provider.clone(),
                        warm.instrument.instrument_id.clone(),
                    ),
                    warm.instrument.clone(),
                );
                self.catalog_sessions
                    .insert("coinbase".to_string(), COINBASE_PROVIDER_GENERATION);
            }
            self.warm_priority.push(warm.series.clone());
            self.warm_series.insert(warm.series.clone(), warm.clone());
            if self
                .storage
                .try_send(StorageRequest::Read(warm.series.clone(), generation))
                .is_ok()
            {
                self.warming.insert((warm.series, generation));
            }
        }
    }

    pub(super) fn activate_markets_live_hot_set(&mut self) {
        self.retained_live
            .extend(self.live.keys().chain(self.rithmic_live.keys()).cloned());
        let priority = self.warm_priority.clone();
        for series in priority
            .iter()
            .filter(|series| series.provider_id == "coinbase")
        {
            self.retained_live.insert(series.clone());
            if let Ok(generation) = self.provider_generation_for_series(series) {
                let _ = self.ensure_realtime(series);
                let _ = self.enqueue_history(series, generation);
            }
        }
        let Some(series) = priority
            .into_iter()
            .find(|series| series.provider_id == "rithmic")
        else {
            return;
        };
        let Some(warm) = self.warm_series.get(&series) else {
            return;
        };
        let provider_symbol = warm.instrument.provider_symbol.clone();
        self.warm_rithmic_search_generation = self
            .warm_rithmic_search_generation
            .checked_add(1)
            .unwrap_or(1);
        let search_generation = self.warm_rithmic_search_generation;
        let _ = self.providers.dispatch_catalog(
            "rithmic",
            ProviderCatalogCommand::Search(SearchProviderInstruments {
                consumer_id: 0,
                search_generation,
                provider: "rithmic".to_string(),
                query: provider_symbol,
                maximum_results: 16,
            }),
        );
    }

    pub(super) fn prepare_cached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        snapshot: &Arc<axiusflow_market_engine::SeriesSnapshot>,
    ) -> Result<bool, String> {
        let coinbase_live = series.provider_id != "coinbase"
            || self
                .live
                .get(series)
                .is_some_and(|live| live.history.is_ready());
        let needs_covering_repair = snapshot.provider_generation != provider_generation
            || self.prewarmed.remove(series)
            || !coinbase_live;
        if series.provider_id == "rithmic"
            && !needs_covering_repair
            && let Err(error) = self.start_rithmic_realtime_from_snapshot(series, snapshot)
        {
            return Err(error);
        }
        if needs_covering_repair {
            self.local_loaded
                .insert((series.clone(), provider_generation));
            if let Err(detail) = self.enqueue_history(series, provider_generation) {
                self.broadcast_series_resolution_for(
                    series,
                    SeriesLoadState::Partial,
                    PersistenceState::Durable,
                    Some(detail),
                );
            }
        }
        Ok(needs_covering_repair)
    }

    pub(super) fn start_uncached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        waiter: DemandWaiter,
    ) {
        let first = !self.pending.contains_key(series);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.series_state = Some(series_state(
                waiter.consumer_id,
                waiter.generation,
                ipc_series(series),
                SeriesLoadState::Resolving,
                None,
            ));
        }
        self.pending.entry(series.clone()).or_default().push(waiter);
        if !first {
            return;
        }
        // Coinbase's first consumer-visible image comes from its own current
        // provider page. Publishing a coarser projection of another cached
        // timeframe first created a plausible-looking but stale chart, then the
        // live trade tail opened several buckets later. Flowsurface avoids that
        // seam by using provider klines keyed on candle start; we keep the same
        // invariant here while retaining the resident trade session.
        let local_history_started = series.provider_id != "coinbase"
            && self
                .enqueue_local_history(series, provider_generation)
                .is_ok();
        let history = if local_history_started {
            Ok(())
        } else {
            self.enqueue_history(series, provider_generation)
        };
        if let Err(detail) = history
            && let Some(waiters) = self.pending.remove(series)
        {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::ProviderHistory,
                detail,
            );
        }
    }

    pub(super) fn remember_pending_empty_repair(
        &mut self,
        series: &BarSeriesKey,
        range: HistoryRange,
    ) {
        self.pending_empty_repairs
            .entry(series.clone())
            .and_modify(|pending| {
                pending.start_unix_nanos = pending.start_unix_nanos.min(range.start_unix_nanos);
                pending.end_unix_nanos = pending.end_unix_nanos.max(range.end_unix_nanos);
            })
            .or_insert(range);
    }

    pub(super) fn retry_pending_empty_repairs(&mut self) {
        if self.pending_empty_repairs.is_empty() || Instant::now() < self.empty_repair_retry_at {
            return;
        }
        let Some((series, range)) = self
            .pending_empty_repairs
            .iter()
            .next()
            .map(|(series, range)| (series.clone(), *range))
        else {
            return;
        };
        match self
            .storage
            .try_send(StorageRequest::ResolveConfirmedEmpty(series.clone(), range))
        {
            Ok(()) => {
                self.pending_empty_repairs.remove(&series);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.empty_repair_retry_at = Instant::now() + EMPTY_REPAIR_RETRY_DELAY;
            }
        }
    }

    pub(super) fn schedule_history_retry(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        error: &str,
    ) -> bool {
        let key = (series.clone(), generation);
        let attempts = self
            .history_retries
            .get(&key)
            .map_or(1, |(_, attempts)| attempts.saturating_add(1));
        eprintln!(
            "Axiusflow engine Coinbase history attempt {attempts} failed for {}: {error}",
            series.instrument_id
        );
        if attempts > MAXIMUM_HISTORY_RETRIES {
            self.history_retries.remove(&key);
            return false;
        }
        self.history_retries
            .insert(key, (Instant::now() + HISTORY_RETRY_DELAY, attempts));
        self.broadcast_provider_for(
            "coinbase",
            ProviderConnectionState::Recovering,
            generation,
            Some("Coinbase current history is retrying"),
        );
        true
    }

    pub(super) fn retry_history(&mut self) {
        let now = Instant::now();
        let Some(key) = self
            .history_retries
            .iter()
            .find(|(key, (retry_at, _))| {
                now >= *retry_at && !self.history_inflight.contains_key(*key)
            })
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        let (series, generation) = &key;
        let current_generation = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        if !self.engine.has_subscription(series) || current_generation != Some(*generation) {
            self.history_retries.remove(&key);
            return;
        }
        let _ = self.enqueue_history(series, *generation);
        if let Some((retry_at, _)) = self.history_retries.get_mut(&key) {
            *retry_at = now + HISTORY_RETRY_DELAY;
        }
    }

    pub(super) fn protected_history_ranges(&self) -> Vec<(BarSeriesKey, HistoryRange)> {
        let mut protected = BTreeSet::new();
        for (consumer_id, viewport) in &self.active_viewports {
            if self
                .engine
                .current_demand(*consumer_id)
                .is_some_and(|demand| {
                    demand.generation == Some(viewport.consumer_generation)
                        && demand.series.as_ref() == Some(&viewport.series)
                })
                && self
                    .engine
                    .provider_status(&viewport.series.provider_id)
                    .and_then(|status| status.generation)
                    == Some(viewport.provider_generation)
            {
                protected.insert((viewport.series.clone(), viewport.range));
            }
        }
        for series in self.live.keys().chain(self.rithmic_live.keys()) {
            if !self.engine.has_subscription(series) {
                continue;
            }
            let Some(snapshot) = self.engine.series_snapshot(series) else {
                continue;
            };
            let Some(first) = snapshot.bars.get(
                snapshot
                    .bars
                    .len()
                    .saturating_sub(VIEWPORT_LIVE_TAIL_RESERVE),
            ) else {
                continue;
            };
            let Some(last) = snapshot.bars.last() else {
                continue;
            };
            let end = last
                .exchange_timestamp_unix_nanos
                .saturating_add(series.period.duration_nanos().unwrap_or(1).max(1));
            if first.exchange_timestamp_unix_nanos < end {
                protected.insert((
                    series.clone(),
                    HistoryRange {
                        start_unix_nanos: first.exchange_timestamp_unix_nanos,
                        end_unix_nanos: end,
                    },
                ));
            }
        }
        protected.into_iter().collect()
    }

    pub(super) fn provider_generation_for_series(
        &self,
        series: &BarSeriesKey,
    ) -> Result<ProviderGeneration, String> {
        match series.provider_id.as_str() {
            "coinbase" => {
                self.coinbase_instrument(series)
                    .and_then(|instrument| coinbase_series_profile(series, instrument))?;
                Ok(self.coinbase_provider_generation())
            }
            "rithmic" => {
                crate::rithmic_history::chart_interval(series.period)?;
                if series.definition_version != 1 {
                    return Err("unsupported Rithmic engine series definition".to_string());
                }
                let installed = self.rithmic_instrument(series)?;
                if installed.entitlement_id != series.entitlement_id {
                    return Err("Rithmic series entitlement is inconsistent".to_string());
                }
                let generation = self
                    .engine
                    .provider_status("rithmic")
                    .and_then(|status| status.generation)
                    .ok_or_else(|| "Rithmic engine session is unavailable".to_string())?;
                Ok(generation)
            }
            _ => Err("resident engine market provider is unsupported".to_string()),
        }
    }

    pub(super) fn series_precision(&self, series: &BarSeriesKey) -> Result<(u8, u8), String> {
        match series.provider_id.as_str() {
            "coinbase" => {
                let profile = coinbase_series_profile(series, self.coinbase_instrument(series)?)?;
                Ok((profile.price_scale, profile.quantity_scale))
            }
            "rithmic" => {
                let installed = self.rithmic_instrument(series)?;
                if installed.entitlement_id != series.entitlement_id {
                    return Err("Rithmic series entitlement is inconsistent".to_string());
                }
                Ok((
                    u8::try_from(installed.price_scale)
                        .map_err(|_| "Rithmic price scale is invalid".to_string())?,
                    u8::try_from(installed.quantity_scale)
                        .map_err(|_| "Rithmic quantity scale is invalid".to_string())?,
                ))
            }
            _ => Err("resident engine market provider is unsupported".to_string()),
        }
    }

    pub(super) fn rithmic_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .ok_or_else(|| "Rithmic instrument is not installed".to_string())
    }

    pub(super) fn coinbase_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .filter(|instrument| instrument.provider == "coinbase")
            .ok_or_else(|| "Coinbase instrument is not installed".to_string())
    }

    pub(super) fn enqueue_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        let range = if series.provider_id == "coinbase" {
            initial_coinbase_history_bars(series, self.resource_policy.history_prefetch_bars.max(1))
                .and_then(|bars| recent_coinbase_history_range(series, bars))
                .ok()
        } else {
            None
        };
        self.enqueue_history_request(series, generation, range, HistoryRequestKind::Initial)
    }

    pub(super) fn request_viewport_history(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        viewport: Viewport,
    ) -> Result<(), String> {
        let Some(demand) = self.engine.current_demand(consumer_id) else {
            return Err("market consumer is unavailable".to_string());
        };
        if demand.generation != Some(generation) {
            return Ok(());
        }
        let Some(series) = demand.series.clone() else {
            return Ok(());
        };
        if series.provider_id != "coinbase" {
            return Ok(());
        }
        let provider_generation = self.provider_generation_for_series(&series)?;
        let range = viewport_coinbase_history_range(&series, viewport)?;
        self.active_viewports.insert(
            consumer_id,
            ActiveViewport {
                consumer_generation: generation,
                provider_generation,
                series: series.clone(),
                range,
            },
        );
        let key = (series.clone(), provider_generation);
        // The provider fetch and canonical cache are shared by series, but the
        // viewport intent belongs to each consumer. Plan against their union:
        // a workspace at the live edge must never replace another workspace's
        // older range or cancel the page it is already fetching.
        let range = self
            .combined_active_viewport_range(&series, provider_generation)
            .unwrap_or(range);
        let replaced = self.viewport_history_ranges.insert(key.clone(), range);
        if replaced != Some(range) {
            self.viewport_history_retries.remove(&key);
        }
        if self.viewport_history_retries.contains_key(&key) {
            return Ok(());
        }
        let key = (series.clone(), provider_generation, range);
        if !self.viewport_history_local_inflight.insert(key.clone()) {
            return Ok(());
        }
        match self.storage.try_send(StorageRequest::ReadRange(
            series.clone(),
            provider_generation,
            range,
        )) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.viewport_history_local_inflight.remove(&key);
            }
        }
        self.schedule_next_coinbase_viewport_page(&series, provider_generation);
        Ok(())
    }

    fn combined_active_viewport_range(
        &self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Option<HistoryRange> {
        self.active_viewports
            .values()
            .filter(|viewport| {
                viewport.series == *series && viewport.provider_generation == generation
            })
            .map(|viewport| viewport.range)
            .reduce(|left, right| HistoryRange {
                start_unix_nanos: left.start_unix_nanos.min(right.start_unix_nanos),
                end_unix_nanos: left.end_unix_nanos.max(right.end_unix_nanos),
            })
    }

    /// Seeds the live aggregator from installed history and drains the buffer.
    ///
    /// The seam is closed in three steps, in this order:
    ///
    /// 1. Closed buckets seed the aggregator's completed history.
    /// 2. The bucket that was still open when the page was served seeds the
    ///    in-flight bar, so a chart the trader has just selected opens on the
    ///    real current candle instead of on the first trade after the click.
    /// 3. Trades buffered during the fetch are replayed in order, skipping
    ///    everything the served page already covers so nothing is counted twice.
    ///
    /// The handoff always completes — a series left waiting publishes nothing at
    /// all, and on a thin product it would wait for the whole quiet stretch.
    pub(super) fn complete_coinbase_live_handoff(
        &mut self,
        series: &BarSeriesKey,
        bars: &[MarketBar],
        forming: Option<FormingBar>,
        handoff_boundary_unix_nanos: Option<i64>,
        provisional_edge: bool,
    ) -> bool {
        let already_live = self
            .live
            .get(series)
            .is_some_and(|live| live.history.is_ready());
        if already_live {
            self.resync_coinbase_live(series);
            return true;
        }
        let Some(live) = self.live.get_mut(series) else {
            return true;
        };
        let connected = live.connected;
        let buffered = std::mem::take(&mut live.buffered);
        live.aggregator.reset();
        let forming = forming.map(|forming| forming.bar).filter(|forming| {
            bars.last().is_none_or(|last| {
                last.exchange_timestamp_seconds < forming.exchange_timestamp_seconds
            })
        });
        let seeded = match forming {
            Some(forming) => {
                let mut seed = Vec::with_capacity(bars.len() + 1);
                seed.extend_from_slice(bars);
                seed.push(forming);
                live.aggregator.seed_backfill(&seed)
            }
            None => live.aggregator.seed_history(bars),
        };
        let boundary = handoff_boundary_unix_nanos.unwrap_or(i64::MIN);
        let mut rolled = Vec::new();
        let replay_failed = if seeded.is_ok() {
            buffered
                .iter()
                .filter(|trade| trade.trade_time_unix_nanos > boundary)
                .any(|trade| match live.aggregator.apply_trade(trade) {
                    Ok(Some(completed)) => {
                        if let Some(current) = live.aggregator.in_flight() {
                            rolled.push((completed, current));
                        }
                        false
                    }
                    Ok(None) => false,
                    Err(_) => true,
                })
        } else {
            false
        };
        if seeded.is_err() || replay_failed {
            self.realtime_interrupted(
                FailureStage::Handoff,
                "Coinbase history/live handoff failed",
            );
            return false;
        }
        live.connected = connected;
        live.history = if provisional_edge {
            CoinbaseHistoryReadiness::Provisional
        } else {
            CoinbaseHistoryReadiness::Authoritative
        };
        live.dirty = live.aggregator.in_flight().is_some();
        // The series holds every seeded bar as completed, so only what the
        // aggregator opens after the handoff still needs appending.
        live.published_completed = bars.last().map(|bar| bar.source_sequence);
        let generation = live.generation;
        for (completed, current) in rolled {
            self.schedule_coinbase_live_edge_repair(series, generation, &completed, &current);
        }
        true
    }

    /// Realigns an already-live aggregator with the series a repair just
    /// installed, without discarding the bucket currently forming.
    ///
    /// A backfill extends history backwards and can keep or drop the forming
    /// tail, so what the series holds as *completed* moves. Bar identity comes
    /// from the bucket, so nothing is renumbered — only the append cursor has to
    /// follow. If the aggregator turns out not to line up after all, the next
    /// publication fails for this one series and reseeds it.
    pub(super) fn resync_coinbase_live(&mut self, series: &BarSeriesKey) {
        let Some(snapshot) = self.engine.series_snapshot(series) else {
            return;
        };
        let completed = if snapshot.forming {
            snapshot
                .bars
                .len()
                .checked_sub(2)
                .map(|index| snapshot.bars[index].source_sequence)
        } else {
            snapshot.bars.last().map(|bar| bar.source_sequence)
        };
        if let Some(live) = self.live.get_mut(series) {
            live.published_completed = completed;
            live.dirty = live.aggregator.in_flight().is_some();
        }
    }

    pub(super) fn schedule_coinbase_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        kind: HistoryRequestKind,
    ) -> Result<(), String> {
        let key = (series.clone(), generation);
        if !self.engine.has_subscription(series) {
            self.viewport_history_ranges.remove(&key);
            return Ok(());
        }
        if self.history_inflight.contains_key(&key) {
            return Ok(());
        }
        let Some(requested) = self.viewport_history_ranges.get(&key).copied() else {
            return Ok(());
        };
        let coverage = CoverageSnapshot::try_new(
            self.history_coverage
                .get(series)
                .cloned()
                .unwrap_or_default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .map_err(|error| error.to_string())?;
        let plan = coverage
            .plan(requested)
            .map_err(|error| error.to_string())?;
        let Some(missing) = plan.repair_ranges().first().copied() else {
            self.viewport_history_ranges.remove(&key);
            self.flush_deferred_publication(series);
            return Ok(());
        };
        let page = coinbase_history_page_range(series, missing)?;
        self.enqueue_history_request(series, generation, Some(page), kind)
            .map_err(str::to_string)
    }

    pub(super) fn schedule_next_coinbase_viewport_page(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) {
        if let Err(error) =
            self.schedule_coinbase_history(series, generation, HistoryRequestKind::ViewportBackfill)
        {
            eprintln!(
                "Axiusflow engine could not queue Coinbase viewport history for {}: {error}",
                series.instrument_id
            );
            self.viewport_backfill_failed(
                series,
                generation,
                "Visible history repair could not be queued; retained data remains usable",
            );
        }
    }

    pub(super) fn enqueue_history_request(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        kind: HistoryRequestKind,
    ) -> Result<(), &'static str> {
        if self
            .engine
            .verify_provider_request(&series.provider_id, ProviderRequest::HistoricalBars)
            .is_err()
        {
            return Err("provider does not support historical bars");
        }
        let key = (series.clone(), generation);
        if self.history_inflight.contains_key(&key) {
            return Ok(());
        }
        let instrument = if matches!(series.provider_id.as_str(), "coinbase" | "rithmic") {
            self.catalog
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .cloned()
        } else {
            None
        };
        let stop = Arc::new(AtomicBool::new(false));
        let request = HistoryRequest {
            series: series.clone(),
            provider_generation: generation,
            instrument,
            maximum_bars: self.resource_policy.history_prefetch_bars.max(1),
            range,
            kind,
            stop: Arc::clone(&stop),
        };
        let history = self.providers.history(&series.provider_id)?;
        match try_enqueue_history(history, request) {
            Ok(()) => {
                self.history_inflight.insert(key.clone(), range);
                self.history_cancellations.insert(key, stop);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn history_failed(&mut self, series: &BarSeriesKey, generation: ProviderGeneration) {
        if self.local_loaded.contains(&(series.clone(), generation)) {
            self.pending.remove(series);
            if let Ok(publications) = self.engine.publish_series_snapshot(series) {
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_state(
                            events,
                            &publication,
                            SeriesLoadState::Partial,
                            PersistenceState::Durable,
                            Some(
                                "Retained local history is usable; provider repair is unavailable",
                            ),
                        );
                    }
                }
            }
            if series.provider_id == "rithmic"
                && let Some(snapshot) = self.engine.series_snapshot(series)
            {
                let _ = self.start_rithmic_realtime_from_snapshot(series, &snapshot);
            }
        } else if let Some(waiters) = self.pending.remove(series) {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::ProviderHistory,
                if series.provider_id == "rithmic" {
                    "Rithmic historical bars are unavailable"
                } else {
                    "Coinbase historical bars are unavailable"
                },
            );
        }
        if series.provider_id == "coinbase" {
            self.broadcast_provider_for(
                "coinbase",
                ProviderConnectionState::Recovering,
                generation,
                Some("Coinbase current history is unavailable"),
            );
        }
    }

    pub(super) fn handle_empty_history_snapshot(
        &mut self,
        series: &BarSeriesKey,
        kind: HistoryRequestKind,
        key: &(BarSeriesKey, ProviderGeneration),
        requested: Option<HistoryRange>,
        snapshot: &HistorySnapshot,
    ) -> bool {
        if kind != HistoryRequestKind::ViewportBackfill {
            return false;
        }
        if !snapshot.confirmed_empty {
            return false;
        }
        if let Some(requested) = requested
            && self
                .storage
                .try_send(StorageRequest::RecordConfirmedEmpty(
                    series.clone(),
                    requested,
                ))
                .is_err()
        {
            self.broadcast_persistence_for(
                series,
                PersistenceState::Degraded,
                Some("Confirmed-empty history persistence is unavailable"),
            );
        }
        if let Some(requested) = requested {
            record_covered_range(&mut self.history_coverage, series, requested);
        }
        self.viewport_history_retries.remove(key);
        self.schedule_next_coinbase_viewport_page(series, key.1);
        if !self.viewport_history_ranges.contains_key(key) && !self.series_live_if_ready(series) {
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Partial,
                PersistenceState::Durable,
                Some("Coinbase has no bars in the requested range"),
            );
        }
        true
    }

    pub(super) fn accept_history_completion(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        kind: HistoryRequestKind,
        result: Result<HistorySnapshot, String>,
    ) -> Option<HistorySnapshot> {
        let key = (series.clone(), generation);
        self.history_inflight.remove(&key);
        let cancelled = self
            .history_cancellations
            .remove(&key)
            .is_some_and(|stop| stop.load(Ordering::Acquire));
        let suspended = self.suspended_history.remove(&key);
        let current = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        if current != Some(generation) {
            self.pending_live_edge_repairs.remove(&key);
            self.viewport_history_retries.remove(&key);
            let pending_viewport = self.viewport_history_ranges.remove(&key);
            if series.provider_id == "coinbase"
                && let Some(current) = current
                && self.live.contains_key(series)
            {
                if let Some(range) = pending_viewport {
                    self.viewport_history_ranges
                        .insert((series.clone(), current), range);
                }
                let _ = self.enqueue_history(series, current);
            }
            return None;
        }
        if suspended {
            self.pending_live_edge_repairs.remove(&key);
            if self.resource_mode != ResourceMode::OfflineSuspended
                && self.engine.has_subscription(series)
            {
                if self.viewport_history_ranges.contains_key(&key) {
                    self.schedule_next_coinbase_viewport_page(series, generation);
                } else {
                    let _ = self.enqueue_history(series, generation);
                }
            }
            return None;
        }
        self.flush_coinbase_live_edge_repair(series, generation);
        let snapshot = match result {
            Ok(snapshot) => {
                self.history_retries.remove(&key);
                snapshot
            }
            Err(error) => {
                if cancelled {
                    if self.viewport_history_ranges.contains_key(&key) {
                        self.schedule_next_coinbase_viewport_page(series, generation);
                    } else if self.engine.has_subscription(series) {
                        let _ = self.enqueue_history(series, generation);
                    }
                } else if kind == HistoryRequestKind::Initial {
                    let retrying = series.provider_id == "coinbase"
                        && self.schedule_history_retry(series, generation, &error);
                    if !retrying {
                        self.history_failed(series, generation);
                    }
                } else if kind == HistoryRequestKind::ViewportBackfill {
                    self.viewport_backfill_failed(
                        series,
                        generation,
                        "Visible history backfill is unavailable; retained data remains usable",
                    );
                } else if let HistoryRequestKind::LiveEdgeRepair(attempt) = kind {
                    self.retry_coinbase_live_edge_repair(series, generation, range, attempt);
                }
                return None;
            }
        };
        if cancelled
            && kind == HistoryRequestKind::ViewportBackfill
            && self.viewport_history_ranges.get(&key).copied() != range
        {
            self.schedule_next_coinbase_viewport_page(series, generation);
            return None;
        }
        Some(snapshot)
    }

    pub(super) fn history_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        kind: HistoryRequestKind,
        result: Result<HistorySnapshot, String>,
    ) {
        let key = (series.clone(), generation);
        let Some(snapshot) =
            self.accept_history_completion(series, generation, range, kind, result)
        else {
            return;
        };
        if let HistoryRequestKind::LiveEdgeRepair(attempt) = kind
            && snapshot.bars.is_empty()
        {
            self.retry_coinbase_live_edge_repair(series, generation, range, attempt);
            return;
        }
        if snapshot.bars.is_empty()
            && self.handle_empty_history_snapshot(series, kind, &key, range, &snapshot)
        {
            return;
        }
        let repair = kind != HistoryRequestKind::Initial;
        let replace_covering = series.provider_id == "coinbase" && repair;
        let provider_bars = snapshot.bars.clone();
        let Some(snapshot) = self.prepare_history_repair(series, generation, snapshot, repair)
        else {
            if repair {
                self.history_repair_failed(
                    series,
                    generation,
                    range,
                    kind,
                    "Visible history could not be merged; retained data remains usable",
                );
            }
            return;
        };
        let persisted_bars = if replace_covering {
            provider_bars
        } else {
            snapshot.bars.clone()
        };
        let publish = matches!(kind, HistoryRequestKind::LiveEdgeRepair(_))
            || !replace_covering
            || self.coinbase_repair_publishes(series, range);
        if publish {
            self.deferred_publications.remove(series);
        } else {
            self.deferred_publications.insert(series.clone());
        }
        let handoff_boundary_unix_nanos = snapshot.handoff_boundary_unix_nanos;
        let Some((bars, forming)) = self.install_completed_history(
            series,
            generation,
            snapshot,
            replace_covering,
            persisted_bars,
            publish,
        ) else {
            if repair {
                self.history_repair_failed(
                    series,
                    generation,
                    range,
                    kind,
                    "Visible history could not be installed; retained data remains usable",
                );
            }
            return;
        };
        let provisional_edge = kind == HistoryRequestKind::Initial && forming.is_some();
        if !self.complete_coinbase_live_handoff(
            series,
            &bars,
            forming,
            handoff_boundary_unix_nanos,
            provisional_edge,
        ) {
            return;
        }
        if matches!(kind, HistoryRequestKind::LiveEdgeRepair(_))
            && let Some(live) = self.live.get_mut(series)
        {
            live.history = CoinbaseHistoryReadiness::Authoritative;
        }
        // Fold the seeded/replayed open candle into the queued covering image
        // before `Live` can uncover the chart. `ConsumerEvents` coalesces this
        // revision into that snapshot, so initialization reaches the desktop as
        // one current image instead of a history paint followed by a burst.
        self.publish_live();
        self.pending.remove(series);
        if !self.series_live_if_ready(series) {
            self.provider_online_if_all_series_ready();
        }
        if let Err(error) = self.record_coinbase_history_coverage(series, &bars) {
            self.broadcast_demand_error_for(series, FailureStage::ProviderHistory, &error, None);
        }
        if kind == HistoryRequestKind::ViewportBackfill {
            self.viewport_history_retries.remove(&key);
        }
        // Initial demand stops at the newest bounded window. Older coverage is
        // requested only from `request_viewport_history` when a chart actually
        // moves left, then persisted for the next consumer of the same series.
        self.schedule_next_coinbase_viewport_page(series, generation);
    }

    pub(super) fn record_coinbase_history_coverage(
        &mut self,
        series: &BarSeriesKey,
        bars: &[MarketBar],
    ) -> Result<(), String> {
        for range in coinbase_bar_coverage_ranges(series, bars)? {
            record_covered_range(&mut self.history_coverage, series, range);
        }
        Ok(())
    }

    /// Reports whether one Coinbase covering repair is consumer-visible. An
    /// interior page publishes only while a consumer viewport actually overlaps
    /// the fetched range, so background working-window fills stay off the IPC
    /// path until their plan resolves.
    pub(super) fn coinbase_repair_publishes(
        &self,
        series: &BarSeriesKey,
        page: Option<HistoryRange>,
    ) -> bool {
        let Some(page) = page else {
            return false;
        };
        self.active_viewports.values().any(|viewport| {
            viewport.series == *series
                && viewport.range.start_unix_nanos < page.end_unix_nanos
                && page.start_unix_nanos < viewport.range.end_unix_nanos
        })
    }

    pub(super) fn viewport_backfill_failed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        detail: &'static str,
    ) {
        let key = (series.clone(), generation);
        if self.viewport_history_ranges.contains_key(&key) {
            let attempts = self
                .viewport_history_retries
                .get(&key)
                .map_or(1, |retry| retry.attempts.saturating_add(1));
            eprintln!(
                "Axiusflow engine Coinbase viewport history attempt {attempts} failed for {}: {detail}",
                series.instrument_id
            );
            if attempts > MAXIMUM_VIEWPORT_HISTORY_RETRIES {
                self.viewport_history_retries.remove(&key);
                self.viewport_history_ranges.remove(&key);
                self.broadcast_demand_error_for(
                    series,
                    FailureStage::ProviderHistory,
                    "Visible history repair exhausted bounded retries; move the viewport to retry",
                    None,
                );
                self.flush_deferred_publication(series);
                self.broadcast_series_resolution_for(
                    series,
                    SeriesLoadState::Partial,
                    PersistenceState::Durable,
                    Some("Visible history repair is unavailable; move the viewport to retry"),
                );
                return;
            }
            self.viewport_history_retries.insert(
                key,
                PendingViewportHistoryRetry {
                    ready_at: Instant::now()
                        + VIEWPORT_HISTORY_RETRY_DELAY.saturating_mul(u32::from(attempts)),
                    attempts,
                },
            );
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Partial,
                PersistenceState::Durable,
                Some(detail),
            );
            return;
        }
        self.flush_deferred_publication(series);
        if !self.series_live_if_ready(series) {
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Partial,
                PersistenceState::Durable,
                Some(detail),
            );
        }
    }

    fn history_repair_failed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        kind: HistoryRequestKind,
        detail: &'static str,
    ) {
        match kind {
            HistoryRequestKind::ViewportBackfill => {
                self.viewport_backfill_failed(series, generation, detail);
            }
            HistoryRequestKind::LiveEdgeRepair(attempt) => {
                self.retry_coinbase_live_edge_repair(series, generation, range, attempt);
            }
            HistoryRequestKind::Initial => {}
        }
    }

    pub(super) fn retry_viewport_history(&mut self) {
        if self.resource_mode == ResourceMode::OfflineSuspended {
            return;
        }
        let now = Instant::now();
        let Some(key) = self
            .viewport_history_retries
            .iter()
            .find(|(key, retry)| now >= retry.ready_at && !self.history_inflight.contains_key(*key))
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        let (series, generation) = &key;
        let current_generation = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        if !self.engine.has_subscription(series)
            || current_generation != Some(*generation)
            || !self.viewport_history_ranges.contains_key(&key)
        {
            self.viewport_history_retries.remove(&key);
            return;
        }
        if let Some(retry) = self.viewport_history_retries.get_mut(&key) {
            retry.ready_at =
                now + VIEWPORT_HISTORY_RETRY_DELAY.saturating_mul(u32::from(retry.attempts.max(1)));
        }
        self.schedule_next_coinbase_viewport_page(series, *generation);
    }

    pub(super) fn install_completed_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        snapshot: HistorySnapshot,
        replace_covering: bool,
        persisted_bars: Vec<MarketBar>,
        publish: bool,
    ) -> Option<(Vec<MarketBar>, Option<FormingBar>)> {
        let price_scale = snapshot.price_scale;
        let quantity_scale = snapshot.quantity_scale;
        let handoff_boundary_unix_nanos = snapshot.handoff_boundary_unix_nanos;
        let mut forming = snapshot.forming;
        // Rithmic closes its own seam inside this install; Coinbase closes it
        // afterwards, against the bars the engine actually accepted.
        let rithmic_forming = if series.provider_id == "rithmic" {
            forming.take()
        } else {
            None
        };
        let bars = snapshot.bars;
        let realtime_bars = forming
            .as_ref()
            .map(|forming| [bars.as_slice(), std::slice::from_ref(&forming.bar)].concat());
        let installed = if replace_covering {
            self.engine.replace_covering_history(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
                publish,
            )
        } else if series.provider_id == "coinbase"
            && let Some(realtime_bars) = realtime_bars
        {
            self.engine.install_realtime(
                generation,
                series,
                price_scale,
                quantity_scale,
                realtime_bars,
                true,
            )
        } else {
            self.engine.install_history(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
            )
        };
        let publications = match installed {
            Ok(publications) => publications,
            Err(error) => {
                if let Some(waiters) = self.pending.remove(series) {
                    fail_waiters(
                        &mut self.events,
                        waiters,
                        series,
                        engine_install_failure_stage(&error),
                        &error.to_string(),
                    );
                }
                return None;
            }
        };
        for publication in publications {
            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                publish_state(
                    events,
                    &publication,
                    SeriesLoadState::Ready,
                    PersistenceState::Pending,
                    None,
                );
            }
        }
        self.enqueue_persistence(
            series,
            generation,
            persisted_bars,
            false,
            "Local history persistence is unavailable",
        );
        if let Err(error) = self.finish_rithmic_history_handoff(
            series,
            generation,
            RithmicHandoffSeed {
                price_scale,
                quantity_scale,
                bars: &bars,
                forming: rithmic_forming,
                handoff_boundary_unix_nanos,
            },
        ) {
            if let Some(waiters) = self.pending.remove(series) {
                fail_waiters(
                    &mut self.events,
                    waiters,
                    series,
                    FailureStage::Handoff,
                    &error,
                );
            }
            return None;
        }
        Some((bars, forming))
    }

    pub(super) fn prepare_history_repair(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        mut snapshot: HistorySnapshot,
        repair: bool,
    ) -> Option<HistorySnapshot> {
        if repair {
            let Some(current) = self.engine.series_snapshot(series) else {
                return Some(snapshot);
            };
            let Ok(bars) = reconcile_history_repair(
                &current,
                snapshot.bars,
                HISTORY_BARS_PER_SERIES,
                Some(coinbase_series_interval(series).ok()?),
                HistoryPrecedence::Repair,
            ) else {
                return None;
            };
            snapshot.bars = bars;
            self.local_loaded.remove(&(series.clone(), generation));
            return Some(snapshot);
        }
        if !self.local_loaded.contains(&(series.clone(), generation)) {
            return Some(snapshot);
        }
        let Some(current) = self.engine.series_snapshot(series) else {
            self.history_failed(series, generation);
            return None;
        };
        let merged = reconcile_history_repair(
            &current,
            snapshot.bars,
            HISTORY_BARS_PER_SERIES,
            coinbase_series_interval(series).ok(),
            HistoryPrecedence::Repair,
        );
        let Ok(bars) = merged else {
            self.history_failed(series, generation);
            return None;
        };
        snapshot.bars = bars;
        self.local_loaded.remove(&(series.clone(), generation));
        self.engine.invalidate_series(series);
        Some(snapshot)
    }

    pub(super) fn start_rithmic_realtime_from_snapshot(
        &mut self,
        series: &BarSeriesKey,
        snapshot: &axiusflow_market_engine::SeriesSnapshot,
    ) -> Result<(), String> {
        self.ensure_realtime(series)?;
        // A retained series whose newest bar is still forming hands that bar over
        // as the open period rather than as closed history, so the first live
        // trade revises it instead of being dropped against a bar the provider
        // never closed.
        let (bars, forming) = match snapshot.bars.split_last() {
            Some((last, head)) if snapshot.forming => (
                head,
                Some(FormingBar {
                    bar: *last,
                    trades: None,
                }),
            ),
            _ => (snapshot.bars.as_ref(), None),
        };
        let handoff_boundary_unix_nanos = snapshot
            .bars
            .last()
            .map(|bar| bar.exchange_timestamp_unix_nanos);
        if self.seed_rithmic_history(
            series,
            snapshot.provider_generation,
            RithmicHandoffSeed {
                price_scale: snapshot.price_scale,
                quantity_scale: snapshot.quantity_scale,
                bars,
                forming,
                handoff_boundary_unix_nanos,
            },
        ) {
            Ok(())
        } else {
            Err("Rithmic cached history/live handoff failed".to_string())
        }
    }

    pub(super) fn finish_rithmic_history_handoff(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        seed: RithmicHandoffSeed<'_>,
    ) -> Result<(), String> {
        if series.provider_id != "rithmic" {
            return Ok(());
        }
        self.ensure_realtime(series)?;
        self.seed_rithmic_history(series, generation, seed)
            .then_some(())
            .ok_or_else(|| "Rithmic history/live handoff failed".to_string())
    }

    pub(super) fn seed_rithmic_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        seed: RithmicHandoffSeed<'_>,
    ) -> bool {
        let Some(live) = self.rithmic_live.get_mut(series) else {
            return true;
        };
        let RithmicHandoffSeed {
            price_scale,
            quantity_scale,
            bars,
            forming,
            handoff_boundary_unix_nanos,
        } = seed;
        if live
            .seed(
                price_scale,
                quantity_scale,
                bars,
                forming,
                handoff_boundary_unix_nanos,
            )
            .is_ok()
        {
            return true;
        }
        live.history_ready = false;
        live.dirty = false;
        self.broadcast_provider_for(
            "rithmic",
            ProviderConnectionState::Recovering,
            generation,
            Some("Rithmic history/live handoff failed"),
        );
        false
    }

    pub(super) fn prune_history_tracking(&mut self) {
        self.active_viewports.retain(|consumer_id, viewport| {
            self.engine
                .current_demand(*consumer_id)
                .is_some_and(|demand| {
                    demand.generation == Some(viewport.consumer_generation)
                        && demand.series.as_ref() == Some(&viewport.series)
                })
                && self
                    .engine
                    .provider_status(&viewport.series.provider_id)
                    .and_then(|status| status.generation)
                    == Some(viewport.provider_generation)
        });
        let pending_viewports = self
            .viewport_history_ranges
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for key in pending_viewports {
            let (series, generation) = &key;
            if let Some(range) = self.combined_active_viewport_range(series, *generation) {
                let changed =
                    self.viewport_history_ranges.insert(key.clone(), range) != Some(range);
                if changed {
                    self.viewport_history_retries.remove(&key);
                }
            } else {
                self.viewport_history_ranges.remove(&key);
                self.viewport_history_retries.remove(&key);
            }
        }
        let unused_coverage = self
            .history_coverage
            .keys()
            .filter(|series| {
                !self.engine.has_subscription(series)
                    && self.engine.series_snapshot(series).is_none()
            })
            .cloned()
            .collect::<Vec<_>>();
        for series in unused_coverage {
            self.history_coverage.remove(&series);
        }
        let obsolete_viewports = self
            .viewport_history_ranges
            .keys()
            .filter(|(series, generation)| {
                !self.engine.has_subscription(series)
                    || self
                        .engine
                        .provider_status(&series.provider_id)
                        .and_then(|status| status.generation)
                        != Some(*generation)
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in obsolete_viewports {
            self.viewport_history_ranges.remove(&key);
            self.viewport_history_retries.remove(&key);
        }
        self.history_retries.retain(|(series, generation), _| {
            self.engine.has_subscription(series)
                && self
                    .engine
                    .provider_status(&series.provider_id)
                    .and_then(|status| status.generation)
                    == Some(*generation)
        });
        self.viewport_history_retries
            .retain(|(series, generation), _| {
                self.viewport_history_ranges
                    .contains_key(&(series.clone(), *generation))
                    && self.engine.has_subscription(series)
                    && self
                        .engine
                        .provider_status(&series.provider_id)
                        .and_then(|status| status.generation)
                        == Some(*generation)
            });
        self.pending_live_edge_repairs
            .retain(|(series, generation), _| {
                self.engine.has_subscription(series)
                    && self
                        .engine
                        .provider_status(&series.provider_id)
                        .and_then(|status| status.generation)
                        == Some(*generation)
            });
        self.deferred_publications
            .retain(|series| self.engine.has_subscription(series));
    }
}
