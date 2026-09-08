use super::{
    ActiveWorkerGuard, Arc, AtomicBool, BTreeSet, BarPeriod, BarSeriesKey, Command, ConsumerId,
    Coordinator, DeferredHistoryRequest, DemandWaiter, EMPTY_REPAIR_RETRY_DELAY, FailureStage,
    FormingBar, GenerationId, HISTORY_BARS_PER_SERIES, HISTORY_CAPACITY_EXHAUSTED,
    HISTORY_RETRY_DELAY, HistoryRange, HistoryRequest, HistorySnapshot, HistorySource, HotSeries,
    HotSetManager, HotSetTier, HyperliquidHandoffSeed, InstallProviderInstrument, Instant,
    MAXIMUM_HISTORY_RETRIES, MAXIMUM_SERIES, MarketBar, Mutex, NonZeroU64, NonZeroUsize, Ordering,
    PersistenceState, ProviderCatalogCommand, ProviderGeneration, ProviderRequest,
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, Receiver, ResourceMode, RithmicHandoffSeed,
    SearchProviderInstruments, SeriesCadence, SeriesKey, SeriesLoadState, SeriesSnapshot,
    StorageRequest, SyncSender, TrySendError, VIEWPORT_LIVE_TAIL_RESERVE, Viewport, WarmSeries,
    WorkspaceId, WorkspaceState, engine_install_failure_stage, fail_waiters,
    hyperliquid_interval_for_period, ipc_series, publish_state, series_state, thread,
    try_enqueue_history,
};
use crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ACCOUNT_ID;

pub(super) fn reconcile_history_repair(
    current: &SeriesSnapshot,
    repair: Vec<MarketBar>,
    maximum_bars: usize,
) -> Result<Vec<MarketBar>, String> {
    if maximum_bars == 0 || current.bars.is_empty() || repair.is_empty() {
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
    let skip = by_timestamp.len().saturating_sub(maximum_bars);
    let mut retained = by_timestamp.into_values().skip(skip).collect::<Vec<_>>();
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

fn merge_deferred_history_request(
    deferred: &mut DeferredHistoryRequest,
    range: Option<HistoryRange>,
) {
    match (deferred, range) {
        (DeferredHistoryRequest::Full, _) => {}
        (slot, None) => *slot = DeferredHistoryRequest::Full,
        (DeferredHistoryRequest::Range(existing), Some(range)) => {
            existing.start_unix_nanos = existing.start_unix_nanos.min(range.start_unix_nanos);
            existing.end_unix_nanos = existing.end_unix_nanos.max(range.end_unix_nanos);
        }
    }
}

fn deferred_history_request(range: Option<HistoryRange>) -> DeferredHistoryRequest {
    range.map_or(DeferredHistoryRequest::Full, DeferredHistoryRequest::Range)
}

pub(super) fn merge_history_ranges(left: HistoryRange, right: HistoryRange) -> HistoryRange {
    HistoryRange {
        start_unix_nanos: left.start_unix_nanos.min(right.start_unix_nanos),
        end_unix_nanos: left.end_unix_nanos.max(right.end_unix_nanos),
    }
}

fn viewport_history_range(
    series: &BarSeriesKey,
    snapshot: &axiusflow_market_engine::SeriesSnapshot,
    viewport: Viewport,
) -> Option<HistoryRange> {
    let first = snapshot.bars.first()?;
    if viewport.start_unix_nanos >= first.exchange_timestamp_unix_nanos {
        return None;
    }
    let mut start_unix_nanos = viewport.start_unix_nanos;
    let end_unix_nanos = first.exchange_timestamp_unix_nanos;
    if let Some(duration) = series.period.duration_nanos() {
        let maximum = HISTORY_BARS_PER_SERIES
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
    if series.provider != "rithmic" && series.provider != "hyperliquid"
        || series.provider_symbol.trim().is_empty()
        || series.venue_id.trim().is_empty()
        || series.display_symbol.trim().is_empty()
        || series.price_scale > 18
        || series.quantity_scale > 18
    {
        return Err("hot-set instrument metadata is invalid".to_string());
    }
    if series.provider == "rithmic" && series.account_id != RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID {
        return Err("hot-set instrument metadata is invalid".to_string());
    }
    if series.provider == "hyperliquid" && series.account_id != HYPERLIQUID_PUBLIC_ACCOUNT_ID {
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
                    .or_insert((Instant::now() + HISTORY_RETRY_DELAY, 0, None));
                Ok(())
            }
            result => result,
        }
    }
    pub(super) fn restore_hot_series(&mut self, series: Vec<WarmSeries>) {
        self.hot_set_priority_count = series.len().min(MAXIMUM_SERIES);
        for warm in series
            .into_iter()
            .filter(|warm| {
                warm.series.provider_id == "rithmic" || warm.series.provider_id == "hyperliquid"
            })
            .take(MAXIMUM_SERIES)
        {
            if !self.warm_series.contains_key(&warm.series) {
                self.warm_priority.push(warm.series.clone());
            }
            self.warm_series.insert(warm.series.clone(), warm.clone());
        }
        self.schedule_warm_local_history();
    }

    pub(super) fn schedule_warm_local_history(&mut self) {
        if self.resource_mode == ResourceMode::OfflineSuspended {
            return;
        }
        let candidates = self
            .warm_priority
            .iter()
            .take(self.resource_policy.maximum_cached_series)
            .filter_map(|series| self.warm_series.get(series))
            .cloned()
            .collect::<Vec<_>>();
        for warm in candidates {
            let generation = ProviderGeneration(
                NonZeroU64::new(warm.provider_watermark.max(1)).unwrap_or(NonZeroU64::MIN),
            );
            let key = (warm.series.clone(), generation);
            if self.warm_reads.contains(&key)
                || self.retained_history.contains_key(&warm.series)
                || self.prewarmed.contains(&warm.series)
            {
                continue;
            }
            match self
                .storage
                .try_send(StorageRequest::Read(warm.series.clone(), generation))
            {
                Ok(()) => {
                    self.warm_reads.insert(key);
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => break,
            }
        }
    }
    pub(super) fn activate_markets_live_hot_set(&mut self) {
        self.retained_live.extend(self.rithmic_live.keys().cloned());
        self.retained_live
            .extend(self.hyperliquid_live.keys().cloned());
        self.activate_markets_live_provider_hot_set("rithmic");
        self.activate_markets_live_provider_hot_set("hyperliquid");
    }

    pub(super) fn activate_markets_live_provider_hot_set(&mut self, provider: &str) {
        if self.resource_mode != ResourceMode::MarketsLive
            || self.warm_restore_pending.contains_key(provider)
            || self.retained_live.len() >= self.resource_policy.maximum_derived_series
        {
            return;
        }
        let Some(series) = self
            .warm_priority
            .iter()
            .find(|series| {
                series.provider_id == provider
                    && !self.retained_live.contains(*series)
                    && !self.warm_restore_skipped.contains(*series)
            })
            .cloned()
        else {
            return;
        };
        let Some(warm) = self.warm_series.get(&series) else {
            return;
        };
        let provider_symbol = warm.instrument.provider_symbol.clone();
        let generation = self
            .warm_search_generations
            .get(provider)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .unwrap_or(1);
        match self.providers.dispatch_catalog(
            provider,
            ProviderCatalogCommand::Search(SearchProviderInstruments {
                consumer_id: 0,
                search_generation: generation,
                provider: provider.to_string(),
                query: provider_symbol,
                maximum_results: 16,
            }),
        ) {
            Ok(()) => {
                self.warm_search_generations
                    .insert(provider.to_string(), generation);
                self.warm_restore_pending
                    .insert(provider.to_string(), series);
            }
            Err(error) if error.ends_with("catalog command capacity is exhausted") => {
                // Capacity pressure is transient. Leave this series eligible so
                // the coordinator-tick retry below can dispatch it as soon as
                // the bounded catalog lane drains.
            }
            Err(_) => {
                // A disconnected/unavailable catalog worker cannot make forward
                // progress by being hammered every tick. Skip this retained item
                // until a later MarketsLive activation rebuilds restore state.
                self.warm_restore_skipped.insert(series);
            }
        }
    }
    pub(super) fn prepare_cached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        snapshot: &Arc<axiusflow_market_engine::SeriesSnapshot>,
    ) -> Result<bool, String> {
        let needs_covering_repair =
            snapshot.provider_generation != provider_generation || self.prewarmed.remove(series);
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
        } else if series.provider_id == "hyperliquid" {
            self.start_hyperliquid_realtime_from_snapshot(series, snapshot)?;
        } else {
            self.start_rithmic_realtime_from_snapshot(series, snapshot)?;
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
        let history = if self
            .enqueue_local_history(series, provider_generation)
            .is_ok()
        {
            Ok(())
        } else {
            self.enqueue_history_recovery(series, provider_generation)
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
        range: Option<HistoryRange>,
        error: &str,
    ) -> bool {
        let key = (series.clone(), generation);
        let attempts = self
            .history_retries
            .get(&key)
            .map_or(1, |(_, attempts, _)| attempts.saturating_add(1));
        eprintln!(
            "Axiusflow engine {} history attempt {attempts} failed for {}: {error}",
            series.provider_id, series.instrument_id
        );
        if attempts > MAXIMUM_HISTORY_RETRIES {
            self.history_retries.remove(&key);
            return false;
        }
        self.history_retries
            .insert(key, (Instant::now() + HISTORY_RETRY_DELAY, attempts, range));
        self.broadcast_series_recovery_for(
            series,
            if series.provider_id == "hyperliquid" {
                "Hyperliquid current history is retrying"
            } else {
                "Rithmic current history is retrying"
            },
        );
        true
    }

    pub(super) fn retry_history(&mut self) {
        if self.resource_mode == ResourceMode::MarketsLive {
            self.activate_markets_live_provider_hot_set("rithmic");
            self.activate_markets_live_provider_hot_set("hyperliquid");
        }
        let now = Instant::now();
        let Some(key) = self
            .history_retries
            .iter()
            .find(|(key, (retry_at, _, _))| {
                now >= *retry_at && !self.history_inflight.contains_key(*key)
            })
            .map(|(key, (_, _, range))| (key.clone(), *range))
        else {
            return;
        };
        let (key, range) = key;
        let (series, generation) = &key;
        let current_generation = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        if !self.engine.has_subscription(series) || current_generation != Some(*generation) {
            self.history_retries.remove(&key);
            return;
        }
        let _ = self.enqueue_history_request(series, *generation, range);
        if let Some((retry_at, _, _)) = self.history_retries.get_mut(&key) {
            *retry_at = now + HISTORY_RETRY_DELAY;
        }
    }

    pub(super) fn protected_history_ranges(&self) -> Vec<(BarSeriesKey, HistoryRange)> {
        let mut protected = BTreeSet::new();
        for series in self.rithmic_live.keys().chain(self.hyperliquid_live.keys()) {
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
        if series.provider_id == "hyperliquid" {
            hyperliquid_interval_for_period(series.period)
                .map_err(|_| "Hyperliquid history is unavailable for this interval".to_string())?;
            if series.definition_version != 1 {
                return Err("unsupported Hyperliquid engine series definition".to_string());
            }
            let installed = self.hyperliquid_instrument(series)?;
            if installed.entitlement_id != series.entitlement_id {
                return Err("Hyperliquid series entitlement is inconsistent".to_string());
            }
            return self
                .engine
                .provider_status("hyperliquid")
                .and_then(|status| status.generation)
                .ok_or_else(|| "Hyperliquid engine session is unavailable".to_string());
        }
        if series.provider_id != "rithmic" {
            return Err("resident engine market provider is unsupported".to_string());
        }
        crate::rithmic_history::chart_interval(series.period)?;
        if series.definition_version != 1 {
            return Err("unsupported Rithmic engine series definition".to_string());
        }
        let installed = self.rithmic_instrument(series)?;
        if installed.entitlement_id != series.entitlement_id {
            return Err("Rithmic series entitlement is inconsistent".to_string());
        }
        self.engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            .ok_or_else(|| "Rithmic engine session is unavailable".to_string())
    }
    pub(super) fn series_precision(&self, series: &BarSeriesKey) -> Result<(u8, u8), String> {
        if series.provider_id == "hyperliquid" {
            let installed = self.hyperliquid_instrument(series)?;
            if installed.entitlement_id != series.entitlement_id {
                return Err("Hyperliquid series entitlement is inconsistent".to_string());
            }
            return Ok((
                u8::try_from(installed.price_scale)
                    .map_err(|_| "Hyperliquid price scale is invalid".to_string())?,
                u8::try_from(installed.quantity_scale)
                    .map_err(|_| "Hyperliquid quantity scale is invalid".to_string())?,
            ));
        }
        if series.provider_id != "rithmic" {
            return Err("resident engine market provider is unsupported".to_string());
        }
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

    pub(super) fn rithmic_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .ok_or_else(|| "Rithmic instrument is not installed".to_string())
    }

    pub(super) fn hyperliquid_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .ok_or_else(|| "Hyperliquid instrument is not installed".to_string())
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
        let Some(snapshot) = self.engine.series_snapshot(&series) else {
            // Initial history already owns the unresolved series. Once the
            // first covering snapshot lands, later viewport movement can ask
            // for older coverage without duplicating startup work.
            return Ok(());
        };
        let Some(range) = viewport_history_range(&series, &snapshot, viewport) else {
            return Ok(());
        };
        let provider_generation = self.provider_generation_for_series(&series)?;
        let key = (series.clone(), provider_generation);
        if let Some(pending) = self.local_history_deadlines.get(&key) {
            if pending.range == Some(range) {
                return Ok(());
            }
            // The current viewport already lives in the demand registry. Keep
            // one local read in flight; its completion re-derives the newest
            // aggregate viewport before any provider fallback, so rapid pans do
            // not bypass available cache or require a second scheduling queue.
            return Ok(());
        }
        match self.enqueue_local_history_range(&series, provider_generation, range) {
            Ok(()) => Ok(()),
            Err(_) => self
                .enqueue_history_request(&series, provider_generation, Some(range))
                .map_err(str::to_string),
        }
    }

    pub(super) fn current_viewport_history_range(
        &self,
        series: &BarSeriesKey,
    ) -> Option<HistoryRange> {
        let snapshot = self.engine.series_snapshot(series)?;
        self.engine
            .retained_viewports(series)
            .into_iter()
            .map(|(_, _, viewport)| viewport)
            .filter_map(|viewport| viewport_history_range(series, &snapshot, viewport))
            .reduce(merge_history_ranges)
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
    /// Realigns an already-live aggregator with the series a repair just
    /// installed, without discarding the bucket currently forming.
    ///
    /// A backfill extends history backwards and can keep or drop the forming
    /// tail, so what the series holds as *completed* moves. Bar identity comes
    /// from the bucket, so nothing is renumbered — only the append cursor has to
    /// follow. If the aggregator turns out not to line up after all, the next
    /// publication fails for this one series and reseeds it.
    pub(super) fn enqueue_history_request(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
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
            self.history_deferred
                .entry(key)
                .and_modify(|deferred| merge_deferred_history_request(deferred, range))
                .or_insert_with(|| deferred_history_request(range));
            return Ok(());
        }
        let instrument = if series.provider_id == "rithmic" || series.provider_id == "hyperliquid" {
            self.catalog
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .cloned()
        } else {
            None
        };
        let stop = Arc::new(AtomicBool::new(false));
        let maximum_bars = if series.provider_id == "hyperliquid" {
            // Hyperliquid exposes only its latest 5,000 candles. Fetch that
            // complete public window once; local history then extends it.
            axiusflow_hyperliquid_market_adapter::MAXIMUM_HYPERLIQUID_CANDLES
        } else {
            self.resource_policy.history_prefetch_bars.max(1)
        };
        let request = HistoryRequest {
            series: series.clone(),
            provider_generation: generation,
            instrument,
            maximum_bars,
            range,
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
            if let Some(snapshot) = self.engine.series_snapshot(series) {
                if series.provider_id == "hyperliquid" {
                    let _ = self.start_hyperliquid_realtime_from_snapshot(series, &snapshot);
                } else {
                    let _ = self.start_rithmic_realtime_from_snapshot(series, &snapshot);
                }
            }
        } else if let Some(waiters) = self.pending.remove(series) {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::ProviderHistory,
                if series.provider_id == "hyperliquid" {
                    "Hyperliquid historical bars are unavailable"
                } else {
                    "Rithmic historical bars are unavailable"
                },
            );
        }
    }
    pub(super) fn accept_history_completion(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
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
        if current != Some(generation) || suspended {
            if !suspended && current == Some(generation) && self.engine.has_subscription(series) {
                let _ = self.enqueue_history(series, generation);
            }
            return None;
        }
        match result {
            Ok(snapshot) if !cancelled => {
                self.history_retries.remove(&key);
                Some(snapshot)
            }
            Ok(_) => {
                if self.engine.has_subscription(series) {
                    let _ = self.enqueue_history(series, generation);
                }
                None
            }
            Err(error) => {
                if !self.schedule_history_retry(series, generation, range, &error) {
                    if range.is_none() {
                        self.history_deferred.remove(&key);
                        self.history_failed(series, generation);
                    } else {
                        self.broadcast_demand_error_for(
                            series,
                            FailureStage::ProviderHistory,
                            "Visible history backfill is temporarily unavailable",
                            None,
                        );
                        self.dispatch_deferred_history(series, generation);
                    }
                }
                None
            }
        }
    }
    pub(super) fn history_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        result: Result<HistorySnapshot, String>,
    ) {
        let Some(snapshot) = self.accept_history_completion(series, generation, range, result)
        else {
            return;
        };
        if snapshot.bars.is_empty() {
            if range.is_some() {
                self.dispatch_deferred_history(series, generation);
            } else {
                self.history_deferred.remove(&(series.clone(), generation));
                self.history_failed(series, generation);
            }
            return;
        }
        let replacing_existing = self.engine.series_snapshot(series).is_some();
        let Some(snapshot) = self.prepare_history_repair(series, generation, snapshot) else {
            self.history_failed(series, generation);
            return;
        };
        if self
            .install_completed_history(
                series,
                generation,
                snapshot,
                replacing_existing,
                true,
                range.is_none(),
            )
            .is_none()
        {
            return;
        }
        self.pending.remove(series);
        self.series_live_if_ready(series);
        if range.is_none() {
            let viewport_demands = self.engine.retained_viewports(series);
            for (consumer_id, consumer_generation, viewport) in viewport_demands {
                let _ = self.request_viewport_history(consumer_id, consumer_generation, viewport);
            }
        }
        self.dispatch_deferred_history(series, generation);
    }

    fn dispatch_deferred_history(&mut self, series: &BarSeriesKey, generation: ProviderGeneration) {
        let key = (series.clone(), generation);
        let Some(deferred) = self.history_deferred.remove(&key) else {
            return;
        };
        let range = match deferred {
            DeferredHistoryRequest::Full => None,
            DeferredHistoryRequest::Range(range) => Some(range),
        };
        let _ = self.enqueue_history_request(series, generation, range);
    }
    pub(super) fn install_completed_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        snapshot: HistorySnapshot,
        replace_covering: bool,
        publish: bool,
        reseed_live: bool,
    ) -> Option<(Vec<MarketBar>, Option<FormingBar>)> {
        let price_scale = snapshot.price_scale;
        let quantity_scale = snapshot.quantity_scale;
        let handoff_boundary_unix_nanos = snapshot.handoff_boundary_unix_nanos;
        let persisted_bars = snapshot.bars.clone();
        let mut forming = snapshot.forming;
        let provider_forming =
            if series.provider_id == "hyperliquid" || series.provider_id == "rithmic" {
                forming.take()
            } else {
                None
            };
        let bars = snapshot.bars;
        let installed = if replace_covering {
            self.engine.replace_covering_history(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
                publish,
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
        self.publish_installed_history(&publications);
        self.enqueue_persistence(
            series,
            generation,
            persisted_bars,
            false,
            "Local history persistence is unavailable",
        );
        if !reseed_live {
            return Some((bars, forming));
        }
        if series.provider_id == "hyperliquid" {
            if let Err(error) = self.finish_hyperliquid_history_handoff(
                series,
                generation,
                HyperliquidHandoffSeed {
                    price_scale,
                    quantity_scale,
                    bars: &bars,
                    forming: provider_forming,
                },
            ) {
                self.fail_history_handoff(series, &error);
                return None;
            }
        } else if let Err(error) = self.finish_rithmic_history_handoff(
            series,
            generation,
            RithmicHandoffSeed {
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

    /// Publishes one installed covering snapshot to every matching consumer
    /// as ready-but-undurable: the storage worker confirms durability next.
    pub(super) fn publish_installed_history(
        &mut self,
        publications: &[axiusflow_market_engine::ConsumerPublication],
    ) {
        for publication in publications {
            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                publish_state(
                    events,
                    publication,
                    SeriesLoadState::Ready,
                    PersistenceState::Pending,
                    None,
                );
            }
        }
    }

    pub(super) fn prepare_history_repair(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        mut snapshot: HistorySnapshot,
    ) -> Option<HistorySnapshot> {
        let had_local_history = self.local_loaded.contains(&(series.clone(), generation));
        let Some(current) = self.engine.series_snapshot(series) else {
            return Some(snapshot);
        };
        let merged = reconcile_history_repair(&current, snapshot.bars, HISTORY_BARS_PER_SERIES);
        let Ok(bars) = merged else {
            self.history_failed(series, generation);
            return None;
        };
        snapshot.bars = bars;
        if let Some(forming) = snapshot.forming.as_mut() {
            let next_sequence = snapshot
                .bars
                .last()
                .map_or(1, |bar| bar.source_sequence.saturating_add(1));
            forming.bar.source_sequence = next_sequence;
        }
        if had_local_history {
            self.local_loaded.remove(&(series.clone(), generation));
        }
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

    pub(super) fn finish_hyperliquid_history_handoff(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        seed: HyperliquidHandoffSeed<'_>,
    ) -> Result<(), String> {
        self.ensure_realtime(series)?;
        self.seed_hyperliquid_history(series, generation, seed)
            .then_some(())
            .ok_or_else(|| "Hyperliquid history/live handoff failed".to_string())
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

    pub(super) fn start_hyperliquid_realtime_from_snapshot(
        &mut self,
        series: &BarSeriesKey,
        snapshot: &axiusflow_market_engine::SeriesSnapshot,
    ) -> Result<(), String> {
        self.ensure_realtime(series)?;
        // A retained series whose newest bar is still forming hands that bar
        // over as the open period rather than as closed history, so the first
        // live replacement revises it instead of opening a duplicate period.
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
        if self.seed_hyperliquid_history(
            series,
            snapshot.provider_generation,
            HyperliquidHandoffSeed {
                price_scale: snapshot.price_scale,
                quantity_scale: snapshot.quantity_scale,
                bars,
                forming,
            },
        ) {
            Ok(())
        } else {
            Err("Hyperliquid cached history/live handoff failed".to_string())
        }
    }

    pub(super) fn seed_hyperliquid_history(
        &mut self,
        series: &BarSeriesKey,
        _generation: ProviderGeneration,
        seed: HyperliquidHandoffSeed<'_>,
    ) -> bool {
        let Some(live) = self.hyperliquid_live.get_mut(series) else {
            return true;
        };
        let HyperliquidHandoffSeed {
            price_scale,
            quantity_scale,
            bars,
            forming,
        } = seed;
        if live
            .seed(price_scale, quantity_scale, bars, forming)
            .is_ok()
        {
            return true;
        }
        live.history_ready = false;
        live.dirty = false;
        self.broadcast_series_recovery_for(series, "Hyperliquid history/live handoff failed");
        false
    }

    pub(super) fn seed_rithmic_history(
        &mut self,
        series: &BarSeriesKey,
        _generation: ProviderGeneration,
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
        self.broadcast_series_recovery_for(series, "Rithmic history/live handoff failed");
        false
    }

    pub(super) fn prune_history_tracking(&mut self) {
        // Timeframe/symbol changes supersede the old series immediately. Cancel
        // its queued/in-flight provider work so one user rapidly changing the
        // chart cannot occupy the bounded history queue with obsolete fetches.
        for ((series, _), stop) in &self.history_cancellations {
            if !self.engine.has_subscription(series) {
                stop.store(true, Ordering::Release);
            }
        }
        self.history_deferred
            .retain(|(series, _), _| self.engine.has_subscription(series));
        self.history_retries.retain(|(series, generation), _| {
            self.engine.has_subscription(series)
                && self
                    .engine
                    .provider_status(&series.provider_id)
                    .and_then(|status| status.generation)
                    == Some(*generation)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc as StdArc, Mutex as StdMutex, mpsc};

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
        HistoryRequest {
            series: test_series(instrument_id),
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            instrument: None,
            maximum_bars: 10,
            range: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
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
            })
        }
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
            Ok(Command::HistoryCompleted(series, _, None, Ok(_))) if series.instrument_id == "first"
        ));
        assert!(matches!(
            completions_rx.try_recv(),
            Ok(Command::HistoryCompleted(series, _, None, Ok(_))) if series.instrument_id == "second"
        ));
    }

    #[test]
    fn deferred_history_coalesces_ranges_and_full_request_supersedes_them() {
        let mut deferred = DeferredHistoryRequest::Range(HistoryRange {
            start_unix_nanos: 100,
            end_unix_nanos: 200,
        });
        merge_deferred_history_request(
            &mut deferred,
            Some(HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 150,
            }),
        );
        assert_eq!(
            deferred,
            DeferredHistoryRequest::Range(HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 200,
            })
        );

        merge_deferred_history_request(&mut deferred, None);
        assert_eq!(deferred, DeferredHistoryRequest::Full);
        merge_deferred_history_request(
            &mut deferred,
            Some(HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 300,
            }),
        );
        assert_eq!(deferred, DeferredHistoryRequest::Full);
    }
}
