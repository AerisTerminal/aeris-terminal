use super::{
    Arc, AtomicBool, BarSeriesKey, Command, Coordinator, FailureStage, HistoryRange, HistoryScope,
    Instant, LOCAL_HISTORY_READ_TIMEOUT, LocalHistoryError, LocalHistoryStore, MarketBar, Ordering,
    PersistenceState, ProviderGeneration, RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, Receiver,
    RetainedRange, SeriesLoadState, StorageRequest, StoredHistory, SyncSender, TrySendError,
    fail_waiters, local_history_failure_stage, publish_state, thread,
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
        if self
            .storage
            .try_send(StorageRequest::Persist(
                series.clone(),
                generation,
                bars,
                derived,
                self.protected_history_ranges(),
                Instant::now(),
            ))
            .is_err()
        {
            self.broadcast_persistence_for(series, PersistenceState::Degraded, Some(unavailable));
            self.broadcast_demand_error_for(
                series,
                FailureStage::FilesystemWrite,
                unavailable,
                Some(0),
            );
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
                self.local_history_deadlines
                    .insert(key, Instant::now() + LOCAL_HISTORY_READ_TIMEOUT);
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
        if self.warming.remove(&(series.clone(), generation)) {
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
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for (series, generation) in expired {
            self.local_history_deadlines
                .remove(&(series.clone(), generation));
            self.broadcast_persistence_for(
                &series,
                PersistenceState::Degraded,
                Some("Local history read timed out; provider repair continues"),
            );
            if let Err(detail) = self.enqueue_history(&series, generation)
                && let Some(waiters) = self.pending.remove(&series)
            {
                fail_waiters(
                    &mut self.events,
                    waiters,
                    &series,
                    FailureStage::ProviderHistory,
                    detail,
                );
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
        if self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        let (state, detail) = if result.is_ok() {
            (PersistenceState::Durable, None)
        } else {
            (
                PersistenceState::Degraded,
                Some("Local history persistence is degraded"),
            )
        };
        self.broadcast_persistence_for(series, state, detail);
        if let Err(error) = result {
            self.broadcast_demand_error_for(
                series,
                local_history_failure_stage(error),
                &error.to_string(),
                Some(elapsed_millis),
            );
        }
    }
}
