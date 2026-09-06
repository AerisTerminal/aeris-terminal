use super::{
    BTreeMap, BTreeSet, BarSeriesKey, CONSUMER_SERIES_QUEUE_CAPACITY, CanonicalOrderBookState,
    ClientId, CoinbaseHistoryReadiness, ConsumerEvents, ConsumerId, Coordinator, DemandError,
    DemandWaiter, DepthLevel, EngineError, EngineFaultCode, FailureStage, GenerationId,
    IpcOrderBookLevel, IpcOrderBookSnapshot, IpcOrderBookState, IpcOrderFlowLevel,
    IpcOrderFlowSnapshot, IpcOrderFlowTrade, IpcOrderFlowUpdate, IpcSeriesSnapshot,
    LocalHistoryError, NonZeroU64, OrderBookRecoveryReason, OrderFlowAggressor,
    OrderFlowPublicationKind, PersistenceState, ProviderConnectionState, ProviderGeneration,
    ProviderOrderBook, ProviderState, REALTIME_DRAIN_BUDGET, Reply, SeriesKey, SeriesLoadState,
    SeriesState, SeriesTailOperation, SeriesUpdateOperation, SyncSender, TrySendError,
    authorize_consumer, envelope, ipc_bar, ipc_series,
};

pub(super) fn fail_waiters(
    events: &mut BTreeMap<ConsumerId, ConsumerEvents>,
    waiters: Vec<DemandWaiter>,
    series: &BarSeriesKey,
    stage: FailureStage,
    detail: &str,
) {
    for waiter in waiters {
        if let Some(events) = events.get_mut(&waiter.consumer_id) {
            events.series_state = Some(series_state(
                waiter.consumer_id,
                waiter.generation,
                ipc_series(series),
                SeriesLoadState::Failed,
                Some(detail.to_string()),
            ));
            events.demand_error = Some(envelope::Payload::DemandError(DemandError {
                consumer_id: waiter.consumer_id.0.get(),
                generation: waiter.generation.0.get(),
                code: EngineFaultCode::Retryable as i32,
                stage: failure_stage_name(stage).to_string(),
                detail: detail.to_string(),
                series: Some(ipc_series(series)),
                stage_code: stage as i32,
                cause: failure_stage_cause(stage).to_string(),
                elapsed_millis: Some(
                    u64::try_from(waiter.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
            }));
        }
    }
}

pub(super) const fn failure_stage_name(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::Unspecified => "unspecified",
        FailureStage::ProviderHistory => "provider_history",
        FailureStage::CanonicalValidation => "canonical_validation",
        FailureStage::MemoryInstall => "memory_install",
        FailureStage::Aggregation => "aggregation",
        FailureStage::SegmentEncode => "segment_encode",
        FailureStage::Encryption => "encryption",
        FailureStage::FilesystemWrite => "filesystem_write",
        FailureStage::CatalogCommit => "catalog_commit",
        FailureStage::Handoff => "handoff",
        FailureStage::Publication => "publication",
        FailureStage::IpcSend => "ipc_send",
        FailureStage::ChartInstall => "chart_install",
        FailureStage::ProviderRealtime => "provider_realtime",
    }
}

pub(super) const fn failure_stage_cause(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::ProviderHistory => "provider history did not produce usable canonical bars",
        FailureStage::CanonicalValidation => "market data failed canonical validation",
        FailureStage::MemoryInstall => "validated market data could not enter bounded memory",
        FailureStage::Aggregation => "canonical events could not be aggregated",
        FailureStage::SegmentEncode => "canonical history could not be encoded for persistence",
        FailureStage::Encryption => "history encryption could not complete",
        FailureStage::FilesystemWrite => "encrypted history could not be written durably",
        FailureStage::CatalogCommit => "history catalog publication could not commit",
        FailureStage::Handoff => "history and realtime state could not be joined safely",
        FailureStage::Publication => "validated market state could not be published",
        FailureStage::IpcSend => "local protocol delivery could not complete",
        FailureStage::ChartInstall => "desktop presentation rejected the engine publication",
        FailureStage::ProviderRealtime => "provider realtime delivery was interrupted",
        FailureStage::Unspecified => "the owning processing stage is unavailable",
    }
}

pub(super) const fn engine_install_failure_stage(error: &EngineError) -> FailureStage {
    match error {
        EngineError::EmptySeries
        | EngineError::DiscontinuousSeries { .. }
        | EngineError::NonIncreasingSeriesTime
        | EngineError::ConflictingSeriesGeneration(_)
        | EngineError::InvalidMarketData(_) => FailureStage::CanonicalValidation,
        _ => FailureStage::MemoryInstall,
    }
}

pub(super) const fn local_history_failure_stage(error: LocalHistoryError) -> FailureStage {
    match error {
        LocalHistoryError::SegmentEncode
        | LocalHistoryError::InvalidSeries
        | LocalHistoryError::EmptySeries
        | LocalHistoryError::InvalidRange
        | LocalHistoryError::InvalidSegment => FailureStage::SegmentEncode,
        LocalHistoryError::Encryption | LocalHistoryError::InvalidVaultKey => {
            FailureStage::Encryption
        }
        LocalHistoryError::FilesystemWrite
        | LocalHistoryError::InvalidRoot
        | LocalHistoryError::Unavailable => FailureStage::FilesystemWrite,
        LocalHistoryError::CatalogCommit => FailureStage::CatalogCommit,
    }
}

const COINBASE_PROVISIONAL_DETAIL: &str =
    "The current Coinbase candle is provisional until its authoritative close";

/// Queues a covering image with the readiness of the shared live series.
///
/// Re-publication happens when another consumer joins, a pane changes resource
/// class, and a deferred viewport page becomes visible. Those presentation
/// changes must not downgrade an already-live series to generic `Ready`.
pub(super) fn publish_current_state(
    events: &mut ConsumerEvents,
    publication: &axiusflow_market_engine::ConsumerPublication,
    coinbase_history: Option<CoinbaseHistoryReadiness>,
    persistence: PersistenceState,
) {
    match coinbase_history {
        Some(CoinbaseHistoryReadiness::Authoritative) => publish_state(
            events,
            publication,
            SeriesLoadState::Live,
            persistence,
            None,
        ),
        Some(CoinbaseHistoryReadiness::Provisional) => publish_state(
            events,
            publication,
            SeriesLoadState::Partial,
            persistence,
            Some(COINBASE_PROVISIONAL_DETAIL),
        ),
        Some(CoinbaseHistoryReadiness::Pending) | None => publish_state(
            events,
            publication,
            SeriesLoadState::Ready,
            persistence,
            None,
        ),
    }
}

pub(super) fn publish_state(
    events: &mut ConsumerEvents,
    publication: &axiusflow_market_engine::ConsumerPublication,
    state: SeriesLoadState,
    persistence: PersistenceState,
    detail: Option<&str>,
) {
    let series = ipc_series(&publication.snapshot.series);
    events.publish_snapshot(snapshot_message(publication));
    events.series_state = Some(series_state_with_persistence(
        publication.consumer_id,
        publication.generation,
        series,
        state,
        persistence,
        detail.map(str::to_string),
    ));
}

pub(super) fn snapshot_message(
    publication: &axiusflow_market_engine::ConsumerPublication,
) -> envelope::Payload {
    envelope::Payload::SeriesSnapshot(IpcSeriesSnapshot {
        consumer_id: publication.consumer_id.0.get(),
        generation: publication.generation.0.get(),
        series: Some(ipc_series(&publication.snapshot.series)),
        provider_generation: publication.snapshot.provider_generation.0.get(),
        price_scale: u32::from(publication.snapshot.price_scale),
        quantity_scale: u32::from(publication.snapshot.quantity_scale),
        bars: publication
            .snapshot
            .bars
            .iter()
            .copied()
            .map(ipc_bar)
            .collect(),
        publication_generation: publication.publication_generation,
        forming: publication.snapshot.forming,
    })
}

pub(super) fn series_update_message(
    publication: &axiusflow_market_engine::ConsumerSeriesUpdate,
) -> envelope::Payload {
    envelope::Payload::SeriesUpdate(axiusflow_engine_protocol::SeriesUpdate {
        consumer_id: publication.consumer_id.0.get(),
        generation: publication.generation.0.get(),
        series: Some(ipc_series(&publication.series)),
        provider_generation: publication.provider_generation.0.get(),
        bar: Some(ipc_bar(publication.bar)),
        forming: publication.forming,
        publication_generation: publication.publication_generation,
        operation: match publication.operation {
            SeriesTailOperation::Revise => SeriesUpdateOperation::ReviseTail,
            SeriesTailOperation::Append => SeriesUpdateOperation::AppendTail,
        } as i32,
    })
}

pub(super) fn order_flow_payload(
    publication: &axiusflow_market_engine::ConsumerOrderFlowPublication,
) -> envelope::Payload {
    match &publication.kind {
        OrderFlowPublicationKind::Snapshot(snapshot) => {
            envelope::Payload::OrderFlowSnapshot(IpcOrderFlowSnapshot {
                consumer_id: publication.consumer_id.0.get(),
                generation: publication.generation.0.get(),
                series: Some(ipc_series(&publication.series)),
                provider_generation: snapshot.provider_generation.0.get(),
                publication_generation: publication.publication_generation,
                source_watermark: snapshot.source_watermark,
                cumulative_delta: snapshot.cumulative_delta,
                levels: snapshot
                    .levels
                    .iter()
                    .copied()
                    .map(ipc_order_flow_level)
                    .collect(),
                tape: snapshot
                    .tape
                    .iter()
                    .copied()
                    .map(ipc_order_flow_trade)
                    .collect(),
            })
        }
        OrderFlowPublicationKind::Update(update) => {
            envelope::Payload::OrderFlowUpdate(IpcOrderFlowUpdate {
                consumer_id: publication.consumer_id.0.get(),
                generation: publication.generation.0.get(),
                series: Some(ipc_series(&publication.series)),
                provider_generation: update.provider_generation.0.get(),
                publication_generation: publication.publication_generation,
                cumulative_delta: update.cumulative_delta,
                level: Some(ipc_order_flow_level(update.level)),
                trade: Some(ipc_order_flow_trade(update.trade)),
            })
        }
    }
}

pub(super) fn ipc_order_flow_level(
    level: axiusflow_market_engine::OrderFlowLevel,
) -> IpcOrderFlowLevel {
    IpcOrderFlowLevel {
        price: level.price,
        bid_volume: level.bid_volume,
        ask_volume: level.ask_volume,
        trade_count: level.trade_count,
        time_at_price_count: level.time_at_price_count,
    }
}

pub(super) fn ipc_order_flow_trade(
    trade: axiusflow_market_engine::OrderFlowTrade,
) -> IpcOrderFlowTrade {
    IpcOrderFlowTrade {
        source_sequence: trade.source_sequence,
        exchange_timestamp_unix_nanos: trade.exchange_timestamp_unix_nanos,
        price: trade.price,
        quantity: trade.quantity,
        aggressor: match trade.aggressor {
            axiusflow_market_data::AggressorSide::Unknown => OrderFlowAggressor::Unknown,
            axiusflow_market_data::AggressorSide::Buy => OrderFlowAggressor::Buy,
            axiusflow_market_data::AggressorSide::Sell => OrderFlowAggressor::Sell,
        } as i32,
    }
}

pub(super) fn order_book_snapshot(
    consumer_id: ConsumerId,
    generation: GenerationId,
    order_book: &ProviderOrderBook,
) -> envelope::Payload {
    let publication = order_book.book.publication();
    let provider_generation = if publication.session_generation == 0 {
        order_book.instrument.session_generation
    } else {
        publication.session_generation
    };
    envelope::Payload::OrderBookSnapshot(IpcOrderBookSnapshot {
        consumer_id: consumer_id.0.get(),
        generation: generation.0.get(),
        provider: order_book.instrument.provider.clone(),
        instrument_id: order_book.instrument.instrument_id.clone(),
        entitlement_id: order_book.instrument.entitlement_id.clone(),
        provider_generation,
        selection_generation: order_book.instrument.selection_generation,
        revision: publication.revision,
        source_watermark: publication.source_watermark,
        state: ipc_order_book_state(publication.state) as i32,
        bids: ipc_order_book_levels(&publication.bids, &order_book.traded_volumes),
        asks: ipc_order_book_levels(&publication.asks, &order_book.traded_volumes),
    })
}

pub(super) fn ipc_order_book_levels(
    levels: &[DepthLevel],
    traded_volumes: &BTreeMap<i64, i64>,
) -> Vec<IpcOrderBookLevel> {
    levels
        .iter()
        .map(|level| IpcOrderBookLevel {
            price: level.price,
            quantity: level.quantity,
            order_count: level.order_count,
            traded_volume: traded_volumes.get(&level.price).copied().unwrap_or(0),
        })
        .collect()
}

pub(super) const fn ipc_order_book_state(state: CanonicalOrderBookState) -> IpcOrderBookState {
    match state {
        CanonicalOrderBookState::Ready => IpcOrderBookState::Ready,
        CanonicalOrderBookState::Stale => IpcOrderBookState::Stale,
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot) => {
            IpcOrderBookState::AwaitingSnapshot
        }
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap) => {
            IpcOrderBookState::SequenceGap
        }
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::CrossedBook) => {
            IpcOrderBookState::CrossedBook
        }
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::InvalidUpdate) => {
            IpcOrderBookState::InvalidUpdate
        }
    }
}

pub(super) fn series_state(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: SeriesKey,
    state: SeriesLoadState,
    detail: Option<String>,
) -> envelope::Payload {
    series_state_with_persistence(
        consumer_id,
        generation,
        series,
        state,
        PersistenceState::NotRequested,
        detail,
    )
}

pub(super) fn series_state_with_persistence(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: SeriesKey,
    state: SeriesLoadState,
    persistence: PersistenceState,
    detail: Option<String>,
) -> envelope::Payload {
    envelope::Payload::SeriesState(SeriesState {
        consumer_id: consumer_id.0.get(),
        generation: generation.0.get(),
        series: Some(series),
        state: state as i32,
        persistence: persistence as i32,
        detail,
    })
}

impl ConsumerEvents {
    pub(super) fn front(&self) -> Option<&envelope::Payload> {
        self.provider
            .as_ref()
            .or_else(|| self.series.front())
            .or(self.series_state.as_ref())
            .or(self.demand_error.as_ref())
            .or(self.order_book.as_ref())
            .or(self.order_flow.as_ref())
            .or(self.catalog_selection.as_ref())
            .or(self.catalog_search.as_ref())
    }

    pub(super) fn pop(&mut self) -> Option<envelope::Payload> {
        self.provider
            .take()
            .or_else(|| self.series.pop_front())
            .or_else(|| self.series_state.take())
            .or_else(|| self.demand_error.take())
            .or_else(|| self.order_book.take())
            .or_else(|| self.order_flow.take())
            .or_else(|| self.catalog_selection.take())
            .or_else(|| self.catalog_search.take())
    }

    /// Queues one covering snapshot, discarding everything it already covers.
    pub(super) fn publish_snapshot(&mut self, snapshot: envelope::Payload) {
        self.series.clear();
        self.series_overflowed = false;
        self.series.push_back(snapshot);
    }

    pub(super) fn clear_series(&mut self) {
        self.series.clear();
        self.series_overflowed = false;
    }

    #[cfg(test)]
    pub(super) fn queued_series(&self) -> Option<&envelope::Payload> {
        self.series.front()
    }

    /// Queues one incremental bar update.
    ///
    /// Two foldings keep the queue short without losing a bar: repeated updates
    /// to the same forming bucket carry the same sequence, so the newest fully
    /// supersedes the queued one; and an update that extends a queued snapshot
    /// by exactly one bar is folded into it. Anything else is appended.
    pub(super) fn publish_series_update(&mut self, update: envelope::Payload) {
        let envelope::Payload::SeriesUpdate(next) = update else {
            return;
        };
        match self.series.back_mut() {
            Some(envelope::Payload::SeriesUpdate(queued))
                if queued.consumer_id == next.consumer_id
                    && queued.generation == next.generation
                    && queued.series == next.series
                    && next.operation == SeriesUpdateOperation::ReviseTail as i32
                    && queued.bar.map(|bar| bar.source_sequence)
                        == next.bar.map(|bar| bar.source_sequence) =>
            {
                let operation = queued.operation;
                *queued = next;
                queued.operation = operation;
                return;
            }
            Some(envelope::Payload::SeriesSnapshot(snapshot))
                if snapshot.consumer_id == next.consumer_id
                    && snapshot.generation == next.generation
                    && snapshot.series == next.series =>
            {
                if let Some(bar) = next.bar.as_ref() {
                    let folded = match snapshot.bars.last_mut() {
                        Some(current)
                            if next.operation == SeriesUpdateOperation::ReviseTail as i32
                                && current.source_sequence == bar.source_sequence =>
                        {
                            *current = *bar;
                            true
                        }
                        Some(current)
                            if next.operation == SeriesUpdateOperation::AppendTail as i32
                                && current.source_sequence.checked_add(1)
                                    == Some(bar.source_sequence) =>
                        {
                            snapshot.bars.push(*bar);
                            true
                        }
                        _ => false,
                    };
                    if folded {
                        snapshot.provider_generation = next.provider_generation;
                        snapshot.publication_generation = next.publication_generation;
                        snapshot.forming = next.forming;
                        return;
                    }
                }
            }
            _ => {}
        }
        if self.series.len() >= CONSUMER_SERIES_QUEUE_CAPACITY {
            // Everything queued is about to be superseded by a covering
            // snapshot, so the newest update is kept and the rest discarded.
            // Silently dropping the oldest instead leaves a hole in the middle
            // of a strictly sequenced run, which the consumer cannot detect
            // until the following bar fails to continue it.
            self.series.clear();
            self.series_overflowed = true;
        }
        self.series.push_back(envelope::Payload::SeriesUpdate(next));
    }
}

impl Coordinator<'_> {
    pub(super) fn handle_attach(
        &mut self,
        client_id: ClientId,
        events: Option<SyncSender<(u64, envelope::Payload)>>,
        reply: &Reply<()>,
    ) {
        let result = self
            .attached
            .insert(client_id)
            .then_some(())
            .ok_or_else(|| "client identity is already attached".to_string());
        if result.is_ok()
            && let Some(events) = events
        {
            self.attached_sinks.insert(client_id, events);
        }
        let _ = reply.send(result);
    }

    pub(super) fn flush_attached_events(&mut self) {
        let consumers = self.events.keys().copied().collect::<Vec<_>>();
        let mut disconnected = BTreeSet::new();
        let mut remaining = REALTIME_DRAIN_BUDGET;
        for consumer_id in consumers {
            if remaining == 0 {
                break;
            }
            let Some(&client_id) = self.consumer_clients.get(&consumer_id) else {
                continue;
            };
            let Some(sender) = self.attached_sinks.get(&client_id).cloned() else {
                continue;
            };
            let Some(events) = self.events.get_mut(&consumer_id) else {
                continue;
            };
            while remaining > 0 {
                let Some(event) = events.front().cloned() else {
                    break;
                };
                match sender.try_send((consumer_id.0.get(), event)) {
                    Ok(()) => {
                        let _ = events.pop();
                        remaining -= 1;
                    }
                    Err(TrySendError::Full(_)) => break,
                    Err(TrySendError::Disconnected(_)) => {
                        disconnected.insert(client_id);
                        break;
                    }
                }
            }
        }
        for client_id in disconnected {
            self.attached.remove(&client_id);
            self.attached_sinks.remove(&client_id);
            self.detach_client(client_id);
        }
    }

    pub(super) fn handle_poll(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        reply: &Reply<Option<envelope::Payload>>,
    ) {
        let result = authorize_consumer(&self.engine, client_id, consumer_id).map(|()| {
            self.events
                .get_mut(&consumer_id)
                .and_then(ConsumerEvents::pop)
        });
        let _ = reply.send(result);
    }

    pub(super) fn publish_cached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        waiter: &DemandWaiter,
        publication: &axiusflow_market_engine::ConsumerPublication,
    ) -> Result<(), String> {
        let needs_covering_repair =
            self.prepare_cached_demand(series, provider_generation, &publication.snapshot)?;
        let coinbase_history = self.live.get(series).map(|live| live.history);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            if needs_covering_repair && series.provider_id == "coinbase" {
                // A dormant Coinbase series can belong to the current provider
                // session while still ending several buckets behind the market.
                // Keep it inside the engine as fallback, but do not establish a
                // chart baseline until provider history has repaired the edge.
                events.series_state = Some(series_state(
                    waiter.consumer_id,
                    waiter.generation,
                    ipc_series(series),
                    SeriesLoadState::Resolving,
                    Some("Refreshing cached history to the Coinbase live edge".to_string()),
                ));
            } else if needs_covering_repair {
                publish_state(
                    events,
                    publication,
                    SeriesLoadState::Partial,
                    PersistenceState::Durable,
                    Some("Showing retained local history while provider coverage repairs"),
                );
            } else {
                // A second pane joining the same live calendar series must see
                // the same usable provisional state as its first consumer.
                publish_current_state(
                    events,
                    publication,
                    coinbase_history,
                    PersistenceState::Durable,
                );
            }
        }
        // A cached snapshot is the consumer's covering baseline. If the shared
        // series is already live, readiness must be announced after that
        // baseline is queued; publishing `Ready` after `Live` overwrites the
        // live transition and leaves a newly split pane stuck at connecting.
        if !needs_covering_repair {
            self.series_live_if_ready(series);
        }
        Ok(())
    }

    pub(super) fn flush_deferred_publication(&mut self, series: &BarSeriesKey) {
        if !self.deferred_publications.remove(series) {
            return;
        }
        match self.engine.publish_series_snapshot(series) {
            Ok(publications) => {
                let coinbase_history = self.live.get(series).map(|live| live.history);
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_current_state(
                            events,
                            &publication,
                            coinbase_history,
                            PersistenceState::Pending,
                        );
                    }
                }
            }
            Err(error) => {
                eprintln!("Axiusflow engine deferred history publication failed: {error}");
            }
        }
    }

    /// Resolves one failed Coinbase repair page without stranding the live feed.
    ///
    /// The live handoff is never held open for a repair, so a page the provider
    /// refuses to serve costs the consumer visible history and nothing else: the
    /// series is reported `Partial` and keeps receiving bars.
    pub(super) fn broadcast_persistence_for(
        &mut self,
        selected: &BarSeriesKey,
        persistence: PersistenceState,
        detail: Option<&str>,
    ) {
        let current_provider_generation = self
            .engine
            .provider_status(&selected.provider_id)
            .and_then(|status| status.generation)
            .unwrap_or(ProviderGeneration(NonZeroU64::MIN));
        let local_loaded = &self.local_loaded;
        let live = &self.live;
        let engine = &self.engine;
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                let (state, detail) =
                    if local_loaded.contains(&(series.clone(), current_provider_generation)) {
                        (SeriesLoadState::Partial, detail.map(str::to_string))
                    } else if live
                        .get(series)
                        .is_some_and(|live| live.connected && live.history.is_authoritative())
                    {
                        (SeriesLoadState::Live, detail.map(str::to_string))
                    } else if live
                        .get(series)
                        .is_some_and(|live| live.connected && live.history.is_ready())
                    {
                        (
                            SeriesLoadState::Partial,
                            Some(COINBASE_PROVISIONAL_DETAIL.to_string()),
                        )
                    } else if engine.has_publication(*consumer_id) {
                        (SeriesLoadState::Ready, detail.map(str::to_string))
                    } else {
                        (SeriesLoadState::Resolving, detail.map(str::to_string))
                    };
                events.series_state = Some(series_state_with_persistence(
                    *consumer_id,
                    generation,
                    ipc_series(series),
                    state,
                    persistence,
                    detail,
                ));
            }
        }
    }

    pub(super) fn broadcast_demand_error_for(
        &mut self,
        selected: &BarSeriesKey,
        stage: FailureStage,
        detail: &str,
        elapsed_millis: Option<u64>,
    ) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                events.demand_error = Some(envelope::Payload::DemandError(DemandError {
                    consumer_id: consumer_id.0.get(),
                    generation: generation.0.get(),
                    code: EngineFaultCode::Retryable as i32,
                    stage: failure_stage_name(stage).to_string(),
                    detail: detail.to_string(),
                    series: Some(ipc_series(series)),
                    stage_code: stage as i32,
                    cause: failure_stage_cause(stage).to_string(),
                    elapsed_millis,
                }));
            }
        }
    }

    pub(super) fn broadcast_series_resolution_for(
        &mut self,
        selected: &BarSeriesKey,
        state: SeriesLoadState,
        persistence: PersistenceState,
        detail: Option<&str>,
    ) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                events.series_state = Some(series_state_with_persistence(
                    *consumer_id,
                    generation,
                    ipc_series(series),
                    state,
                    persistence,
                    detail.map(str::to_string),
                ));
            }
        }
    }

    pub(super) fn publish_order_book_to_consumer(&mut self, consumer_id: ConsumerId) {
        let Some(demand) = self.engine.current_demand(consumer_id) else {
            return;
        };
        let Some(series) = demand.series.as_ref() else {
            return;
        };
        let Some(generation) = demand.generation else {
            return;
        };
        let Some(order_book) = self
            .order_books
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
        else {
            return;
        };
        if order_book.instrument.entitlement_id != series.entitlement_id {
            return;
        }
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.order_book = Some(order_book_snapshot(consumer_id, generation, order_book));
        }
    }

    pub(super) fn broadcast_order_book(&mut self, provider: &str, instrument_id: &str) {
        let consumers = self
            .events
            .keys()
            .filter_map(|consumer_id| {
                let demand = self.engine.current_demand(*consumer_id)?;
                let generation = demand.generation?;
                let series = demand.series.as_ref()?;
                (series.provider_id == provider && series.instrument_id == instrument_id)
                    .then_some((*consumer_id, generation))
            })
            .collect::<Vec<_>>();
        let Some(order_book) = self
            .order_books
            .get(&(provider.to_string(), instrument_id.to_string()))
        else {
            return;
        };
        for (consumer_id, generation) in consumers {
            if let Some(events) = self.events.get_mut(&consumer_id) {
                events.order_book = Some(order_book_snapshot(consumer_id, generation, order_book));
            }
        }
    }

    pub(super) fn recover_overflowed_series_queues(&mut self) {
        let overflowed = self
            .events
            .iter()
            .filter(|(_, events)| events.series_overflowed)
            .map(|(consumer_id, _)| *consumer_id)
            .collect::<Vec<_>>();
        for consumer_id in overflowed {
            let series = self
                .engine
                .current_demand(consumer_id)
                .and_then(|demand| demand.series.clone());
            let Some(series) = series else {
                if let Some(events) = self.events.get_mut(&consumer_id) {
                    events.clear_series();
                }
                continue;
            };
            match self.engine.publish_series_snapshot(&series) {
                Ok(publications) => {
                    for publication in publications
                        .iter()
                        .filter(|publication| publication.consumer_id == consumer_id)
                    {
                        if let Some(events) = self.events.get_mut(&consumer_id) {
                            events.publish_snapshot(snapshot_message(publication));
                        }
                    }
                }
                Err(error) => {
                    eprintln!("Axiusflow engine series-queue overflow recovery failed: {error}");
                }
            }
            if let Some(events) = self.events.get_mut(&consumer_id) {
                events.series_overflowed = false;
            }
        }
    }

    /// Publishes every Coinbase series that has bars the consumer has not seen.
    ///
    /// A series whose viewport sits back in history still publishes: the bars go
    /// into the canonical series and the consumer's own viewport decides what is
    /// drawn. Withholding them used to stall the feed permanently, because
    /// nothing re-armed publication when the viewport came back to the edge.
    pub(super) fn broadcast_provider_for(
        &mut self,
        provider: &str,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        let payload = envelope::Payload::ProviderState(ProviderState {
            provider: provider.to_string(),
            state: state as i32,
            generation: generation.0.get(),
            detail: detail.map(str::to_string),
        });
        for (consumer_id, events) in &mut self.events {
            if self
                .engine
                .current_demand(*consumer_id)
                .and_then(|demand| demand.series.as_ref())
                .is_some_and(|series| series.provider_id == provider)
            {
                events.provider = Some(payload.clone());
            }
        }
    }

    pub(super) fn broadcast_provider(
        &mut self,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        self.broadcast_provider_for("coinbase", state, generation, detail);
    }

    #[cfg(test)]
    pub(super) fn broadcast_rithmic_provider(
        &mut self,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        self.broadcast_provider_for("rithmic", state, generation, detail);
    }

    pub(super) fn broadcast_series_state_for(
        &mut self,
        selected: &BarSeriesKey,
        state: SeriesLoadState,
    ) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                events.series_state = Some(series_state(
                    *consumer_id,
                    generation,
                    ipc_series(series),
                    state,
                    None,
                ));
            }
        }
    }

    pub(super) fn broadcast_series_recovery_for(&mut self, selected: &BarSeriesKey, detail: &str) {
        let state = if self.engine.series_snapshot(selected).is_some() {
            SeriesLoadState::Partial
        } else {
            SeriesLoadState::Resolving
        };
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series != selected {
                continue;
            }
            let persistence = match events.series_state.as_ref() {
                Some(envelope::Payload::SeriesState(current)) => {
                    PersistenceState::try_from(current.persistence)
                        .unwrap_or(PersistenceState::NotRequested)
                }
                _ => PersistenceState::NotRequested,
            };
            events.series_state = Some(series_state_with_persistence(
                *consumer_id,
                generation,
                ipc_series(series),
                state,
                persistence,
                Some(detail.to_string()),
            ));
        }
    }
}
