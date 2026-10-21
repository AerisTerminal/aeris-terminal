use super::coordinator::StudyChange;
use super::lifecycle::CandleHandoffSeed;
use super::{
    ActiveWorkerGuard, Arc, AtomicBool, BTreeSet, BarSeriesKey, Command, ConsumerId, Coordinator,
    DemandWaiter, EMPTY_CURRENT_HISTORY_DETAIL, EMPTY_CURRENT_HISTORY_RETAINED_DETAIL,
    EmptyCurrentHistory, FailureStage, FormingBar, GenerationId, HISTORY_CAPACITY_EXHAUSTED,
    HISTORY_SERIES_HIGH_WATERMARK, HISTORY_SERIES_TARGET_BARS, HistoryRange, HistoryRequest,
    HistorySnapshot, HistorySource, INITIAL_HISTORY_BARS, InstallProviderInstrument,
    MAXIMUM_HISTORY_BARS_PER_REQUEST, MAXIMUM_HISTORY_RETRIES, MarketBar, Mutex, Ordering,
    ProviderGeneration, ProviderRequest, Receiver, SeriesLoadState, SeriesSnapshot, SyncSender,
    TradeHandoffSeed, VIEWPORT_LIVE_TAIL_RESERVE, Viewport, engine_install_failure_stage,
    fail_waiters, publish_state, series_state, thread, try_enqueue_history,
};
use aeris_observability::diagnostic;

pub(super) mod tracker;
use tracker::{DeferredRequest, HistoryTicket, LaneKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewportRefillPlan {
    Current,
    Range(HistoryRange),
}

struct CompactedHistory {
    bars: Vec<MarketBar>,
    dropped_newer: bool,
}

struct PreparedHistoryRepair {
    snapshot: HistorySnapshot,
    replace_window: bool,
    detached_from_live: bool,
}

#[derive(Clone, Copy)]
enum CompletedHistoryInstall {
    Initial,
    Covering { reseed_live: bool },
    Window { reseed_live: bool },
}

impl CompletedHistoryInstall {
    const fn reseeds_live(self) -> bool {
        match self {
            Self::Initial => true,
            Self::Covering { reseed_live } | Self::Window { reseed_live } => reseed_live,
        }
    }
}

pub(super) fn reconcile_history_repair(
    current: &SeriesSnapshot,
    repair: Vec<MarketBar>,
) -> Result<Vec<MarketBar>, String> {
    if current.bars.is_empty() || repair.is_empty() {
        return Err("covering history repair is empty".to_string());
    }
    let mut by_timestamp = std::collections::BTreeMap::new();
    let current_completed = if current.forming {
        current
            .bars
            .get(..current.bars.len().saturating_sub(1))
            .unwrap_or(&[])
    } else {
        current.bars.as_ref()
    };
    for bar in current_completed.iter().copied() {
        by_timestamp.insert(bar.exchange_timestamp_unix_nanos, bar);
    }
    // Provider repair is authoritative for overlapping buckets and may extend
    // either edge of a shorter retained local window.
    for bar in repair {
        by_timestamp.insert(bar.exchange_timestamp_unix_nanos, bar);
    }
    // Flowsurface retains every fetched kline in its BTreeMap. Do the same at
    // Aeris's canonical owner: a provider request remains page-bounded, but
    // older successful pages must not be thrown away when the series grows.
    let mut retained = by_timestamp.into_values().collect::<Vec<_>>();
    for (index, bar) in retained.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "covering history repair sequence overflowed".to_string())?;
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

fn merge_deferred_request(deferred: &mut DeferredRequest, range: Option<HistoryRange>) {
    match (*deferred, range) {
        (DeferredRequest::Full, _) => {}
        (DeferredRequest::Range(_), None) => *deferred = DeferredRequest::Full,
        (DeferredRequest::Range(mut existing), Some(range)) => {
            existing.start_unix_nanos = existing.start_unix_nanos.min(range.start_unix_nanos);
            existing.end_unix_nanos = existing.end_unix_nanos.max(range.end_unix_nanos);
            *deferred = DeferredRequest::Range(existing);
        }
    }
}

pub(super) fn history_request_bar_limit(
    series: &BarSeriesKey,
    range: Option<HistoryRange>,
) -> usize {
    range.map_or(INITIAL_HISTORY_BARS, |requested| {
        series
            .period
            .duration_nanos()
            .and_then(|duration| {
                let span = requested
                    .end_unix_nanos
                    .saturating_sub(requested.start_unix_nanos);
                let duration = duration.max(1);
                let bars = span
                    .saturating_add(duration.saturating_sub(1))
                    .div_euclid(duration);
                usize::try_from(bars).ok()
            })
            .unwrap_or(INITIAL_HISTORY_BARS)
            .saturating_add(64)
            .clamp(1, MAXIMUM_HISTORY_BARS_PER_REQUEST)
    })
}

fn viewport_history_range(
    series: &BarSeriesKey,
    snapshot: &aeris_market_engine::SeriesSnapshot,
    viewport: Viewport,
) -> Option<HistoryRange> {
    let first = snapshot.bars.first()?;
    if viewport.start_unix_nanos >= first.exchange_timestamp_unix_nanos {
        return None;
    }
    // Match Flowsurface: when the visible left edge escapes retained coverage,
    // fetch one additional visible span behind it so back-scroll has runway.
    let visible_span = viewport
        .end_unix_nanos
        .saturating_sub(viewport.start_unix_nanos);
    let mut start_unix_nanos = viewport.start_unix_nanos.saturating_sub(visible_span);
    let end_unix_nanos = first.exchange_timestamp_unix_nanos;
    if let Some(duration) = series.period.duration_nanos() {
        let maximum = MAXIMUM_HISTORY_BARS_PER_REQUEST
            .saturating_sub(VIEWPORT_LIVE_TAIL_RESERVE)
            .max(1);
        if let Ok(maximum) = i64::try_from(maximum)
            && let Some(span) = duration.checked_mul(maximum)
        {
            start_unix_nanos = start_unix_nanos.max(end_unix_nanos.saturating_sub(span));
        }
    }
    (start_unix_nanos < end_unix_nanos).then_some(HistoryRange {
        start_unix_nanos,
        end_unix_nanos,
    })
}

fn newer_viewport_history_range(
    series: &BarSeriesKey,
    snapshot: &aeris_market_engine::SeriesSnapshot,
    viewport: Viewport,
) -> Option<HistoryRange> {
    let last = snapshot.bars.last()?;
    let visible_span = viewport
        .end_unix_nanos
        .saturating_sub(viewport.start_unix_nanos);
    let start_unix_nanos = last.exchange_timestamp_unix_nanos;
    let mut end_unix_nanos = viewport.end_unix_nanos.saturating_add(visible_span);
    if let Some(duration) = series.period.duration_nanos() {
        let maximum = MAXIMUM_HISTORY_BARS_PER_REQUEST
            .saturating_sub(VIEWPORT_LIVE_TAIL_RESERVE)
            .max(1);
        if let Ok(maximum) = i64::try_from(maximum)
            && let Some(span) = duration.checked_mul(maximum)
        {
            end_unix_nanos = end_unix_nanos.min(start_unix_nanos.saturating_add(span));
        }
    }
    (start_unix_nanos < end_unix_nanos).then_some(HistoryRange {
        start_unix_nanos,
        end_unix_nanos,
    })
}

fn viewport_refill_plan(
    series: &BarSeriesKey,
    snapshot: &aeris_market_engine::SeriesSnapshot,
    viewport: Viewport,
    detached_from_live: bool,
    live_coverage: Option<(i64, i64)>,
) -> Option<ViewportRefillPlan> {
    if let Some(range) = viewport_history_range(series, snapshot, viewport) {
        return Some(ViewportRefillPlan::Range(range));
    }
    if !detached_from_live {
        return None;
    }
    if live_coverage.is_some_and(|(start, _)| viewport.end_unix_nanos >= start) {
        return Some(ViewportRefillPlan::Current);
    }
    newer_viewport_history_range(series, snapshot, viewport).map(ViewportRefillPlan::Range)
}

fn compact_history_window(
    bars: Vec<MarketBar>,
    viewport: Option<Viewport>,
    reserve_forming_tail: bool,
) -> CompactedHistory {
    let reserved = usize::from(reserve_forming_tail);
    let high = HISTORY_SERIES_HIGH_WATERMARK
        .saturating_sub(reserved)
        .max(1);
    if bars.len() < high {
        return CompactedHistory {
            bars,
            dropped_newer: false,
        };
    }
    let retained = HISTORY_SERIES_TARGET_BARS
        .saturating_sub(reserved)
        .max(1)
        .min(bars.len());
    let (start, end) = retained_window_bounds(&bars, viewport, retained);
    CompactedHistory {
        dropped_newer: end < bars.len(),
        bars: bars[start..end].to_vec(),
    }
}

fn retained_window_bounds(
    bars: &[MarketBar],
    viewport: Option<Viewport>,
    retained: usize,
) -> (usize, usize) {
    if retained >= bars.len() {
        return (0, bars.len());
    }
    let Some(viewport) = viewport else {
        return (bars.len() - retained, bars.len());
    };
    let visible_span = viewport
        .end_unix_nanos
        .saturating_sub(viewport.start_unix_nanos);
    let focus_start = viewport.start_unix_nanos.saturating_sub(visible_span);
    let focus_end = viewport.end_unix_nanos;
    let first = bars.partition_point(|bar| bar.exchange_timestamp_unix_nanos < focus_start);
    let last = bars.partition_point(|bar| bar.exchange_timestamp_unix_nanos < focus_end);
    if first >= last {
        let anchor = bars
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos < viewport.start_unix_nanos)
            .min(bars.len().saturating_sub(1));
        let start = anchor
            .saturating_sub(retained / 2)
            .min(bars.len() - retained);
        return (start, start + retained);
    }
    let focus_len = last - first;
    if focus_len >= retained {
        let start = first.min(bars.len() - retained);
        return (start, start + retained);
    }
    let spare = retained - focus_len;
    let mut start = first.saturating_sub(spare / 2);
    let mut end = (last + spare.saturating_sub(first - start)).min(bars.len());
    if end - start < retained {
        start = end.saturating_sub(retained);
    }
    end = start + retained;
    (start, end)
}

/// Removes coverage already owned by the current backwards history request.
fn history_range_after_inflight(
    requested: HistoryRange,
    inflight: HistoryRange,
) -> Option<HistoryRange> {
    if inflight.start_unix_nanos <= requested.start_unix_nanos
        && inflight.end_unix_nanos >= requested.end_unix_nanos
    {
        return None;
    }
    if requested.start_unix_nanos < inflight.start_unix_nanos
        && requested.end_unix_nanos > inflight.start_unix_nanos
        && requested.end_unix_nanos <= inflight.end_unix_nanos
    {
        return Some(HistoryRange {
            start_unix_nanos: requested.start_unix_nanos,
            end_unix_nanos: inflight.start_unix_nanos,
        });
    }
    Some(requested)
}

/// Removes a provider-confirmed empty suffix from a backwards viewport fetch.
///
/// Historical viewport requests end at the first retained canonical bar. An
/// empty provider result therefore describes the newest missing suffix at that
/// edge. Repeating the same viewport is suppressed; scrolling farther left
/// requests only the older prefix that has not yet been classified.
fn history_range_after_confirmed_empty(
    requested: HistoryRange,
    confirmed_empty: HistoryRange,
) -> Option<HistoryRange> {
    if confirmed_empty.start_unix_nanos <= requested.start_unix_nanos
        && confirmed_empty.end_unix_nanos >= requested.end_unix_nanos
    {
        return None;
    }
    if requested.start_unix_nanos < confirmed_empty.start_unix_nanos
        && requested.end_unix_nanos > confirmed_empty.start_unix_nanos
        && confirmed_empty.end_unix_nanos >= requested.end_unix_nanos
    {
        return Some(HistoryRange {
            start_unix_nanos: requested.start_unix_nanos,
            end_unix_nanos: confirmed_empty.start_unix_nanos,
        });
    }
    if confirmed_empty.start_unix_nanos <= requested.start_unix_nanos
        && confirmed_empty.end_unix_nanos > requested.start_unix_nanos
        && requested.end_unix_nanos > confirmed_empty.end_unix_nanos
    {
        return Some(HistoryRange {
            start_unix_nanos: confirmed_empty.end_unix_nanos,
            end_unix_nanos: requested.end_unix_nanos,
        });
    }
    Some(requested)
}

fn merge_confirmed_empty_range(existing: &mut HistoryRange, range: HistoryRange) {
    if range.start_unix_nanos <= existing.end_unix_nanos
        && existing.start_unix_nanos <= range.end_unix_nanos
    {
        existing.start_unix_nanos = existing.start_unix_nanos.min(range.start_unix_nanos);
        existing.end_unix_nanos = existing.end_unix_nanos.max(range.end_unix_nanos);
    } else {
        // A disjoint empty range can only arise after the canonical left edge
        // has moved. Keep the newest observation; older internal gaps no longer
        // participate in backwards-edge request planning.
        *existing = range;
    }
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
    let mut queued = Vec::<HistoryRequest>::new();
    let mut current_burst = 0usize;
    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        if queued.is_empty() {
            match requests.recv_timeout(super::COORDINATOR_TICK) {
                Ok(request) => queued.push(request),
                Err(super::RecvTimeoutError::Timeout) => continue,
                Err(super::RecvTimeoutError::Disconnected) => return,
            }
        }
        while queued.len() < super::HISTORY_CAPACITY {
            let Ok(request) = requests.try_recv() else {
                break;
            };
            queued.push(request);
        }
        // Current repair/initial load precede optional backfill, but every
        // fourth slot serves the oldest waiting request to prevent starvation.
        let index = if current_burst < 3 {
            queued
                .iter()
                .position(|request| request.range.is_none())
                .unwrap_or(0)
        } else {
            queued
                .iter()
                .position(|request| request.range.is_some())
                .unwrap_or(0)
        };
        let request = queued.remove(index);
        current_burst = if request.range.is_some() || current_burst == 3 {
            0
        } else {
            current_burst + 1
        };
        let result = if request.stop.load(Ordering::Acquire) {
            Err("history request cancelled before fetch".to_string())
        } else {
            source.fetch(&request)
        };
        let completion = Command::HistoryCompleted(
            request.series,
            request.provider_generation,
            request.range,
            result,
            request.ticket,
        );
        if completions.send(completion).is_err() {
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
        self.enqueue_history_request_with_capacity_retry(series, generation, None)
    }

    /// Retains one bounded retry ticket and publishes an observable series state
    /// when an immediate recovery request cannot enter the history worker.
    pub(super) fn request_series_history_recovery(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) {
        if let Err(error) = self.enqueue_history_recovery(series, generation) {
            diagnostic!(
                "Aeris market history enqueue failed for {}: {error}",
                series.instrument_id
            );
            self.schedule_history_retry(series, generation, None, error);
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Partial,
                Some("Current market history is unavailable; recovery will retry"),
            );
        }
    }

    pub(super) fn enqueue_history_request_with_capacity_retry(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
    ) -> Result<(), &'static str> {
        match self.enqueue_history_request(series, generation, range) {
            Err(HISTORY_CAPACITY_EXHAUSTED) => {
                diagnostic!(
                    "Aeris market history queue full for {}; bounded retry scheduled",
                    series.instrument_id
                );
                self.broadcast_series_resolution_for(
                    series,
                    SeriesLoadState::Partial,
                    Some("Current market history is queued for retry"),
                );
                Ok(())
            }
            result => result,
        }
    }
    pub(super) fn prepare_cached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        snapshot: &Arc<aeris_market_engine::SeriesSnapshot>,
    ) -> Result<bool, String> {
        let needs_covering_repair = snapshot.provider_generation != provider_generation;
        if needs_covering_repair {
            if let Err(detail) = self.enqueue_history(series, provider_generation) {
                self.broadcast_series_resolution_for(
                    series,
                    SeriesLoadState::Partial,
                    Some(detail),
                );
            }
        } else if self.provider_uses_candles(&series.provider_id) {
            self.restore_candle_from_snapshot(series, snapshot)?;
        } else {
            self.restore_trade_from_snapshot(series, snapshot)?;
        }
        Ok(needs_covering_repair)
    }
    pub(super) fn start_uncached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        waiter: DemandWaiter,
    ) {
        if self.current_history_known_empty(series, provider_generation) {
            // The provider already answered this series for this generation.
            // A re-stated demand is answered from that record, not re-fetched.
            if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
                events.series_state = Some(series_state(
                    waiter.consumer_id,
                    waiter.generation,
                    series.clone(),
                    SeriesLoadState::Empty,
                    Some(EMPTY_CURRENT_HISTORY_DETAIL.to_string()),
                ));
            }
            return;
        }
        let first = !self.pending.contains_key(series);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.series_state = Some(series_state(
                waiter.consumer_id,
                waiter.generation,
                series.clone(),
                SeriesLoadState::Resolving,
                None,
            ));
        }
        self.pending.entry(series.clone()).or_default().push(waiter);
        if !first {
            return;
        }
        if let Err(detail) = self.enqueue_history_recovery(series, provider_generation)
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

    pub(super) fn schedule_history_retry(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        error: &str,
    ) -> bool {
        let Some(epoch) = self
            .provider_epochs
            .current(&series.provider_id, generation.0.get())
        else {
            return false;
        };
        let kind = if range.is_some() {
            LaneKind::Backfill
        } else {
            LaneKind::Current
        };
        let attempts = self
            .history
            .pending(series, epoch, kind)
            .map_or(1, |(_, _, attempts)| attempts);
        diagnostic!(
            "Aeris engine {} history attempt {attempts} failed for {}: {error}",
            series.provider_id,
            series.instrument_id
        );
        let retry = self.history.schedule(
            series.clone(),
            epoch,
            kind,
            range,
            self.clock.as_ref(),
            false,
        );
        // An older-range page sits behind the published series; retrying it
        // leaves the current series state untouched.
        if retry && range.is_none() {
            self.broadcast_series_recovery_for(series, "Current market history is retrying");
        }
        retry
    }

    pub(super) fn retry_history(&mut self) {
        let now = self.clock.monotonic();
        let Some((series, epoch, _kind, range)) = self.history.retry_due(now) else {
            return;
        };
        let current_generation = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        let Some(generation) = current_generation else {
            return;
        };
        if !self.engine.has_subscription(&series)
            || self
                .provider_epochs
                .current(&series.provider_id, generation.0.get())
                != Some(epoch)
        {
            self.prune_history_tracking();
            return;
        }
        if let Err(error) = self.enqueue_history_request(&series, generation, range) {
            self.schedule_history_retry(&series, generation, range, error);
            if range.is_none() {
                self.broadcast_series_resolution_for(
                    &series,
                    SeriesLoadState::Partial,
                    Some("Current market history is unavailable; recovery will retry"),
                );
            }
        }
    }

    pub(super) fn provider_generation_for_series(
        &self,
        series: &BarSeriesKey,
    ) -> Result<ProviderGeneration, String> {
        self.providers
            .validate_period(&series.provider_id, series.period)?;
        if self.provider_uses_candles(&series.provider_id) {
            if series.definition_version != 1 {
                return Err("unsupported Provider engine series definition".to_string());
            }
            let installed = self.candle_instrument(series)?;
            if installed.entitlement_id != series.entitlement_id {
                return Err("Provider series entitlement is inconsistent".to_string());
            }
            return self
                .engine
                .provider_status(&series.provider_id)
                .and_then(|status| status.generation)
                .ok_or_else(|| "Provider engine session is unavailable".to_string());
        }
        if series.definition_version != 1 {
            return Err("unsupported Rithmic engine series definition".to_string());
        }
        let installed = self.rithmic_instrument(series)?;
        if installed.entitlement_id != series.entitlement_id {
            return Err("Rithmic series entitlement is inconsistent".to_string());
        }
        self.engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            .ok_or_else(|| "Rithmic engine session is unavailable".to_string())
    }
    pub(super) fn rithmic_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .ok_or_else(|| "Rithmic instrument is not installed".to_string())
    }

    pub(super) fn candle_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .ok_or_else(|| "Provider instrument is not installed".to_string())
    }
    pub(super) fn enqueue_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        self.enqueue_history_request(series, generation, None)
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
        if !demand.resource_class.retains_subscription() {
            // Viewport intent may still be queued when a consumer is demoted to
            // Warm/Detached. The demand registry owns that lifecycle: once the
            // consumer no longer retains an upstream subscription, stale
            // presentation commands must not schedule cache or provider work.
            return Ok(());
        }
        let Some(series) = demand.series.clone() else {
            return Ok(());
        };
        if self.engine.primary_retained_viewport(&series).is_some_and(
            |(primary, primary_generation, _)| {
                primary != consumer_id || primary_generation != generation
            },
        ) {
            // One canonical bounded window cannot follow two disjoint visible
            // ranges at once. Foreground ConsumerId ordering is deterministic;
            // only the primary viewport schedules provider work.
            return Ok(());
        }
        let Some(snapshot) = self.engine.series_snapshot(&series) else {
            // Initial history already owns the unresolved series. Once the
            // first covering snapshot lands, later viewport movement can ask
            // for older coverage without duplicating startup work.
            return Ok(());
        };
        let provider_generation = self.provider_generation_for_series(&series)?;
        let detached = self
            .provider_epochs
            .current(&series.provider_id, provider_generation.0.get())
            .is_some_and(|epoch| self.history.is_detached(&series, epoch));
        let Some(plan) = viewport_refill_plan(
            &series,
            &snapshot,
            viewport,
            detached,
            self.live_history_coverage(&series),
        ) else {
            return Ok(());
        };
        match plan {
            ViewportRefillPlan::Current => {
                self.begin_live_history_reseed(&series);
                let result = self.enqueue_history_request_with_capacity_retry(
                    &series,
                    provider_generation,
                    None,
                );
                if result.is_err() {
                    let _ = self.cancel_live_history_reseed(&series);
                }
                result
            }
            ViewportRefillPlan::Range(range) => self.enqueue_history_request_with_capacity_retry(
                &series,
                provider_generation,
                Some(range),
            ),
        }
        .map_err(str::to_string)
    }

    fn live_history_coverage(&self, series: &BarSeriesKey) -> Option<(i64, i64)> {
        if self.provider_uses_candles(&series.provider_id) {
            self.series.candle(series)?.coverage()
        } else {
            self.series.trade(series)?.coverage()
        }
    }

    fn begin_live_history_reseed(&mut self, series: &BarSeriesKey) {
        if self.provider_uses_candles(&series.provider_id) {
            if let Some(live) = self.series.candle_mut(series) {
                live.begin_history_reseed();
            }
        } else if let Some(live) = self.series.trade_lifecycle_mut(series) {
            live.begin_trade_history_reseed();
        }
    }

    fn cancel_live_history_reseed(&mut self, series: &BarSeriesKey) -> Result<(), String> {
        if self.provider_uses_candles(&series.provider_id) {
            if let Some(live) = self.series.candle_mut(series) {
                live.cancel_history_reseed()?;
            }
        } else if let Some(live) = self.series.trade_lifecycle_mut(series) {
            live.cancel_trade_history_reseed()?;
        }
        Ok(())
    }

    /// Schedules a covering or page-bounded older-history request. A second
    /// request for the same series is deferred until the active page completes.
    fn history_epoch_for_request(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<
        aeris_market_engine::fence::Epoch<aeris_market_engine::fence::ProviderTag>,
        &'static str,
    > {
        if !self.ensure_provider_connection(&series.provider_id) {
            return Err("provider connection is not ready");
        }
        #[cfg(not(test))]
        let _ = generation;
        #[cfg(test)]
        if !self.provider_registry.contains(&series.provider_id) {
            return self
                .provider_epochs
                .stamp(
                    &series.provider_id,
                    generation.0.get(),
                    &mut self.provider_registry,
                )
                .ok_or("history request belongs to a retired provider session");
        }
        self.provider_registry
            .epoch(&series.provider_id)
            .ok_or("provider connection is not yet reserved")
    }

    pub(super) fn enqueue_history_request(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
    ) -> Result<(), &'static str> {
        self.engine
            .verify_provider_request(&series.provider_id, ProviderRequest::HistoricalBars)
            .map_err(|_| "provider does not support historical bars")?;
        let epoch = self.history_epoch_for_request(series, generation)?;
        let kind = if range.is_some() {
            LaneKind::Backfill
        } else {
            LaneKind::Current
        };
        let range = if let Some(requested) = range {
            let Some(missing) = self.uncovered_history_range(series, epoch, requested) else {
                return Ok(());
            };
            Some(missing)
        } else {
            if self.current_history_known_empty(series, generation) {
                // Every recovery trigger (heartbeat, re-stated demand, study
                // lease, retry ticket) funnels through here; none of them is new
                // evidence against the provider's empty answer.
                return Ok(());
            }
            None
        };
        if let Some((_, inflight)) = self.history.inflight(series, epoch) {
            if inflight.is_none() && range.is_none() {
                // A study lease and its chart may request the same covering
                // snapshot in one turn. The active request already owns both.
                return Ok(());
            }
            let range = match (inflight, range) {
                (Some(inflight), Some(requested)) => {
                    let Some(missing) = history_range_after_inflight(requested, inflight) else {
                        return Ok(());
                    };
                    Some(missing)
                }
                _ => range,
            };
            self.history.defer_next(series, epoch, kind, range);
            return Ok(());
        }
        if !self.history_retry_ready(series, epoch, kind, range) {
            return Ok(());
        }
        let instrument = self.history_request_instrument(series);
        let ticket = self
            .history
            .begin(series.clone(), epoch, kind, range, self.clock.as_ref());
        let stop = self.history.cancel_token(&ticket);
        let maximum_bars = history_request_bar_limit(series, range);
        let request = HistoryRequest {
            ticket: ticket.clone(),
            series: series.clone(),
            provider_generation: generation,
            instrument,
            maximum_bars,
            range,
            stop: Arc::clone(&stop),
        };
        let dispatched = self.dispatch_history_request(request, &ticket, range)?;
        match dispatched {
            Ok(()) => {
                if range.is_none() && self.provider_uses_candles(&series.provider_id) {
                    self.transition_candle_series(
                        series,
                        super::lifecycle::SeriesInput::HistoryRequested(&ticket),
                    );
                }
                if range.is_none() {
                    diagnostic!(
                        "Aeris market covering history requested for {} generation={}",
                        series.instrument_id,
                        generation.0.get()
                    );
                }
                Ok(())
            }
            Err(error) => {
                self.history.defer(&ticket, range);
                self.history.schedule(
                    series.clone(),
                    epoch,
                    kind,
                    range,
                    self.clock.as_ref(),
                    true,
                );
                Err(error)
            }
        }
    }

    fn dispatch_history_request(
        &mut self,
        request: HistoryRequest,
        ticket: &super::history::tracker::HistoryTicket,
        range: Option<HistoryRange>,
    ) -> Result<Result<(), &'static str>, &'static str> {
        let provider = request.series.provider_id.clone();
        Ok(if self.provider_registry.contains(&provider) {
            match self.provider_registry.request_history(&provider, request) {
                Ok(true) => Ok(()),
                Ok(false) => Err("provider history queue is full"),
                Err(_) => Err("provider history worker is unavailable"),
            }
        } else {
            let history = match self.providers.history(&provider) {
                Ok(history) => history,
                Err(error) => {
                    self.history.defer(ticket, range);
                    return Err(error);
                }
            };
            try_enqueue_history(history, request)
        })
    }

    fn history_retry_ready(
        &mut self,
        series: &BarSeriesKey,
        epoch: aeris_market_engine::fence::Epoch<aeris_market_engine::fence::ProviderTag>,
        kind: LaneKind,
        range: Option<HistoryRange>,
    ) -> bool {
        if let Some((retry_at, failed_range, attempts)) = self.history.pending(series, epoch, kind)
        {
            if failed_range == range && self.clock.monotonic() < retry_at {
                return false;
            }
            if attempts > MAXIMUM_HISTORY_RETRIES || failed_range != range {
                // The failed cooldown expired, or the user moved to a genuinely
                // different range. Start that request with a fresh retry budget.
                self.history.clear_retry(series, epoch, kind);
            }
        }
        true
    }

    fn history_request_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Option<InstallProviderInstrument> {
        self.providers.live_model(&series.provider_id)?;
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .cloned()
    }

    fn provider_uses_candles(&self, provider_id: &str) -> bool {
        self.providers.live_model(provider_id) == Some(super::LiveModel::ProviderCandles)
    }
    fn backwards_history_exhausted(
        &self,
        series: &BarSeriesKey,
        epoch: aeris_market_engine::fence::Epoch<aeris_market_engine::fence::ProviderTag>,
        requested: HistoryRange,
    ) -> bool {
        self.history
            .coverage(series, epoch)
            .is_some_and(|coverage| coverage.backwards_exhausted)
            && self.engine.series_segments(series).is_some_and(|segments| {
                segments.first().is_some_and(|first| {
                    requested.end_unix_nanos <= first.exchange_timestamp_unix_nanos
                })
            })
    }
    fn uncovered_history_range(
        &self,
        series: &BarSeriesKey,
        epoch: aeris_market_engine::fence::Epoch<aeris_market_engine::fence::ProviderTag>,
        requested: HistoryRange,
    ) -> Option<HistoryRange> {
        if self.backwards_history_exhausted(series, epoch, requested) {
            return None;
        }
        self.history
            .coverage(series, epoch)
            .and_then(|coverage| coverage.confirmed_empty)
            .map_or(Some(requested), |confirmed| {
                history_range_after_confirmed_empty(requested, confirmed)
            })
    }
    pub(super) fn history_failed(
        &mut self,
        series: &BarSeriesKey,
        _generation: ProviderGeneration,
    ) {
        if self.engine.series_bar_count(series).is_some() {
            self.pending.remove(series);
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Partial,
                Some("Current provider history is unavailable; recovery will retry"),
            );
        } else if let Some(waiters) = self.pending.remove(series) {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::ProviderHistory,
                "Market historical bars are unavailable",
            );
        }
    }
    pub(super) fn accept_history_completion(
        &mut self,
        ticket: &HistoryTicket,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        result: Result<HistorySnapshot, String>,
    ) -> Option<HistorySnapshot> {
        let series = &ticket.series;
        let Some(cancelled) = self.history.finish(ticket, range, result.is_ok()) else {
            self.provider_epochs.drop_history();
            return None;
        };
        let current = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        if current != Some(generation) {
            self.prune_history_tracking();
            return None;
        }
        if cancelled {
            if self.engine.has_subscription(series) {
                self.request_series_history_recovery(series, generation);
            }
            return None;
        }
        match result {
            Ok(snapshot) => {
                if range.is_none() {
                    diagnostic!(
                        "Aeris market covering history completed for {} generation={} bars={}",
                        series.instrument_id,
                        generation.0.get(),
                        snapshot.bars.len()
                    );
                }
                Some(snapshot)
            }
            Err(error) => {
                let retry = self.schedule_history_retry(series, generation, range, &error);
                if retry && range.is_none() && self.provider_uses_candles(&series.provider_id) {
                    self.transition_candle_series(
                        series,
                        super::lifecycle::SeriesInput::HistoryFailed,
                    );
                }
                self.dispatch_deferred_history(ticket, generation);
                if !retry {
                    if range.is_none() {
                        if self.provider_uses_candles(&series.provider_id) {
                            self.transition_candle_series(
                                series,
                                super::lifecycle::SeriesInput::HistoryRetryExhausted,
                            );
                        }
                        let _ = self.cancel_live_history_reseed(series);
                        self.history_failed(series, generation);
                    } else {
                        self.broadcast_older_history_error_for(
                            series,
                            "Visible history backfill is temporarily unavailable",
                        );
                    }
                }
                None
            }
        }
    }
    pub(super) fn history_completed(
        &mut self,
        ticket: &HistoryTicket,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        result: Result<HistorySnapshot, String>,
    ) {
        let series = &ticket.series;
        let Some(snapshot) = self.accept_history_completion(ticket, generation, range, result)
        else {
            return;
        };
        if snapshot.bars.is_empty() {
            self.handle_empty_history(ticket, generation, range, &snapshot);
            return;
        }
        let repaired_timestamp_span = range.and_then(|_| {
            snapshot
                .bars
                .first()
                .zip(snapshot.bars.last())
                .map(|(first, last)| {
                    (
                        first.exchange_timestamp_unix_nanos,
                        last.exchange_timestamp_unix_nanos,
                    )
                })
        });
        let replacing_existing = self.engine.series_bar_count(series).is_some();
        let Some(prepared) = self.prepare_history_repair(series, generation, range, snapshot)
        else {
            if range.is_none() {
                let _ = self.cancel_live_history_reseed(series);
            }
            self.history_failed(series, generation);
            return;
        };
        let detached_from_live = prepared.detached_from_live;
        let reseed_live = range.is_none() && !detached_from_live;
        let install = if prepared.replace_window {
            CompletedHistoryInstall::Window { reseed_live }
        } else if replacing_existing {
            CompletedHistoryInstall::Covering { reseed_live }
        } else {
            CompletedHistoryInstall::Initial
        };
        if self
            .install_completed_history(ticket, generation, prepared.snapshot, install)
            .is_none()
        {
            if range.is_none() {
                let _ = self.cancel_live_history_reseed(series);
            }
            return;
        }
        if detached_from_live {
            self.history
                .set_detached(series, ticket.provider, true, self.clock.as_ref());
        } else if range.is_none() {
            self.history
                .set_detached(series, ticket.provider, false, self.clock.as_ref());
        }
        if range.is_some() && !detached_from_live {
            let Some(canonical) = self.engine.series_snapshot(series) else {
                self.history_failed(series, generation);
                return;
            };
            let realigned = if self.provider_uses_candles(&series.provider_id) {
                self.restore_candle_from_snapshot(series, &canonical)
            } else {
                self.restore_trade_from_snapshot(series, &canonical)
            };
            if realigned.is_err() {
                self.history_failed(series, generation);
                return;
            }
        }
        self.pending.remove(series);
        self.series_live_if_ready(series);
        if range.is_none() && replacing_existing {
            diagnostic!(
                "Aeris market series recovery finished for {} generation={}",
                series.instrument_id,
                generation.0.get()
            );
        }
        self.execute_studies_after_history_install(series, repaired_timestamp_span);
        if let Some(coverage) = self.history.coverage_mut(series, ticket.provider) {
            coverage.confirmed_empty = None;
            if range.is_none() {
                coverage.current_empty = None;
            }
        }
        // Re-check visible coverage after every successful non-empty install.
        // A provider may return a partial page; the canonical snapshot remains
        // the sole coverage fact and determines whether more older data is due.
        let viewport_demands = self.engine.retained_viewports(series);
        for (consumer_id, consumer_generation, viewport) in viewport_demands {
            let _ = self.request_viewport_history(consumer_id, consumer_generation, viewport);
        }
        self.dispatch_deferred_history(ticket, generation);
    }

    fn handle_empty_history(
        &mut self,
        ticket: &HistoryTicket,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        snapshot: &HistorySnapshot,
    ) {
        let series = &ticket.series;
        let epoch = ticket.provider;
        if let Some(range) = range {
            if let Some(coverage) = self.history.coverage_mut(series, epoch) {
                if snapshot.backwards_exhausted {
                    coverage.backwards_exhausted = true;
                }
                match &mut coverage.confirmed_empty {
                    Some(existing) => merge_confirmed_empty_range(existing, range),
                    none @ None => *none = Some(range),
                }
            }
            self.dispatch_deferred_history(ticket, generation);
            return;
        }
        let _ = self.cancel_live_history_reseed(series);
        if self.live_history_ready(series) {
            // A live edge is already installed; this was only a refill of a
            // detached window and the live handoff remains authoritative.
            self.history_failed(series, generation);
            self.dispatch_deferred_history(ticket, generation);
            return;
        }
        let recorded = match self.history.current_empty(series, epoch) {
            Some(EmptyCurrentHistory::LiveTradeRequested | EmptyCurrentHistory::Final) => {
                EmptyCurrentHistory::Final
            }
            Some(EmptyCurrentHistory::AwaitingLiveTrade { .. }) | None => {
                EmptyCurrentHistory::AwaitingLiveTrade {
                    boundary_unix_nanos: snapshot.handoff_boundary_unix_nanos.unwrap_or(i64::MIN),
                }
            }
        };
        diagnostic!(
            "Aeris market covering history for {} generation={} is empty; {}",
            series.instrument_id,
            generation.0.get(),
            if recorded == EmptyCurrentHistory::Final {
                "no further requests this session"
            } else {
                "waiting for a live trade"
            }
        );
        // Only the generation being answered can still be current; keeping one
        // record per series bounds this map across repeated reconnects.
        if let Some(coverage) = self.history.coverage_mut(series, epoch) {
            coverage.current_empty = Some(recorded);
        }
        self.pending.remove(series);
        if self.provider_uses_candles(&series.provider_id) {
            self.transition_candle_series(
                series,
                super::lifecycle::SeriesInput::HistoryEmpty(Some(ticket)),
            );
        }
        let detail = if self.engine.series_bar_count(series).is_some() {
            EMPTY_CURRENT_HISTORY_RETAINED_DETAIL
        } else {
            EMPTY_CURRENT_HISTORY_DETAIL
        };
        self.broadcast_series_resolution_for(series, SeriesLoadState::Empty, Some(detail));
        self.dispatch_deferred_history(ticket, generation);
    }

    pub(super) fn current_history_known_empty(
        &self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> bool {
        self.provider_epochs
            .current(&series.provider_id, generation.0.get())
            .and_then(|epoch| self.history.current_empty(series, epoch))
            .is_some_and(EmptyCurrentHistory::suppresses_current_requests)
    }

    /// Load state and detail for a retained snapshot whose covering repair is
    /// outstanding, or that the provider has already answered with no bars.
    pub(super) fn covering_repair_state(
        &self,
        series: &BarSeriesKey,
    ) -> (SeriesLoadState, &'static str) {
        let known_empty = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            .is_some_and(|generation| self.current_history_known_empty(series, generation));
        if known_empty {
            (
                SeriesLoadState::Empty,
                EMPTY_CURRENT_HISTORY_RETAINED_DETAIL,
            )
        } else {
            (SeriesLoadState::Partial, "Refreshing provider coverage")
        }
    }

    /// Issues the single covering request an empty current answer allows, once
    /// a live trade proves the provider now has data after that answer.
    pub(super) fn request_current_history_after_live_trade(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
        instrument_id: &str,
        entitlement_id: &str,
        exchange_unix_nanos: i64,
    ) {
        let Some(epoch) = self.provider_epochs.current(provider, generation.0.get()) else {
            return;
        };
        let due = self.history.current_empty_due(
            provider,
            epoch,
            instrument_id,
            entitlement_id,
            exchange_unix_nanos,
        );
        for series in due {
            if let Some(coverage) = self.history.coverage_mut(&series, epoch) {
                coverage.current_empty = Some(EmptyCurrentHistory::LiveTradeRequested);
            }
            diagnostic!(
                "Aeris market live trade arrived for {} generation={}; requesting covering history",
                series.instrument_id,
                generation.0.get()
            );
            self.request_series_history_recovery(&series, generation);
        }
    }

    fn live_history_ready(&self, series: &BarSeriesKey) -> bool {
        self.series
            .trade_lifecycle(series)
            .is_some_and(super::lifecycle::SeriesLifecycle::history_ready)
            || self
                .series
                .candle(series)
                .is_some_and(super::lifecycle::SeriesLifecycle::history_ready)
    }

    fn execute_studies_after_history_install(
        &mut self,
        series: &BarSeriesKey,
        repaired_timestamp_span: Option<(i64, i64)>,
    ) {
        let study_execution = match repaired_timestamp_span {
            Some((first_changed_unix_nanos, last_changed_unix_nanos)) => self.execute_study_change(
                StudyChange::History(series, first_changed_unix_nanos, last_changed_unix_nanos),
            ),
            None => self.execute_study_change(StudyChange::Market(series)),
        };
        match study_execution {
            Ok(batch) => {
                self.publish_study_outputs(&batch.executed);
                for error in batch.errors {
                    diagnostic!("Aeris study execution after history install failed: {error}");
                }
            }
            Err(error) => {
                diagnostic!("Aeris study execution after history install failed: {error}");
            }
        }
    }

    fn dispatch_deferred_history(
        &mut self,
        ticket: &HistoryTicket,
        generation: ProviderGeneration,
    ) {
        let series = &ticket.series;
        if self
            .provider_epochs
            .current(&series.provider_id, generation.0.get())
            != Some(ticket.provider)
        {
            return;
        }
        let Some(deferred) = self.history.take_deferred(series, ticket.provider) else {
            return;
        };
        let range = match deferred {
            DeferredRequest::Full => None,
            DeferredRequest::Range(range) => Some(range),
        };
        if let Err(error) =
            self.enqueue_history_request_with_capacity_retry(series, generation, range)
        {
            diagnostic!(
                "Aeris market deferred history enqueue failed for {}: {error}",
                series.instrument_id
            );
            self.schedule_history_retry(series, generation, range, error);
            if range.is_none() {
                self.broadcast_series_resolution_for(
                    series,
                    SeriesLoadState::Partial,
                    Some("Current market history is unavailable; recovery will retry"),
                );
            }
        }
    }
    fn install_completed_history(
        &mut self,
        ticket: &HistoryTicket,
        generation: ProviderGeneration,
        snapshot: HistorySnapshot,
        install: CompletedHistoryInstall,
    ) -> Option<(Vec<MarketBar>, Option<FormingBar>)> {
        let series = &ticket.series;
        let price_scale = snapshot.price_scale;
        let quantity_scale = snapshot.quantity_scale;
        let handoff_boundary_unix_nanos = snapshot.handoff_boundary_unix_nanos;
        let mut forming = snapshot.forming;
        let provider_forming = self
            .providers
            .live_model(&series.provider_id)
            .and_then(|_| forming.take());
        let bars = snapshot.bars;
        let installed = match install {
            CompletedHistoryInstall::Window { .. } => self.engine.replace_history_window(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
                true,
            ),
            CompletedHistoryInstall::Covering { .. } => self.engine.replace_covering_history(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
                true,
            ),
            CompletedHistoryInstall::Initial => self.engine.install_history(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
            ),
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
        self.publish_installed_history(&publications);
        if !install.reseeds_live() {
            return Some((bars, forming));
        }
        if self.provider_uses_candles(&series.provider_id) {
            if let Err(error) = self.install_candle_seed(
                series,
                generation,
                CandleHandoffSeed {
                    price_scale,
                    quantity_scale,
                    bars: &bars,
                    forming: provider_forming,
                },
                Some(ticket),
            ) {
                self.fail_history_handoff(series, &error);
                return None;
            }
        } else if let Err(error) = self.install_trade_seed(
            series,
            TradeHandoffSeed {
                price_scale,
                quantity_scale,
                bars: &bars,
                forming: provider_forming,
                handoff_boundary_unix_nanos,
            },
        ) {
            self.fail_history_handoff(series, &error);
            return None;
        }
        Some((bars, forming))
    }

    fn fail_history_handoff(&mut self, series: &BarSeriesKey, error: &str) {
        if let Some(waiters) = self.pending.remove(series) {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::Handoff,
                error,
            );
        }
    }

    /// Publishes one installed covering snapshot to every matching consumer.
    pub(super) fn publish_installed_history(
        &mut self,
        publications: &[aeris_market_engine::ConsumerPublication],
    ) {
        for publication in publications {
            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                publish_state(events, publication, SeriesLoadState::Ready, None);
            }
        }
    }

    /// Keeps a live canonical series inside its per-series high watermark.
    ///
    /// The provider-owned handoff remains authoritative for current live state.
    /// If viewport-focused compaction has to discard the live edge, the canonical
    /// series is explicitly detached and later live publications are drained but
    /// not installed until current history rejoins it.
    pub(super) fn compact_live_series_if_needed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<bool, String> {
        let epoch = self
            .provider_epochs
            .current(&series.provider_id, generation.0.get());
        if epoch.is_some_and(|epoch| self.history.is_detached(series, epoch)) {
            return Ok(true);
        }
        if self.engine.series_bar_count(series).unwrap_or(0) < HISTORY_SERIES_HIGH_WATERMARK {
            return Ok(false);
        }
        let snapshot = self
            .engine
            .series_snapshot(series)
            .ok_or_else(|| "canonical series disappeared before compaction".to_string())?;
        let viewport = self
            .engine
            .primary_retained_viewport(series)
            .map(|(_, _, viewport)| viewport);
        let compacted = compact_history_window(snapshot.bars.to_vec(), viewport, false);
        let publications = if compacted.dropped_newer {
            self.engine.replace_history_window(
                generation,
                series,
                snapshot.price_scale,
                snapshot.quantity_scale,
                compacted.bars,
                true,
            )
        } else {
            self.engine.replace_covering_history(
                generation,
                series,
                snapshot.price_scale,
                snapshot.quantity_scale,
                compacted.bars,
                true,
            )
        }
        .map_err(|error| error.to_string())?;
        self.publish_installed_history(&publications);
        if compacted.dropped_newer {
            if let Some(epoch) = epoch {
                self.history
                    .set_detached(series, epoch, true, self.clock.as_ref());
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn prepare_history_repair(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        mut snapshot: HistorySnapshot,
    ) -> Option<PreparedHistoryRepair> {
        let was_detached = self
            .provider_epochs
            .current(&series.provider_id, generation.0.get())
            .is_some_and(|epoch| self.history.is_detached(series, epoch));
        let viewport = self
            .engine
            .primary_retained_viewport(series)
            .map(|(_, _, viewport)| viewport);
        let current = self.engine.series_snapshot(series);
        let rejoining_current = range.is_none() && was_detached;
        let disjoint_newer = current.as_ref().is_some_and(|current| {
            range.is_some_and(|requested| {
                requested.start_unix_nanos
                    >= current
                        .bars
                        .last()
                        .map_or(i64::MAX, |bar| bar.exchange_timestamp_unix_nanos)
                    && snapshot.bars.first().is_some_and(|bar| {
                        current.bars.last().is_some_and(|last| {
                            bar.exchange_timestamp_unix_nanos > last.exchange_timestamp_unix_nanos
                        })
                    })
            })
        });
        let bars = if rejoining_current || disjoint_newer {
            snapshot.bars
        } else if let Some(current) = current.as_ref() {
            let Ok(merged) = reconcile_history_repair(current, snapshot.bars) else {
                self.history_failed(series, generation);
                return None;
            };
            merged
        } else {
            snapshot.bars
        };
        let reserve_forming_tail = current.as_ref().is_some_and(|current| current.forming)
            && !was_detached
            && !rejoining_current
            && !disjoint_newer;
        let compacted = compact_history_window(bars, viewport, reserve_forming_tail);
        let detached_from_live = if rejoining_current {
            false
        } else {
            was_detached || disjoint_newer || compacted.dropped_newer
        };
        snapshot.bars = compacted.bars;
        if let Some(forming) = snapshot.forming.as_mut() {
            let next_sequence = snapshot
                .bars
                .last()
                .map_or(1, |bar| bar.source_sequence.saturating_add(1));
            forming.bar.source_sequence = next_sequence;
        }
        Some(PreparedHistoryRepair {
            snapshot,
            replace_window: detached_from_live || rejoining_current,
            detached_from_live,
        })
    }

    pub(super) fn prune_history_tracking(&mut self) {
        // Timeframe/symbol changes supersede the old series immediately. Cancel
        // its queued/in-flight provider work so one user rapidly changing the
        // chart cannot occupy the bounded history queue with obsolete fetches.
        let demanded = self
            .engine
            .demanded_series()
            .into_iter()
            .filter_map(|series| {
                let generation = self
                    .engine
                    .provider_status(&series.provider_id)?
                    .generation?;
                let epoch = self
                    .provider_epochs
                    .current(&series.provider_id, generation.0.get())?;
                Some((series, epoch))
            })
            .collect::<BTreeSet<_>>();
        self.history.retain_demanded(&demanded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_data::BarPeriod;
    use std::num::NonZeroU64;
    use std::sync::{Arc as StdArc, Mutex as StdMutex, mpsc};
    use std::time::Duration;

    fn test_series(instrument_id: &str) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: "fixture-entitlement".to_string(),
            period: BarPeriod::time(60).expect("fixture period"),
            definition_version: 1,
        }
    }

    fn test_history_request(instrument_id: &str) -> HistoryRequest {
        let series = test_series(instrument_id);
        HistoryRequest {
            ticket: super::super::fixture_history_ticket(&series),
            series,
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            instrument: None,
            maximum_bars: 10,
            range: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn test_bar(sequence: u64, minute: i64) -> MarketBar {
        let timestamp = minute * 60_i64 * 1_000_000_000;
        MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: minute * 60,
            exchange_timestamp_unix_nanos: timestamp,
            open: 100,
            high: 101,
            low: 99,
            close: 100,
            volume: 1,
        }
    }

    #[test]
    fn historical_backfill_is_retained_beyond_one_provider_page_limit() {
        let existing = 8_500usize;
        let older = 1_000usize;
        let current = SeriesSnapshot {
            series: test_series("deep-history"),
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            publication_generation: 1,
            price_scale: 2,
            quantity_scale: 0,
            forming: false,
            bars: (0..existing)
                .map(|index| {
                    test_bar(
                        u64::try_from(index + 1).expect("bounded sequence"),
                        i64::try_from(index + older).expect("bounded minute"),
                    )
                })
                .collect::<Vec<_>>()
                .into(),
        };
        let repair = (0..older)
            .map(|index| {
                test_bar(
                    u64::try_from(index + 1).expect("bounded sequence"),
                    i64::try_from(index).expect("bounded minute"),
                )
            })
            .collect();

        let merged = reconcile_history_repair(&current, repair).expect("history merges");
        assert_eq!(merged.len(), existing + older);
        assert_eq!(
            merged.first().map(|bar| bar.exchange_timestamp_seconds),
            Some(0)
        );
        assert_eq!(
            merged.last().map(|bar| bar.exchange_timestamp_seconds),
            Some(i64::try_from(existing + older - 1).expect("bounded minute") * 60)
        );
    }

    #[test]
    fn history_compaction_is_bounded_and_preserves_visible_prefetch_focus() {
        let total = HISTORY_SERIES_HIGH_WATERMARK + 1_000;
        let bars = (0..total)
            .map(|index| {
                test_bar(
                    u64::try_from(index + 1).expect("bounded sequence"),
                    i64::try_from(index).expect("bounded minute"),
                )
            })
            .collect::<Vec<_>>();
        let minute = 60_i64 * 1_000_000_000;
        let viewport = Viewport::try_new(2_000 * minute, 2_200 * minute).expect("viewport");

        let compacted = compact_history_window(bars, Some(viewport), false);

        assert_eq!(compacted.bars.len(), HISTORY_SERIES_TARGET_BARS);
        assert!(compacted.dropped_newer);
        let first = compacted
            .bars
            .first()
            .expect("bounded window has a first bar");
        let last = compacted
            .bars
            .last()
            .expect("bounded window has a last bar");
        assert!(first.exchange_timestamp_unix_nanos <= 1_800 * minute);
        assert!(last.exchange_timestamp_unix_nanos >= viewport.end_unix_nanos);
        assert!(compacted.bars.windows(2).all(|pair| {
            pair[0].source_sequence.checked_add(1) == Some(pair[1].source_sequence)
                && pair[0].exchange_timestamp_unix_nanos < pair[1].exchange_timestamp_unix_nanos
        }));
    }

    #[test]
    fn detached_viewport_refill_continues_left_moves_right_and_rejoins_live() {
        let minute = 60_i64 * 1_000_000_000;
        let series = test_series("movable-window");
        let snapshot = aeris_market_engine::SeriesSnapshot {
            series: series.clone(),
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            publication_generation: 1,
            price_scale: 2,
            quantity_scale: 0,
            forming: false,
            bars: (1_000..1_100)
                .enumerate()
                .map(|(index, minute_index)| {
                    test_bar(
                        u64::try_from(index + 1).expect("bounded sequence"),
                        minute_index,
                    )
                })
                .collect::<Vec<_>>()
                .into(),
        };

        let older = Viewport::try_new(900 * minute, 980 * minute).expect("older viewport");
        assert!(matches!(
            viewport_refill_plan(&series, &snapshot, older, true, Some((10_000 * minute, 10_500 * minute))),
            Some(ViewportRefillPlan::Range(range))
                if range.end_unix_nanos == 1_000 * minute
                    && range.start_unix_nanos < older.start_unix_nanos
        ));

        let newer = Viewport::try_new(1_200 * minute, 1_300 * minute).expect("newer viewport");
        assert!(matches!(
            viewport_refill_plan(&series, &snapshot, newer, true, Some((10_000 * minute, 10_500 * minute))),
            Some(ViewportRefillPlan::Range(range))
                if range.start_unix_nanos == 1_099 * minute
                    && range.end_unix_nanos > newer.end_unix_nanos
        ));

        let recent = Viewport::try_new(10_100 * minute, 10_200 * minute).expect("recent viewport");
        assert_eq!(
            viewport_refill_plan(
                &series,
                &snapshot,
                recent,
                true,
                Some((10_000 * minute, 10_500 * minute)),
            ),
            Some(ViewportRefillPlan::Current)
        );
    }

    struct RecordingHistorySource {
        calls: StdArc<StdMutex<Vec<String>>>,
    }

    impl HistorySource for RecordingHistorySource {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.series.instrument_id.clone());
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: Vec::new(),
                forming: None,
                handoff_boundary_unix_nanos: None,
                backwards_exhausted: false,
            })
        }
    }

    #[test]
    fn history_worker_prioritizes_current_work_and_skips_cancelled_fetches() {
        let calls = StdArc::new(StdMutex::new(Vec::new()));
        let (requests, receiver) = mpsc::sync_channel(3);
        let (completions, _results) = mpsc::sync_channel(3);
        let mut backfill = test_history_request("backfill");
        backfill.range = Some(HistoryRange {
            start_unix_nanos: 1,
            end_unix_nanos: 2,
        });
        requests.send(backfill).unwrap();
        let cancelled = test_history_request("cancelled");
        cancelled.stop.store(true, Ordering::Release);
        requests.send(cancelled).unwrap();
        requests.send(test_history_request("current")).unwrap();
        drop(requests);
        run_history_worker(
            Box::new(RecordingHistorySource {
                calls: StdArc::clone(&calls),
            }),
            &receiver,
            &completions,
            &AtomicBool::new(false),
        );
        assert_eq!(*calls.lock().unwrap(), ["current", "backfill"]);
    }

    #[test]
    fn history_worker_processes_provider_requests_serially_in_queue_order() {
        let calls = StdArc::new(StdMutex::new(Vec::new()));
        let (requests_tx, requests_rx) = mpsc::sync_channel(2);
        let (completions_tx, completions_rx) = mpsc::sync_channel(2);
        requests_tx
            .send(test_history_request("first"))
            .expect("first request queues");
        requests_tx
            .send(test_history_request("second"))
            .expect("second request queues");
        drop(requests_tx);

        run_history_worker(
            Box::new(RecordingHistorySource {
                calls: StdArc::clone(&calls),
            }),
            &requests_rx,
            &completions_tx,
            &AtomicBool::new(false),
        );

        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["first".to_string(), "second".to_string()]
        );
        assert!(matches!(
            completions_rx.try_recv(),
            Ok(Command::HistoryCompleted(series, _, None, Ok(_), _)) if series.instrument_id == "first"
        ));
        assert!(matches!(
            completions_rx.try_recv(),
            Ok(Command::HistoryCompleted(series, _, None, Ok(_), _)) if series.instrument_id == "second"
        ));
    }

    #[test]
    fn history_worker_blocks_on_full_completion_channel() {
        let calls = StdArc::new(StdMutex::new(Vec::new()));
        let (requests_tx, requests_rx) = mpsc::sync_channel(2);
        let (completions_tx, completions_rx) = mpsc::sync_channel(1);
        completions_tx.send(Command::ProviderWake).unwrap();
        requests_tx.send(test_history_request("first")).unwrap();
        requests_tx.send(test_history_request("second")).unwrap();
        drop(requests_tx);
        let observed = StdArc::clone(&calls);
        let worker = thread::spawn(move || {
            run_history_worker(
                Box::new(RecordingHistorySource { calls: observed }),
                &requests_rx,
                &completions_tx,
                &AtomicBool::new(false),
            );
        });
        assert!(matches!(
            completions_rx.recv_timeout(Duration::from_secs(1)),
            Ok(Command::ProviderWake)
        ));
        assert!(matches!(
            completions_rx.recv_timeout(Duration::from_secs(1)),
            Ok(Command::HistoryCompleted(series, _, _, _, _)) if series.instrument_id == "first"
        ));
        assert!(matches!(
            completions_rx.recv_timeout(Duration::from_secs(1)),
            Ok(Command::HistoryCompleted(series, _, _, _, _)) if series.instrument_id == "second"
        ));
        worker.join().expect("worker exits after draining");
        assert_eq!(*calls.lock().unwrap(), ["first", "second"]);
    }

    #[test]
    fn history_worker_serves_oldest_backfill_in_every_fourth_slot() {
        let calls = StdArc::new(StdMutex::new(Vec::new()));
        let (requests_tx, requests_rx) = mpsc::sync_channel(super::super::HISTORY_CAPACITY);
        let (completions_tx, _completions_rx) = mpsc::sync_channel(super::super::HISTORY_CAPACITY);
        for name in ["current-a", "current-b", "current-c", "current-d"] {
            requests_tx.send(test_history_request(name)).unwrap();
        }
        for name in ["backfill-old", "backfill-new"] {
            let mut request = test_history_request(name);
            request.range = Some(HistoryRange {
                start_unix_nanos: 1,
                end_unix_nanos: 2,
            });
            requests_tx.send(request).unwrap();
        }
        drop(requests_tx);
        run_history_worker(
            Box::new(RecordingHistorySource {
                calls: StdArc::clone(&calls),
            }),
            &requests_rx,
            &completions_tx,
            &AtomicBool::new(false),
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "current-a",
                "current-b",
                "current-c",
                "backfill-old",
                "current-d",
                "backfill-new"
            ]
        );
    }

    #[test]
    fn deferred_history_coalesces_ranges_and_full_request_supersedes_them() {
        let mut deferred = DeferredRequest::Range(HistoryRange {
            start_unix_nanos: 100,
            end_unix_nanos: 200,
        });
        merge_deferred_request(
            &mut deferred,
            Some(HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 150,
            }),
        );
        assert_eq!(
            deferred,
            DeferredRequest::Range(HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 200,
            })
        );

        merge_deferred_request(&mut deferred, None);
        assert_eq!(deferred, DeferredRequest::Full);
        merge_deferred_request(
            &mut deferred,
            Some(HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 300,
            }),
        );
        assert_eq!(deferred, DeferredRequest::Full);
    }

    #[test]
    fn viewport_history_prefetches_one_visible_span_before_the_left_edge() {
        let minute = 60_i64 * 1_000_000_000;
        let series = test_series("prefetch");
        let snapshot = aeris_market_engine::SeriesSnapshot {
            series: series.clone(),
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            publication_generation: 1,
            price_scale: 2,
            quantity_scale: 0,
            forming: false,
            bars: vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 520 * 60,
                exchange_timestamp_unix_nanos: 520 * minute,
                open: 100,
                high: 100,
                low: 100,
                close: 100,
                volume: 1,
            }]
            .into(),
        };
        let viewport = Viewport::try_new(500 * minute, 550 * minute).expect("viewport");

        assert_eq!(
            viewport_history_range(&series, &snapshot, viewport),
            Some(HistoryRange {
                start_unix_nanos: 450 * minute,
                end_unix_nanos: 520 * minute,
            })
        );

        let partial = aeris_market_engine::SeriesSnapshot {
            bars: vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 510 * 60,
                exchange_timestamp_unix_nanos: 510 * minute,
                open: 100,
                high: 100,
                low: 100,
                close: 100,
                volume: 1,
            }]
            .into(),
            ..snapshot
        };
        assert_eq!(
            viewport_history_range(&series, &partial, viewport),
            Some(HistoryRange {
                start_unix_nanos: 450 * minute,
                end_unix_nanos: 510 * minute,
            })
        );
    }

    #[test]
    fn inflight_viewport_coverage_suppresses_duplicates_and_trims_older_extension() {
        let inflight = HistoryRange {
            start_unix_nanos: 100,
            end_unix_nanos: 200,
        };
        assert_eq!(
            history_range_after_inflight(
                HistoryRange {
                    start_unix_nanos: 120,
                    end_unix_nanos: 180,
                },
                inflight,
            ),
            None
        );
        assert_eq!(
            history_range_after_inflight(
                HistoryRange {
                    start_unix_nanos: 50,
                    end_unix_nanos: 200,
                },
                inflight,
            ),
            Some(HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 100,
            })
        );
        let disjoint = HistoryRange {
            start_unix_nanos: 10,
            end_unix_nanos: 90,
        };
        assert_eq!(
            history_range_after_inflight(disjoint, inflight),
            Some(disjoint)
        );
    }

    #[test]
    fn confirmed_empty_history_suppresses_repeats_and_only_fetches_older_extension() {
        let empty = HistoryRange {
            start_unix_nanos: 100,
            end_unix_nanos: 200,
        };
        assert_eq!(history_range_after_confirmed_empty(empty, empty), None);
        assert_eq!(
            history_range_after_confirmed_empty(
                HistoryRange {
                    start_unix_nanos: 50,
                    end_unix_nanos: 200,
                },
                empty,
            ),
            Some(HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 100,
            })
        );

        let mut accumulated = empty;
        merge_confirmed_empty_range(
            &mut accumulated,
            HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 100,
            },
        );
        assert_eq!(
            accumulated,
            HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 200,
            }
        );
        assert_eq!(
            history_range_after_confirmed_empty(
                HistoryRange {
                    start_unix_nanos: 0,
                    end_unix_nanos: 200,
                },
                accumulated,
            ),
            Some(HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 50,
            })
        );
        assert_eq!(
            history_range_after_confirmed_empty(
                HistoryRange {
                    start_unix_nanos: 50,
                    end_unix_nanos: 250,
                },
                accumulated,
            ),
            Some(HistoryRange {
                start_unix_nanos: 200,
                end_unix_nanos: 250,
            })
        );
    }
}
