use super::{
    Arc, AtomicBool, BarPeriod, BarSeriesKey, COINBASE_PUBLIC_ACCOUNT_ID, Command, Coordinator,
    ENTITLEMENT_CLASS, FailureStage, HISTORY_BARS_PER_SERIES, HistoryPrecedence, HistoryRange,
    HistoryScope, Instant, LOCAL_HISTORY_READ_TIMEOUT, LocalHistoryError, LocalHistoryStore,
    LocalRangeHistory, MarketBar, Ordering, PersistenceState, ProviderGeneration,
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, Receiver, RetainedRange, SeriesLoadState, StorageRequest,
    StoredHistory, SyncSender, TrySendError, aggregate_coinbase_bars, canonical_local_range,
    canonicalize_coinbase_history, coinbase_bar_coverage_ranges, coinbase_interval,
    coinbase_series_interval, fail_waiters, local_history_failure_stage, publish_state,
    reconcile_history_repair, record_covered_range, thread,
};

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
                Some(Ok(storage)) => local_history_scope(&series).and_then(|scope| {
                    let stored = storage
                        .read_range(
                            &scope,
                            &series,
                            range.start_unix_nanos,
                            range.end_unix_nanos,
                        )
                        .map_err(|error| error.to_string())?;
                    let stored = canonical_local_range(&series, stored)?;
                    let confirmed_empty = storage
                        .confirmed_empty_ranges(&scope, &series)
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .map(|range| HistoryRange {
                            start_unix_nanos: range.start_unix_nanos,
                            end_unix_nanos: range.end_unix_nanos,
                        })
                        .collect();
                    Ok(LocalRangeHistory {
                        stored,
                        confirmed_empty,
                    })
                }),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(LocalRangeHistory {
                    stored: None,
                    confirmed_empty: Vec::new(),
                }),
            };
            Command::ViewportHistoryLocalCompleted(series, generation, range, result)
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
        StorageRequest::RecordConfirmedEmpty(series, range) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => local_history_scope(&series)
                    .map_err(|_| LocalHistoryError::InvalidSeries)
                    .and_then(|scope| {
                        storage.record_confirmed_empty(
                            &scope,
                            &series,
                            range.start_unix_nanos,
                            range.end_unix_nanos,
                        )
                    }),
                Some(Err(_)) => Err(LocalHistoryError::Unavailable),
                None => Ok(()),
            };
            Command::ConfirmedEmptyRecorded(series, result)
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
    if let Some(mut stored) = storage
        .read_latest(&scope, series)
        .map_err(|error| error.to_string())?
    {
        stored.bars = canonicalize_coinbase_history(series, stored.bars)?;
        if stored.bars.is_empty() {
            return Ok(None);
        }
        return Ok(Some(stored));
    }
    let target_seconds = match series.period {
        BarPeriod::Time { seconds } if series.provider_id == "coinbase" && seconds > 60 => seconds,
        BarPeriod::Time { .. }
        | BarPeriod::Tick { .. }
        | BarPeriod::Session { .. }
        | BarPeriod::Week { .. }
        | BarPeriod::Month { .. } => return Ok(None),
    };
    let interval = coinbase_interval(target_seconds)?;
    let source_series = BarSeriesKey {
        period: BarPeriod::time(60).map_err(|error| error.to_string())?,
        ..series.clone()
    };
    let Some(source) = storage
        .read_latest(&scope, &source_series)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let (bars, _) = aggregate_coinbase_bars(&source.bars, interval, None)?;
    if bars.is_empty() {
        return Ok(None);
    }
    let durable = storage.persist(&scope, series, &bars, true).is_ok();
    Ok(Some(StoredHistory {
        bars,
        derived: true,
        durable,
    }))
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
    let account_id = match series.provider_id.as_str() {
        "coinbase" if series.entitlement_id == ENTITLEMENT_CLASS => COINBASE_PUBLIC_ACCOUNT_ID,
        "rithmic" => RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        _ => return Err("local history provider scope is unsupported".to_string()),
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
        let repaired_range = (!derived && series.provider_id == "coinbase")
            .then(|| coinbase_bar_coverage_ranges(series, &bars).ok())
            .flatten()
            .and_then(|ranges| {
                Some(HistoryRange {
                    start_unix_nanos: ranges.first()?.start_unix_nanos,
                    end_unix_nanos: ranges.last()?.end_unix_nanos,
                })
            });
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
            if let Some(range) = repaired_range {
                self.remember_pending_empty_repair(series, range);
            }
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
                let coverage = coinbase_bar_coverage_ranges(series, &stored.bars);
                if let Ok(publications) = self.engine.install_history(
                    generation,
                    series,
                    price_scale,
                    quantity_scale,
                    stored.bars,
                ) {
                    if let Ok(ranges) = coverage {
                        for range in ranges {
                            record_covered_range(&mut self.history_coverage, series, range);
                        }
                    }
                    self.local_loaded.insert((series.clone(), generation));
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            publish_state(
                                events,
                                &publication,
                                SeriesLoadState::Partial,
                                persistence,
                                Some(if stored.derived && !stored.durable {
                                    "Showing derived history from retained one-minute data; derived-cache persistence is unavailable"
                                } else if stored.derived {
                                    "Showing retained derived history while provider repair runs"
                                } else {
                                    "Showing retained local history while provider repair runs"
                                }),
                            );
                        }
                    }
                }
            }
            Ok(Some(_) | None) => {}
            Err(_) => {
                self.broadcast_persistence_for(
                    series,
                    PersistenceState::Degraded,
                    Some("Local history is unavailable; provider repair continues"),
                );
            }
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

    pub(super) fn viewport_history_local_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: HistoryRange,
        result: Result<LocalRangeHistory, String>,
    ) {
        self.viewport_history_local_inflight
            .remove(&(series.clone(), generation, range));
        if self
            .viewport_history_ranges
            .get(&(series.clone(), generation))
            != Some(&range)
            || self
                .engine
                .provider_status(&series.provider_id)
                .and_then(|status| status.generation)
                != Some(generation)
        {
            return;
        }
        let Ok(local) = result else {
            self.schedule_next_coinbase_viewport_page(series, generation);
            return;
        };
        for confirmed_empty in local.confirmed_empty {
            record_covered_range(&mut self.history_coverage, series, confirmed_empty);
        }
        if let Some(stored) = local.stored
            && !stored.bars.is_empty()
        {
            let bars = self
                .engine
                .series_snapshot(series)
                .and_then(|current| {
                    reconcile_history_repair(
                        &current,
                        stored.bars.clone(),
                        HISTORY_BARS_PER_SERIES,
                        coinbase_series_interval(series).ok(),
                        HistoryPrecedence::Current,
                    )
                    .ok()
                })
                .unwrap_or(stored.bars);
            if let Ok((price_scale, quantity_scale)) = self.series_precision(series)
                && let Ok(publications) = self.engine.replace_covering_history(
                    generation,
                    series,
                    price_scale,
                    quantity_scale,
                    bars.clone(),
                    true,
                )
            {
                if let Ok(ranges) = coinbase_bar_coverage_ranges(series, &bars) {
                    for covered in ranges {
                        record_covered_range(&mut self.history_coverage, series, covered);
                    }
                }
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_state(
                            events,
                            &publication,
                            SeriesLoadState::Partial,
                            PersistenceState::Durable,
                            Some("Showing retained local history while visible coverage repairs"),
                        );
                    }
                }
                self.resync_coinbase_live(series);
            }
        }
        self.schedule_next_coinbase_viewport_page(series, generation);
    }

    pub(super) fn install_warm_local_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<Option<StoredHistory>, String>,
    ) {
        let Ok(Some(stored)) = result else {
            return;
        };
        if stored.bars.is_empty() {
            return;
        }
        if series.provider_id == "rithmic" {
            self.retained_history.insert(series.clone(), stored);
            return;
        }
        let Some(warm) = self.warm_series.get(series) else {
            return;
        };
        let (Ok(price_scale), Ok(quantity_scale)) = (
            u8::try_from(warm.instrument.price_scale),
            u8::try_from(warm.instrument.quantity_scale),
        ) else {
            return;
        };
        let coverage = coinbase_bar_coverage_ranges(series, &stored.bars);
        if self
            .engine
            .install_retained_history(generation, series, price_scale, quantity_scale, stored.bars)
            .is_err()
        {
            return;
        }
        if let Ok(ranges) = coverage {
            for range in ranges {
                record_covered_range(&mut self.history_coverage, series, range);
            }
        }
        self.prewarmed.insert(series.clone());
        self.local_loaded.insert((series.clone(), generation));
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
