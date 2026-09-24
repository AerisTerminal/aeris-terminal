use super::{
    BTreeMap, BarSeriesKey, CONSUMER_SERIES_QUEUE_CAPACITY, ClientId, ConsumerEvents, ConsumerId,
    Coordinator, DemandWaiter, EngineError, EngineFaultCode, FailureStage, GenerationId,
    ProviderGeneration, ProviderOrderBook, REALTIME_DRAIN_BUDGET, Reply, SeriesLoadState,
    SeriesTailOperation, authorize_consumer,
};
use crate::{
    MarketDemandError, MarketOrderBookSnapshot, MarketRuntimeEvent, MarketSeriesState,
    MarketStudyOutputsInvalidated, MarketStudyRemoved,
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
                series.clone(),
                SeriesLoadState::Failed,
                Some(detail.to_string()),
            ));
            events.demand_error = Some(MarketRuntimeEvent::DemandError(MarketDemandError {
                consumer_id: waiter.consumer_id,
                generation: waiter.generation,
                code: EngineFaultCode::Retryable,
                stage,
                detail: detail.to_string(),
                series: Some(series.clone()),
                cause: failure_stage_cause(stage).to_string(),
                elapsed_millis: Some(
                    u64::try_from(waiter.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
            }));
        }
    }
}

pub(super) const fn failure_stage_cause(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::ProviderHistory => "provider history did not produce usable canonical bars",
        FailureStage::CanonicalValidation => "market data failed canonical validation",
        FailureStage::MemoryInstall => "validated market data could not enter bounded memory",
        FailureStage::Aggregation => "canonical events could not be aggregated",
        FailureStage::Handoff => "history and realtime state could not be joined safely",
        FailureStage::Publication => "validated market state could not be published",
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

pub(super) fn publish_ready(
    events: &mut ConsumerEvents,
    publication: &aeris_market_engine::ConsumerPublication,
) {
    publish_state(events, publication, SeriesLoadState::Ready, None);
}

pub(super) fn publish_state(
    events: &mut ConsumerEvents,
    publication: &aeris_market_engine::ConsumerPublication,
    state: SeriesLoadState,
    detail: Option<&str>,
) {
    events.publish_snapshot(snapshot_message(publication));
    events.series_state = Some(series_state_payload(
        publication.consumer_id,
        publication.generation,
        publication.snapshot.series.clone(),
        state,
        detail.map(str::to_string),
    ));
}

pub(super) fn snapshot_message(
    publication: &aeris_market_engine::ConsumerPublication,
) -> MarketRuntimeEvent {
    MarketRuntimeEvent::SeriesSnapshot(publication.clone())
}

pub(super) fn series_update_message(
    publication: &aeris_market_engine::ConsumerSeriesUpdate,
) -> MarketRuntimeEvent {
    MarketRuntimeEvent::SeriesUpdate(publication.clone())
}

pub(super) fn order_book_snapshot(
    consumer_id: ConsumerId,
    generation: GenerationId,
    order_book: &ProviderOrderBook,
    display_depth: Option<&crate::MarketDisplayDepth>,
) -> MarketRuntimeEvent {
    let mut publication = order_book.book.publication();
    if publication.provider_id.is_empty() {
        publication
            .provider_id
            .clone_from(&order_book.instrument.provider);
        publication
            .instrument_id
            .clone_from(&order_book.instrument.instrument_id);
        publication
            .entitlement_id
            .clone_from(&order_book.instrument.entitlement_id);
        publication.session_generation = order_book.instrument.session_generation;
    }
    let provider_generation = publication.session_generation;
    if order_book.trade_session_generation == provider_generation {
        publication
            .traded_volumes
            .clone_from(&order_book.traded_volumes);
        publication.trade_source_watermark = order_book.last_trade_source_sequence;
    }
    if let Some(quote) = order_book
        .top_of_book
        .as_ref()
        .filter(|quote| quote.metadata.session_generation == provider_generation)
    {
        publication.best_bid = quote.bid;
        publication.best_ask = quote.ask;
        publication.bbo_source_watermark = quote.metadata.source_sequence;
    } else {
        // Providers without a separate BBO update retained yet still have an
        // authoritative best price in the complete L2 snapshot. Expose that
        // top level as BBO metadata without fabricating any additional depth.
        // A quote from another provider session cannot override this image.
        publication.best_bid = publication.bids.first().copied();
        publication.best_ask = publication.asks.first().copied();
        publication.bbo_source_watermark = publication.source_watermark;
    }
    MarketRuntimeEvent::OrderBookSnapshot(MarketOrderBookSnapshot {
        consumer_id,
        generation,
        publication,
        display_depth: display_depth
            .filter(|display| display.provider_generation == provider_generation)
            .cloned(),
    })
}

pub(super) fn series_state(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: BarSeriesKey,
    state: SeriesLoadState,
    detail: Option<String>,
) -> MarketRuntimeEvent {
    series_state_payload(consumer_id, generation, series, state, detail)
}

pub(super) fn series_state_payload(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: BarSeriesKey,
    state: SeriesLoadState,
    detail: Option<String>,
) -> MarketRuntimeEvent {
    MarketRuntimeEvent::SeriesState(MarketSeriesState {
        consumer_id,
        generation,
        series: Some(series),
        state,
        detail,
    })
}

impl ConsumerEvents {
    pub(super) fn pop(&mut self) -> Option<MarketRuntimeEvent> {
        self.provider
            .take()
            .or_else(|| self.series.pop_front())
            .or_else(|| self.study_invalidated.take())
            .or_else(|| self.study_outputs.pop_first().map(|(_, event)| event))
            .or_else(|| self.study_removed.take())
            .or_else(|| self.series_state.take())
            .or_else(|| self.demand_error.take())
            .or_else(|| self.price_alerts.pop_front())
            .or_else(|| self.order_book.take())
            .or_else(|| self.catalog_selection.take())
            .or_else(|| self.catalog_search.take())
    }

    /// Queues one covering snapshot, discarding everything it already covers.
    pub(super) fn publish_snapshot(&mut self, snapshot: MarketRuntimeEvent) {
        self.series.clear();
        self.series_overflowed = false;
        self.series.push_back(snapshot);
    }

    pub(super) fn clear_series(&mut self) {
        self.series.clear();
        self.series_overflowed = false;
    }

    /// Keeps only the newest immutable publication for one study output.
    pub(super) fn publish_study_output(&mut self, event: MarketRuntimeEvent) {
        let MarketRuntimeEvent::StudyOutputSnapshot(snapshot) = &event else {
            return;
        };
        self.study_outputs.insert(snapshot.output_id, event);
    }

    /// Removes queued output images for a deleted subtree and merges the
    /// presentation removal notice with any earlier unconsumed removal.
    pub(super) fn publish_study_removed(
        &mut self,
        consumer_id: ConsumerId,
        removed: &[super::StudyInstanceId],
    ) {
        if removed.is_empty() {
            return;
        }
        let removed_set = removed
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        self.study_outputs
            .retain(|output, _| !removed_set.contains(&output.study_id));

        let mut study_ids = match self.study_removed.take() {
            Some(MarketRuntimeEvent::StudyRemoved(previous))
                if previous.consumer_id == consumer_id =>
            {
                previous.study_ids
            }
            _ => Vec::new(),
        };
        study_ids.extend(removed.iter().copied());
        study_ids.sort_unstable();
        study_ids.dedup();
        self.study_removed = Some(MarketRuntimeEvent::StudyRemoved(MarketStudyRemoved {
            consumer_id,
            study_ids,
        }));
    }

    /// Clears queued output images for a reinitialized subtree while preserving
    /// its live runtime definitions and merges consecutive invalidations.
    pub(super) fn publish_study_invalidated(
        &mut self,
        consumer_id: ConsumerId,
        invalidated: &[super::StudyInstanceId],
    ) {
        if invalidated.is_empty() {
            return;
        }
        let invalidated_set = invalidated
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        self.study_outputs
            .retain(|output, _| !invalidated_set.contains(&output.study_id));
        let mut study_ids = match self.study_invalidated.take() {
            Some(MarketRuntimeEvent::StudyOutputsInvalidated(previous))
                if previous.consumer_id == consumer_id =>
            {
                previous.study_ids
            }
            _ => Vec::new(),
        };
        study_ids.extend(invalidated.iter().copied());
        study_ids.sort_unstable();
        study_ids.dedup();
        self.study_invalidated = Some(MarketRuntimeEvent::StudyOutputsInvalidated(
            MarketStudyOutputsInvalidated {
                consumer_id,
                study_ids,
            },
        ));
    }

    /// Queues the newest trigger per alert identity. This preserves every
    /// distinct alert while bounding repeated crossings during a slow UI frame.
    pub(super) fn publish_price_alert(&mut self, event: MarketRuntimeEvent) {
        let MarketRuntimeEvent::PriceAlertTriggered(next) = event else {
            return;
        };
        if let Some(queued) = self.price_alerts.iter_mut().find(|queued| {
            matches!(
                queued,
                MarketRuntimeEvent::PriceAlertTriggered(current)
                    if current.alert_id == next.alert_id
            )
        }) {
            *queued = MarketRuntimeEvent::PriceAlertTriggered(next);
        } else {
            self.price_alerts
                .push_back(MarketRuntimeEvent::PriceAlertTriggered(next));
        }
    }

    /// Queues one incremental bar update.
    ///
    /// Repeated revisions of the same forming bucket fully supersede one
    /// another. Covering snapshots stay immutable and shared; a following tail
    /// is intentionally queued separately rather than copying the whole Arc
    /// snapshot just to fold one bar into it.
    pub(super) fn publish_series_update(&mut self, update: MarketRuntimeEvent) {
        let MarketRuntimeEvent::SeriesUpdate(next) = update else {
            return;
        };
        match self.series.back_mut() {
            Some(MarketRuntimeEvent::SeriesUpdate(queued))
                if queued.consumer_id == next.consumer_id
                    && queued.generation == next.generation
                    && queued.series == next.series
                    && next.operation == SeriesTailOperation::Revise
                    && queued.bar.source_sequence == next.bar.source_sequence =>
            {
                let operation = queued.operation;
                *queued = next;
                queued.operation = operation;
                return;
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
        self.series
            .push_back(MarketRuntimeEvent::SeriesUpdate(next));
    }
}

impl Coordinator<'_> {
    pub(super) fn canonical_series_load_state(
        &self,
        consumer_id: ConsumerId,
        series: &BarSeriesKey,
    ) -> SeriesLoadState {
        if self.rithmic_live.get(series).is_some_and(|live| {
            live.connected && live.history_state == super::LiveHistoryState::Ready
        }) || self.hyperliquid_live.get(series).is_some_and(|live| {
            live.connected && live.history_state == super::LiveHistoryState::Ready
        }) {
            SeriesLoadState::Live
        } else if self.engine.has_publication(consumer_id) {
            SeriesLoadState::Ready
        } else {
            SeriesLoadState::Resolving
        }
    }

    pub(super) fn publish_current_snapshot(
        &mut self,
        publication: &aeris_market_engine::ConsumerPublication,
    ) {
        let state =
            self.canonical_series_load_state(publication.consumer_id, &publication.snapshot.series);
        let Some(events) = self.events.get_mut(&publication.consumer_id) else {
            return;
        };
        publish_state(events, publication, state, None);
    }

    pub(super) fn handle_attach(&mut self, client_id: ClientId, reply: &Reply<()>) {
        let result = self
            .attached
            .insert(client_id)
            .then_some(())
            .ok_or_else(|| "client identity is already attached".to_string());
        let _ = reply.send(result);
    }

    pub(super) fn handle_poll_client(
        &mut self,
        client_id: ClientId,
        consumer_budgets: Vec<(ConsumerId, usize)>,
        reply: &Reply<Vec<(u64, MarketRuntimeEvent)>>,
    ) {
        if !self.attached.contains(&client_id) {
            let _ = reply.send(Err("market client is not attached".to_string()));
            return;
        }
        if consumer_budgets
            .iter()
            .any(|(consumer_id, _)| self.consumer_clients.get(consumer_id) != Some(&client_id))
        {
            let _ = reply.send(Err("market consumer is not owned by client".to_string()));
            return;
        }
        let mut consumer_budgets = consumer_budgets
            .into_iter()
            .filter(|(_, maximum)| *maximum > 0)
            .fold(BTreeMap::new(), |mut budgets, (consumer_id, maximum)| {
                budgets
                    .entry(consumer_id)
                    .and_modify(|current: &mut usize| *current = (*current).max(maximum))
                    .or_insert(maximum.min(REALTIME_DRAIN_BUDGET));
                budgets
            });
        let requested = consumer_budgets
            .values()
            .copied()
            .fold(0_usize, usize::saturating_add)
            .min(REALTIME_DRAIN_BUDGET);
        let mut batch = Vec::with_capacity(requested);
        let mut remaining = requested;
        while remaining > 0 {
            let mut progressed = false;
            for (consumer_id, budget) in &mut consumer_budgets {
                if *budget == 0 {
                    continue;
                }
                let Some(events) = self.events.get_mut(consumer_id) else {
                    continue;
                };
                let Some(event) = events.pop() else {
                    continue;
                };
                batch.push((consumer_id.0.get(), event));
                *budget -= 1;
                remaining -= 1;
                progressed = true;
                if remaining == 0 {
                    break;
                }
            }
            if !progressed {
                break;
            }
        }
        let _ = reply.send(Ok(batch));
    }

    pub(super) fn handle_poll(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        reply: &Reply<Option<MarketRuntimeEvent>>,
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
        publication: &aeris_market_engine::ConsumerPublication,
    ) -> Result<(), String> {
        let needs_covering_repair =
            self.prepare_cached_demand(series, provider_generation, &publication.snapshot)?;
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            if needs_covering_repair {
                publish_state(
                    events,
                    publication,
                    SeriesLoadState::Partial,
                    Some("Refreshing provider coverage"),
                );
            } else {
                publish_ready(events, publication);
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
                events.demand_error = Some(MarketRuntimeEvent::DemandError(MarketDemandError {
                    consumer_id: *consumer_id,
                    generation,
                    code: EngineFaultCode::Retryable,
                    stage,
                    detail: detail.to_string(),
                    series: Some(series.clone()),
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
                events.series_state = Some(series_state_payload(
                    *consumer_id,
                    generation,
                    series.clone(),
                    state,
                    detail.map(str::to_string),
                ));
            }
        }
    }

    pub(super) fn publish_order_book_to_consumer(&mut self, consumer_id: ConsumerId) {
        let Some(demand) = self.engine.current_demand(consumer_id) else {
            return;
        };
        if !demand.resource_class.publishes_ui() {
            return;
        }
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
        let display_depth = (series.provider_id == "hyperliquid")
            .then(|| self.hyperliquid_display_depth.get(&series.instrument_id))
            .flatten();
        let event = order_book_snapshot(consumer_id, generation, order_book, display_depth);
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.order_book = Some(event);
        }
    }

    pub(super) fn broadcast_order_book(&mut self, provider: &str, instrument_id: &str) {
        let consumers = self
            .events
            .keys()
            .filter_map(|consumer_id| {
                let demand = self.engine.current_demand(*consumer_id)?;
                if !demand.resource_class.publishes_ui() {
                    return None;
                }
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
        let display_depth = (provider == "hyperliquid")
            .then(|| self.hyperliquid_display_depth.get(instrument_id))
            .flatten();
        for (consumer_id, generation) in consumers {
            let event = order_book_snapshot(consumer_id, generation, order_book, display_depth);
            if let Some(events) = self.events.get_mut(&consumer_id) {
                events.order_book = Some(event);
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
                    eprintln!("Aeris engine series-queue overflow recovery failed: {error}");
                }
            }
            if let Some(events) = self.events.get_mut(&consumer_id) {
                events.series_overflowed = false;
            }
        }
    }

    pub(super) fn broadcast_provider_for(&mut self, provider: &str, detail: Option<&str>) {
        let Some(mut state) = self.provider_state(provider) else {
            return;
        };
        if let Some(detail) = detail {
            state.detail = Some(detail.to_string());
        }
        let payload = MarketRuntimeEvent::ProviderState(state);
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
            events.series_state = Some(series_state_payload(
                *consumer_id,
                generation,
                series.clone(),
                state,
                Some(detail.to_string()),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_book_publication_preserves_canonical_depth_beyond_the_old_ui_cutoff() {
        const LEVELS: usize = 64;
        let instrument = aeris_contracts::InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            provider_symbol: "MNQ".to_string(),
            display_symbol: "MNQ".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "rithmic-test:CME:MNQ".to_string(),
            price_increment: Some(25),
        };
        let mut order_book = ProviderOrderBook::new(instrument.clone());
        let snapshot = aeris_market_data::DepthSnapshot {
            metadata: aeris_market_data::EventMetadata {
                provider_id: instrument.provider.clone(),
                instrument_id: instrument.instrument_id.clone(),
                entitlement_id: instrument.entitlement_id.clone(),
                source_sequence: 1,
                session_generation: 1,
                timestamps: aeris_market_data::QualifiedTimestamp {
                    exchange_unix_nanos: Some(1),
                    provider_unix_nanos: None,
                    received_unix_nanos: 1,
                },
            },
            bids: (0..LEVELS)
                .map(|index| aeris_market_data::DepthLevel {
                    price: 20_000 - i64::try_from(index).expect("bounded index"),
                    quantity: 1,
                    order_count: Some(1),
                })
                .collect(),
            asks: (0..LEVELS)
                .map(|index| aeris_market_data::DepthLevel {
                    price: 20_001 + i64::try_from(index).expect("bounded index"),
                    quantity: 1,
                    order_count: Some(1),
                })
                .collect(),
        };
        assert!(matches!(
            order_book.book.install_snapshot(&snapshot),
            Ok(aeris_market_data::OrderBookApplyOutcome::Published)
        ));

        let display_depth = crate::MarketDisplayDepth {
            provider_generation: 1,
            display_generation: 3,
            source_sequence: 7,
            bids: vec![aeris_market_data::DepthLevel {
                price: 20_000,
                quantity: 5,
                order_count: Some(2),
            }],
            asks: vec![aeris_market_data::DepthLevel {
                price: 20_050,
                quantity: 6,
                order_count: Some(3),
            }],
        };
        let event = order_book_snapshot(
            ConsumerId(std::num::NonZeroU64::MIN),
            GenerationId(std::num::NonZeroU64::MIN),
            &order_book,
            Some(&display_depth),
        );
        let MarketRuntimeEvent::OrderBookSnapshot(snapshot) = event else {
            panic!("order-book publication expected");
        };
        assert_eq!(snapshot.publication.bids.len(), LEVELS);
        assert_eq!(snapshot.publication.asks.len(), LEVELS);
        assert_eq!(
            snapshot.publication.bids.last().map(|level| level.price),
            Some(19_937)
        );
        assert_eq!(
            snapshot.publication.asks.last().map(|level| level.price),
            Some(20_064)
        );
        assert_eq!(snapshot.display_depth, Some(display_depth));
    }
}
