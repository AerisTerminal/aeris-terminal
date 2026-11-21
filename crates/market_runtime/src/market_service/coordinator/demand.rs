use super::{
    Arc, BarSeriesKey, ClientId, ConsumerEvents, ConsumerId, ConsumerIdentity,
    ConsumerResourceClass, Coordinator, DemandWaiter, EngineError, GenerationId,
    MarketRuntimeEvent, MarketServiceStatus, MarketStream, ProviderConnectionState,
    ProviderGeneration, ProviderHealth, ProviderState, Reply, StreamRequirements,
    authorize_consumer, publish_state,
};

impl Coordinator<'_> {
    pub(super) fn replace_price_alerts(
        &mut self,
        consumer_id: ConsumerId,
        alerts: Vec<crate::MarketPriceAlert>,
    ) -> Result<(), String> {
        let mut replacement = self.price_alerts.clone();
        replacement.replace_consumer_alerts(consumer_id, alerts)?;
        let keys = replacement.active_keys()?;
        self.engine
            .set_instrument_leases(
                aeris_market_engine::LeaseOwner::PriceAlerts,
                keys,
                StreamRequirements::NONE
                    .with(MarketStream::Trades)
                    .with(MarketStream::Quotes),
            )
            .map_err(|error| error.to_string())?;
        self.price_alerts = replacement;
        self.reconcile_order_books();
        self.pending_activity = true;
        Ok(())
    }

    pub(in super::super) fn reconcile_price_alert_leases(&mut self) -> Result<(), String> {
        self.engine
            .set_instrument_leases(
                aeris_market_engine::LeaseOwner::PriceAlerts,
                self.price_alerts.active_keys()?,
                StreamRequirements::NONE
                    .with(MarketStream::Trades)
                    .with(MarketStream::Quotes),
            )
            .map_err(|error| error.to_string())?;
        self.reconcile_order_books();
        self.mark_alert_demand_changed();
        self.pending_activity = true;
        Ok(())
    }

    pub(super) fn handle_resource_class(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        resource_class: ConsumerResourceClass,
        reply: &Reply<()>,
    ) {
        let result = authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
            let mut needs_covering_repair = false;
            let retained_before = self
                .engine
                .current_demand(consumer_id)
                .is_some_and(|demand| demand.resource_class.retains_subscription());
            let publication = self
                .engine
                .set_resource_class(consumer_id, resource_class)
                .map_err(|error| error.to_string())?;
            if !retained_before
                && resource_class.retains_subscription()
                && let Some((series, streams)) = self
                    .engine
                    .current_demand(consumer_id)
                    .and_then(|demand| demand.series.clone().zip(demand.streams))
            {
                // Background consumers intentionally release provider demand and
                // `release_unused_live_market_data` prunes their live handoff.
                // When the same consumer returns to Foreground the engine restores
                // its subscription from retained demand; rebuild the runtime handoff
                // in the same transition so cached publication cannot leave the
                // chart detached from upstream live data.
                self.ensure_realtime(&series)?;
                if streams.contains(MarketStream::Bars) {
                    let provider_generation = self.provider_generation_for_series(&series)?;
                    if let Some(snapshot) = publication
                        .as_ref()
                        .map(|publication| Arc::clone(&publication.snapshot))
                        .or_else(|| self.engine.series_snapshot(&series))
                    {
                        needs_covering_repair =
                            self.prepare_cached_demand(&series, provider_generation, &snapshot)?;
                    } else if self
                        .engine
                        .provider_status(&series.provider_id)
                        .is_some_and(|status| status.health != ProviderHealth::Disconnected)
                    {
                        self.enqueue_history_recovery(&series, provider_generation)
                            .map_err(str::to_string)?;
                    }
                }
            }
            if let Some(publication) = publication {
                if needs_covering_repair {
                    let (state, detail) = self.covering_repair_state(&publication.snapshot.series);
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_state(events, &publication, state, Some(detail));
                    }
                } else {
                    self.publish_current_snapshot(&publication);
                }
            }
            if let Some(events) = self.events.get_mut(&consumer_id)
                && !resource_class.publishes_ui()
            {
                // A full snapshot queued just before a pane hid must not leak
                // through after the class change. Canonical depth remains
                // runtime-owned and is republished on Foreground restore.
                events.order_book = None;
                events.study_outputs.clear();
            }
            Ok(())
        });
        self.reconcile_order_books();
        if resource_class.publishes_ui() {
            self.publish_order_book_to_consumer(consumer_id);
            self.publish_trade_tape_to_consumer(consumer_id);
            let studies = self.studies.owned_studies(consumer_id);
            self.publish_study_outputs(&studies);
        }
        self.release_unused_live_market_data();
        let _ = reply.send(result);
    }

    pub(super) fn handle_streams(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        generation: GenerationId,
        streams: StreamRequirements,
        reply: &Reply<()>,
    ) {
        let result = authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
            let changed =
                match self
                    .engine
                    .set_stream_requirements(consumer_id, generation, streams)
                {
                    Ok(changed) => changed,
                    Err(EngineError::StaleConsumerGeneration { .. }) => return Ok(()),
                    Err(error) => return Err(error.to_string()),
                };
            if !changed {
                return Ok(());
            }
            let series = self
                .engine
                .current_demand(consumer_id)
                .and_then(|demand| demand.series.clone())
                .ok_or_else(|| "market consumer has no selected series".to_string())?;
            self.reconcile_order_books();
            if !streams.contains(MarketStream::Depth)
                && let Some(events) = self.events.get_mut(&consumer_id)
            {
                events.order_book = None;
            }
            if !streams.contains(MarketStream::Trades)
                && let Some(events) = self.events.get_mut(&consumer_id)
            {
                events.tape_updates.clear();
                events.tape_cursor = None;
            }
            self.ensure_realtime(&series)?;
            if streams.contains(MarketStream::Depth) || streams.contains(MarketStream::OrderLevels)
            {
                self.publish_order_book_to_consumer(consumer_id);
            }
            if streams.contains(MarketStream::Trades) {
                self.publish_trade_tape_to_consumer(consumer_id);
            }
            Ok(())
        });
        self.release_unused_live_market_data();
        let _ = reply.send(result);
    }

    pub(in super::super) fn handle_register(
        &mut self,
        identity: ConsumerIdentity,
    ) -> Result<(), String> {
        if !self.attached.contains(&identity.client_id) {
            return Err("client must attach before registering consumers".to_string());
        }
        self.engine
            .register_consumer(identity, true)
            .map_err(|error| error.to_string())?;
        self.events
            .insert(identity.consumer_id, ConsumerEvents::default());
        self.consumer_clients
            .insert(identity.consumer_id, identity.client_id);
        self.consumer_epochs
            .insert(identity.consumer_id, self.consumer_epoch_sequence.next());
        Ok(())
    }

    pub(in super::super) fn handle_remove(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
    ) -> Result<(), String> {
        authorize_consumer(&self.engine, client_id, consumer_id)?;
        let removed_studies = super::super::consumer_registry::ConsumerRegistry::new(self)
            .remove_consumer(consumer_id, true);
        if removed_studies {
            self.reconcile_study_market_data()?;
        }
        self.mark_alert_demand_changed();
        self.reconcile_order_books();
        for session in self.provider_registry.controls_mut() {
            session.demand_dirty = true;
        }
        self.release_unused_live_market_data();
        Ok(())
    }

    pub(in super::super) fn status(&self) -> MarketServiceStatus {
        let metrics = self.engine.metrics();
        MarketServiceStatus {
            connected_desktop_clients: self.attached.len(),
            study_execution_failed: self.studies.execution_isolated(),
            providers: self
                .providers
                .records
                .keys()
                .copied()
                .filter_map(|provider| self.provider_state(provider))
                .collect(),
            retained_series: metrics.stored_series,
            retained_bars: metrics.stored_bars,
            approximate_series_bytes: metrics.approximate_series_bytes,
            #[cfg(feature = "probe")]
            probe: {
                let (stale_events, stale_history) = self.provider_epochs.stale_counts();
                let mut provider_sessions = std::collections::BTreeMap::new();
                let mut retry_attempts = Vec::new();
                let now = self.clock.monotonic();
                for &provider_id in self.providers.records.keys() {
                    provider_sessions.insert(
                        provider_id.to_string(),
                        self.provider_registry.successful_connections(provider_id),
                    );
                    retry_attempts.extend(self.provider_registry.probe_retries(provider_id, now));
                }
                super::super::ProbeStatus {
                    provider_sessions,
                    stale_events,
                    stale_history,
                    maximum_queued_series_updates: self
                        .events
                        .values()
                        .map(|events| events.maximum_series_depth)
                        .max()
                        .unwrap_or(0),
                    retry_attempts,
                }
            },
        }
    }

    pub(in super::super) fn evict_unreferenced_series(&mut self) -> Vec<BarSeriesKey> {
        let protected = self.engine.demanded_series();
        self.engine.evict_unsubscribed_series(0, 0, &protected)
    }

    pub(in super::super) fn provider_state(&self, provider: &str) -> Option<ProviderState> {
        let status = self.engine.provider_status(provider)?;
        let state = match status.health {
            ProviderHealth::Disconnected => ProviderConnectionState::Disconnected,
            ProviderHealth::Connecting => ProviderConnectionState::Connecting,
            ProviderHealth::Online => ProviderConnectionState::Online,
            ProviderHealth::Recovering => ProviderConnectionState::Recovering,
            ProviderHealth::Failed => ProviderConnectionState::Failed,
        };
        Some(ProviderState {
            provider: provider.to_string(),
            state,
            generation: status.generation.map_or(0, |generation| generation.0.get()),
            detail: self.providers.detail(provider),
            transport_rtt_nanos: self.providers.transport_rtt_nanos(provider),
        })
    }

    pub(in super::super) fn accept_series_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        streams: StreamRequirements,
        waiter: &DemandWaiter,
    ) -> Result<
        (
            ProviderGeneration,
            Option<aeris_market_engine::ConsumerPublication>,
        ),
        String,
    > {
        authorize_consumer(&self.engine, client_id, waiter.consumer_id)?;
        if !self.ensure_provider_connection(&series.provider_id) {
            return Err("provider connection is not ready".into());
        }
        let provider_generation = self.provider_generation_for_series(series)?;
        if matches!(
            series.period,
            aeris_market_data::BarPeriod::Week { .. } | aeris_market_data::BarPeriod::Month { .. }
        ) && self
            .catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .is_some_and(|instrument| {
                self.calendar.source_kind(&instrument.instrument_id)
                    == aeris_instruments::CalendarSourceKind::Unknown
            })
        {
            let error = "weekly or monthly live series requires a supported instrument calendar";
            self.report_unsupported_calendar(waiter, series, error);
            return Err(error.into());
        }
        let publication = self
            .engine
            .set_series_demand_with_streams(waiter.consumer_id, waiter.generation, series, streams)
            .map_err(|error| error.to_string())?;
        self.reconcile_order_books();
        self.remove_waiter(waiter.consumer_id);
        let provider_state = self
            .provider_state(&series.provider_id)
            .map(MarketRuntimeEvent::ProviderState);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.clear_series();
            events.series_state = None;
            events.demand_error = None;
            // Demand can attach to an already-online provider session. Publish
            // its current state immediately instead of waiting for a transition.
            events.provider = provider_state;
        }
        Ok((provider_generation, publication))
    }

    pub(in super::super) fn handle_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        streams: StreamRequirements,
        waiter: DemandWaiter,
        reply: &Reply<()>,
    ) {
        let (provider_generation, publication) =
            match self.accept_series_demand(client_id, series, streams, &waiter) {
                Ok(accepted) => accepted,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
        self.prune_unused_live_series();
        self.prune_history_tracking();
        self.evict_unreferenced_series();
        let retains_subscription = self
            .engine
            .current_demand(waiter.consumer_id)
            .is_some_and(|demand| demand.resource_class.retains_subscription());
        if !retains_subscription {
            // Background/detached consumers retain their requested series identity
            // but intentionally own no upstream subscription. Park that demand
            // without starting provider/history work; a later Foreground resource
            // transition restores the engine subscription and rebuilds realtime
            // through `handle_resource_class`.
            self.release_idle();
            let _ = reply.send(Ok(()));
            return;
        }
        // The live handoff, and the bounded trade buffer inside it, must exist
        // before history starts. A provider history fetch can take tens of
        // seconds; deferring the handoff until it returned dropped every trade
        // that arrived in between, and left live handoff state empty across a
        // symbol switch, so the idle check below tore down the provider session
        // that a presentation change is never allowed to touch.
        let realtime = self.ensure_realtime(series);
        self.release_idle();
        if let Err(error) = realtime {
            let _ = reply.send(Err(error));
            return;
        }
        self.publish_order_book_to_consumer(waiter.consumer_id);
        let result = match publication {
            None => {
                if let Some(snapshot) = self.engine.series_snapshot(series) {
                    self.prepare_cached_demand(series, provider_generation, &snapshot)
                        .map(|_| ())
                } else {
                    self.start_uncached_demand(series, provider_generation, waiter);
                    Ok(())
                }
            }
            Some(publication) => {
                self.publish_cached_demand(series, provider_generation, &waiter, &publication)
            }
        };
        let _ = reply.send(result);
    }

    fn report_unsupported_calendar(
        &mut self,
        waiter: &DemandWaiter,
        series: &BarSeriesKey,
        detail: &str,
    ) {
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.demand_error = Some(MarketRuntimeEvent::DemandError(crate::MarketDemandError {
                consumer_id: waiter.consumer_id,
                generation: waiter.generation,
                scope: crate::MarketDemandErrorScope::Series,
                code: crate::EngineFaultCode::Permanent,
                stage: crate::FailureStage::Handoff,
                detail: detail.to_string(),
                series: Some(series.clone()),
                cause: "calendar unavailable for live series".into(),
                elapsed_millis: None,
            }));
        }
    }
}
