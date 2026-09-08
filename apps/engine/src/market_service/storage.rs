use super::history::{merge_history_ranges, reconcile_history_repair};
use super::{
    Arc, AtomicBool, BarSeriesKey, Command, Coordinator, FailureStage, HISTORY_BARS_PER_SERIES,
    HistoryRange, HistoryScope, Instant, LOCAL_HISTORY_READ_TIMEOUT, LocalHistoryError,
    LocalHistoryStore, MarketBar, Ordering, PERSISTENCE_BACKLOG_CAPACITY, PendingLocalHistoryRead,
    PersistenceState, ProviderGeneration, RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, Receiver,
    RetainedRange, SeriesLoadState, StorageRequest, StoredHistory, SyncSender, TrySendError,
    fail_waiters, publish_state, thread,
};
use crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ACCOUNT_ID;

pub(super) fn spawn_storage_worker(
    storage: Option<Result<LocalHistoryStore, String>>,
    requests: Receiver<StorageRequest>,
    completions: SyncSender<Command>,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("axiusflow-local-history".to_string())
        .spawn(move || run_storage_worker(storage, &requests, &completions, &shutdown))
        .map_err(|error| error.to_string())
}

pub(super) fn run_storage_worker(
    mut storage: Option<Result<LocalHistoryStore, String>>,
    requests: &Receiver<StorageRequest>,
    completions: &SyncSender<Command>,
    shutdown: &AtomicBool,
) {
    while let Ok(request) = requests.recv() {
        let completion = storage_completion(&mut storage, request);
        if !shutdown.load(Ordering::Acquire)
            && completions.send(completion).is_err()
            && !shutdown.load(Ordering::Acquire)
        {
            return;
        }
    }
}

pub(super) fn storage_completion(
    storage: &mut Option<Result<LocalHistoryStore, String>>,
    request: StorageRequest,
) -> Command {
    match request {
        StorageRequest::Read(series, generation) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => read_local_history(storage, &series),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(None),
            };
            Command::LocalHistoryCompleted(series, generation, result)
        }
        StorageRequest::ReadRange(series, generation, range) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => read_local_history_range(storage, &series, range),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(None),
            };
            Command::LocalHistoryRangeCompleted(series, generation, range, result)
        }
        StorageRequest::Persist(
            series,
            generation,
            bars,
            derived,
            protected_ranges,
            started_at,
        ) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => {
                    persist_local_history(storage, &series, &bars, derived, &protected_ranges)
                }
                Some(Err(_)) => Err(LocalHistoryError::Unavailable),
                None => Ok(()),
            };
            Command::PersistenceCompleted(
                series,
                generation,
                result,
                u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            )
        }
        StorageRequest::ResolveConfirmedEmpty(series, range) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => local_history_scope(&series)
                    .map_err(|_| LocalHistoryError::InvalidSeries)
                    .and_then(|scope| {
                        storage.resolve_confirmed_empty(
                            &scope,
                            &series,
                            range.start_unix_nanos,
                            range.end_unix_nanos,
                        )
                    }),
                Some(Err(_)) => Err(LocalHistoryError::Unavailable),
                None => Ok(()),
            };
            Command::ConfirmedEmptyResolved(series, range, result)
        }
    }
}
pub(super) fn read_local_history(
    storage: &mut LocalHistoryStore,
    series: &BarSeriesKey,
) -> Result<Option<StoredHistory>, String> {
    let scope = local_history_scope(series)?;
    storage
        .read_latest(&scope, series)
        .map_err(|error| error.to_string())
}

pub(super) fn read_local_history_range(
    storage: &mut LocalHistoryStore,
    series: &BarSeriesKey,
    range: HistoryRange,
) -> Result<Option<StoredHistory>, String> {
    let scope = local_history_scope(series)?;
    storage
        .read_range(&scope, series, range.start_unix_nanos, range.end_unix_nanos)
        .map_err(|error| error.to_string())
}

pub(super) fn persist_local_history(
    storage: &mut LocalHistoryStore,
    series: &BarSeriesKey,
    bars: &[MarketBar],
    derived: bool,
    protected_ranges: &[(BarSeriesKey, HistoryRange)],
) -> Result<(), LocalHistoryError> {
    let scope = local_history_scope(series).map_err(|_| LocalHistoryError::InvalidSeries)?;
    let protected = protected_ranges
        .iter()
        .map(|(protected_series, range)| {
            Ok((
                local_history_scope(protected_series)
                    .map_err(|_| LocalHistoryError::InvalidSeries)?,
                protected_series.clone(),
                RetainedRange {
                    start_unix_nanos: range.start_unix_nanos,
                    end_unix_nanos: range.end_unix_nanos,
                },
            ))
        })
        .collect::<Result<Vec<_>, LocalHistoryError>>()?;
    storage.persist_with_protected_series_ranges(&scope, series, bars, derived, &protected)
}
pub(super) fn local_history_scope(series: &BarSeriesKey) -> Result<HistoryScope, String> {
    let account_id = if series.provider_id == "hyperliquid" {
        HYPERLIQUID_PUBLIC_ACCOUNT_ID
    } else if series.provider_id == "rithmic" {
        RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID
    } else {
        return Err("local history provider scope is unsupported".to_string());
    };
    Ok(HistoryScope {
        provider_id: series.provider_id.clone(),
        account_id: account_id.to_string(),
        entitlement_revision: series.entitlement_id.clone(),
    })
}

impl Coordinator<'_> {
    pub(super) fn enqueue_persistence(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        bars: Vec<MarketBar>,
        derived: bool,
        unavailable: &'static str,
    ) {
        let request = StorageRequest::Persist(
            series.clone(),
            generation,
            bars,
            derived,
            self.protected_history_ranges(),
            Instant::now(),
        );
        let key = (series.clone(), generation);
        match self.storage.try_send(request) {
            Ok(()) => {
                *self.persistence_pending.entry(key).or_default() += 1;
            }
            Err(TrySendError::Full(request))
                if self.persistence_backlog.len() < PERSISTENCE_BACKLOG_CAPACITY =>
            {
                *self.persistence_pending.entry(key).or_default() += 1;
                self.persistence_backlog.push_back(request);
            }
            Err(TrySendError::Full(_)) => {
                self.persistence_degraded.insert(key);
                self.broadcast_persistence_for(
                    series,
                    PersistenceState::Degraded,
                    Some(unavailable),
                );
                eprintln!(
                    "Axiusflow engine local history persistence backlog overflowed for {}: {unavailable}",
                    series.instrument_id
                );
            }
            Err(TrySendError::Disconnected(_)) => {
                self.persistence_degraded.insert(key);
                self.degrade_disconnected_persistence(unavailable);
            }
        }
    }

    pub(super) fn retry_persistence_backlog(&mut self) {
        while let Some(request) = self.persistence_backlog.pop_front() {
            match self.storage.try_send(request) {
                Ok(()) => {}
                Err(TrySendError::Full(request)) => {
                    self.persistence_backlog.push_front(request);
                    break;
                }
                Err(TrySendError::Disconnected(request)) => {
                    self.persistence_backlog.push_front(request);
                    self.degrade_disconnected_persistence(
                        "Local history persistence worker is unavailable",
                    );
                    break;
                }
            }
        }
    }

    fn degrade_disconnected_persistence(&mut self, detail: &'static str) {
        let mut affected = self
            .persistence_pending
            .keys()
            .map(|(series, generation)| (series.clone(), *generation))
            .collect::<Vec<_>>();
        for request in &self.persistence_backlog {
            if let Some(key) = persistence_key(request) {
                affected.push(key);
            }
        }
        self.persistence_pending.clear();
        self.persistence_backlog.clear();
        affected.sort();
        affected.dedup();
        for (series, generation) in affected {
            self.persistence_degraded
                .insert((series.clone(), generation));
            if self
                .engine
                .provider_status(&series.provider_id)
                .and_then(|status| status.generation)
                == Some(generation)
            {
                self.broadcast_persistence_for(&series, PersistenceState::Degraded, Some(detail));
            }
        }
    }

    pub(super) fn flush_persistence_backlog_for_shutdown(&mut self) {
        while let Some(request) = self.persistence_backlog.pop_front() {
            if self.storage.send(request).is_err() {
                break;
            }
        }
    }

    pub(super) fn enqueue_local_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        let key = (series.clone(), generation);
        if self.local_history_deadlines.contains_key(&key) {
            return Ok(());
        }
        match self
            .storage
            .try_send(StorageRequest::Read(series.clone(), generation))
        {
            Ok(()) => {
                self.local_history_deadlines.insert(
                    key,
                    PendingLocalHistoryRead {
                        deadline: Instant::now() + LOCAL_HISTORY_READ_TIMEOUT,
                        range: None,
                    },
                );
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err("local history capacity is temporarily exhausted"),
            Err(TrySendError::Disconnected(_)) => Err("local history worker is unavailable"),
        }
    }

    pub(super) fn enqueue_local_history_range(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: HistoryRange,
    ) -> Result<(), &'static str> {
        let key = (series.clone(), generation);
        if self.local_history_deadlines.contains_key(&key) {
            return Err("local history read is already pending");
        }
        match self
            .storage
            .try_send(StorageRequest::ReadRange(series.clone(), generation, range))
        {
            Ok(()) => {
                self.local_history_deadlines.insert(
                    key,
                    PendingLocalHistoryRead {
                        deadline: Instant::now() + LOCAL_HISTORY_READ_TIMEOUT,
                        range: Some(range),
                    },
                );
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err("local history capacity is temporarily exhausted"),
            Err(TrySendError::Disconnected(_)) => Err("local history worker is unavailable"),
        }
    }
    pub(super) fn local_history_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<Option<StoredHistory>, String>,
    ) {
        if self.warm_reads.contains(&(series.clone(), generation)) {
            self.install_warm_local_history(series, generation, result);
            return;
        }
        let expected = self
            .local_history_deadlines
            .remove(&(series.clone(), generation))
            .is_some();
        if !expected && !self.pending.contains_key(series) {
            return;
        }
        if self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        match result {
            Ok(Some(stored)) if !stored.bars.is_empty() => {
                let Ok((price_scale, quantity_scale)) = self.series_precision(series) else {
                    return;
                };
                let persistence = if stored.durable {
                    PersistenceState::Durable
                } else {
                    PersistenceState::Degraded
                };
                if let Ok(publications) = self.engine.install_history(
                    generation,
                    series,
                    price_scale,
                    quantity_scale,
                    stored.bars,
                ) {
                    self.local_loaded.insert((series.clone(), generation));
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            publish_state(
                                events,
                                &publication,
                                SeriesLoadState::Partial,
                                persistence,
                                Some("Showing retained local history while provider repair runs"),
                            );
                        }
                    }
                }
            }
            Ok(Some(_) | None) => {}
            Err(_) => self.broadcast_persistence_for(
                series,
                PersistenceState::Degraded,
                Some("Local history is unavailable; provider repair continues"),
            ),
        }
        if let Err(detail) = self.enqueue_history(series, generation)
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

    pub(super) fn local_history_range_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: HistoryRange,
        result: Result<Option<StoredHistory>, String>,
    ) {
        let key = (series.clone(), generation);
        let expected = self
            .local_history_deadlines
            .get(&key)
            .is_some_and(|pending| pending.range == Some(range));
        if !expected {
            return;
        }
        // This completion owns the currently tracked range, so retire only
        // its exact deadline even if the consumer was removed or the provider
        // generation moved on while the storage worker was reading it.
        self.local_history_deadlines.remove(&key);
        if !self.engine.has_subscription(series)
            || self
                .engine
                .provider_status(&series.provider_id)
                .and_then(|status| status.generation)
                != Some(generation)
        {
            return;
        }

        let local_failed = result.is_err();
        match result {
            Ok(Some(stored)) if !stored.bars.is_empty() => {
                if let Some(current) = self.engine.series_snapshot(series)
                    && let Ok(bars) =
                        reconcile_history_repair(&current, stored.bars, HISTORY_BARS_PER_SERIES)
                    && let Ok(publications) = self.engine.replace_covering_history(
                        generation,
                        series,
                        current.price_scale,
                        current.quantity_scale,
                        bars,
                        true,
                    )
                {
                    let persistence = if stored.durable {
                        PersistenceState::Durable
                    } else {
                        PersistenceState::Degraded
                    };
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            publish_state(
                                events,
                                &publication,
                                SeriesLoadState::Partial,
                                persistence,
                                Some("Showing cached viewport history while provider repair runs"),
                            );
                        }
                    }
                }
            }
            Ok(Some(_) | None) => {}
            Err(_) => self.broadcast_persistence_for(
                series,
                PersistenceState::Degraded,
                Some("Cached viewport history is unavailable; provider repair continues"),
            ),
        }

        let mut provider_range = range;
        if let Some(current_range) = self
            .current_viewport_history_range(series)
            .filter(|current_range| *current_range != range)
            && (local_failed
                || self
                    .enqueue_local_history_range(series, generation, current_range)
                    .is_err())
        {
            // Cache access already failed, or the bounded local lane cannot
            // accept the latest viewport yet. Provider fallback must cover
            // both regions so a pan suppressed behind the first read is not
            // lost.
            provider_range = merge_history_ranges(range, current_range);
        }

        if self
            .enqueue_history_request(series, generation, Some(provider_range))
            .is_err()
        {
            self.broadcast_demand_error_for(
                series,
                FailureStage::ProviderHistory,
                "Visible history backfill is temporarily unavailable",
                None,
            );
        }
    }
    pub(super) fn install_warm_local_history(
        &mut self,
        series: &BarSeriesKey,
        _generation: ProviderGeneration,
        result: Result<Option<StoredHistory>, String>,
    ) {
        if let Ok(Some(stored)) = result
            && !stored.bars.is_empty()
            && (series.provider_id == "rithmic" || series.provider_id == "hyperliquid")
        {
            self.retained_history.insert(series.clone(), stored);
        }
    }

    pub(super) fn expire_local_history_reads(&mut self) {
        let now = Instant::now();
        let expired = self
            .local_history_deadlines
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(key, pending)| (key.clone(), pending.range))
            .collect::<Vec<_>>();
        for ((series, generation), range) in expired {
            self.local_history_deadlines
                .remove(&(series.clone(), generation));
            self.broadcast_persistence_for(
                &series,
                PersistenceState::Degraded,
                Some("Local history read timed out; provider repair continues"),
            );
            let repair_range = range.map(|expired_range| {
                self.current_viewport_history_range(&series)
                    .map_or(expired_range, |current_range| {
                        merge_history_ranges(expired_range, current_range)
                    })
            });
            if let Err(detail) = self.enqueue_history_request(&series, generation, repair_range) {
                if range.is_none()
                    && let Some(waiters) = self.pending.remove(&series)
                {
                    fail_waiters(
                        &mut self.events,
                        waiters,
                        &series,
                        FailureStage::ProviderHistory,
                        detail,
                    );
                } else if range.is_some() {
                    self.broadcast_demand_error_for(
                        &series,
                        FailureStage::ProviderHistory,
                        "Visible history backfill is temporarily unavailable",
                        None,
                    );
                }
            }
        }
    }

    pub(super) fn persistence_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<(), LocalHistoryError>,
        elapsed_millis: u64,
    ) {
        let key = (series.clone(), generation);
        if let Some(pending) = self.persistence_pending.get_mut(&key) {
            *pending = pending.saturating_sub(1);
            if *pending == 0 {
                self.persistence_pending.remove(&key);
            }
        }
        if result.is_err() {
            self.persistence_degraded.insert(key.clone());
        }
        self.retry_persistence_backlog();
        let still_pending = self.persistence_pending.contains_key(&key);
        let degraded = self.persistence_degraded.contains(&key);
        if self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            if !still_pending {
                self.persistence_degraded.remove(&key);
            }
            return;
        }
        let (state, detail) = if degraded {
            (
                PersistenceState::Degraded,
                Some("Local history persistence is degraded"),
            )
        } else if still_pending {
            (PersistenceState::Pending, None)
        } else {
            (PersistenceState::Durable, None)
        };
        self.broadcast_persistence_for(series, state, detail);
        if let Err(error) = result {
            eprintln!(
                "Axiusflow engine local history persistence degraded for {} after {elapsed_millis} ms: {error}",
                series.instrument_id
            );
        }
    }
}

fn persistence_key(request: &StorageRequest) -> Option<(BarSeriesKey, ProviderGeneration)> {
    match request {
        StorageRequest::Persist(series, generation, ..) => Some((series.clone(), *generation)),
        StorageRequest::Read(..)
        | StorageRequest::ReadRange(..)
        | StorageRequest::ResolveConfirmedEmpty(..) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_market_data::BarPeriod;
    use std::num::NonZeroU64;

    fn test_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "fixture:rithmic:history".to_string(),
            entitlement_id: "fixture-entitlement".to_string(),
            period: BarPeriod::time(60).expect("fixture period"),
            definition_version: 1,
        }
    }

    #[test]
    fn range_read_completion_preserves_exact_requested_window() {
        let series = test_series();
        let generation = ProviderGeneration(NonZeroU64::MIN);
        let range = HistoryRange {
            start_unix_nanos: 60_000_000_000,
            end_unix_nanos: 180_000_000_000,
        };
        let mut storage = None;

        assert!(matches!(
            storage_completion(
                &mut storage,
                StorageRequest::ReadRange(series.clone(), generation, range)
            ),
            Command::LocalHistoryRangeCompleted(completed, completed_generation, completed_range, Ok(None))
                if completed == series
                    && completed_generation == generation
                    && completed_range == range
        ));
    }

    #[test]
    fn persistence_failure_is_reported_by_storage_ack() {
        let series = test_series();
        let generation = ProviderGeneration(NonZeroU64::MIN);
        let mut storage = Some(Err("fixture storage unavailable".to_string()));

        assert!(matches!(
            storage_completion(
                &mut storage,
                StorageRequest::Persist(
                    series.clone(),
                    generation,
                    Vec::new(),
                    false,
                    Vec::new(),
                    Instant::now(),
                )
            ),
            Command::PersistenceCompleted(completed, completed_generation, Err(LocalHistoryError::Unavailable), _)
                if completed == series && completed_generation == generation
        ));
    }
}
