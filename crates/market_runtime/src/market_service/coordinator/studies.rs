use super::{
    BTreeMap, BarSeriesKey, ClientId, ConsumerId, Coordinator, MarketEngine, MarketRuntimeEvent,
    MarketStream, MarketStudyOutputSnapshot, ProviderHealth, ProviderOrderBook, SeriesLifecycles,
    StreamRequirements, StudyChange, StudyExecutionBatch, StudyLiveMarketData, StudyMarketInput,
    StudyMarketLeaseChangeKind, StudyRuntimeError, authorize_consumer,
};
use aeris_observability::diagnostic;

fn study_market_series_live_ready(
    engine: &MarketEngine,
    series_live: &SeriesLifecycles,
    input: &StudyMarketInput,
) -> bool {
    let Some(provider) = engine.provider_status(&input.series.provider_id) else {
        return false;
    };
    if provider.health != ProviderHealth::Online {
        return false;
    }
    let Some(provider_generation) = provider.generation else {
        return false;
    };
    if let Some(live) = series_live.trade_lifecycle(&input.series) {
        live.generation == provider_generation && live.connected && live.history_ready()
    } else {
        series_live.candle(&input.series).is_some_and(|live| {
            live.generation == provider_generation && live.connected && live.history_ready()
        })
    }
}

pub(super) fn study_live_market_data<'a>(
    engine: &MarketEngine,
    order_books: &'a BTreeMap<(String, String), ProviderOrderBook>,
    series_live: &SeriesLifecycles,
    input: &StudyMarketInput,
) -> Option<StudyLiveMarketData<'a>> {
    if !study_market_series_live_ready(engine, series_live, input) {
        return None;
    }
    let provider = engine.provider_status(&input.series.provider_id)?;
    let provider_generation = provider.generation?;
    let generation = provider_generation.0.get();
    order_books
        .values()
        .find_map(|book| book.study_live_market_data(input, generation))
}

impl Coordinator<'_> {
    pub(super) fn handle_study_command(&mut self, command: super::Command) {
        match command {
            super::Command::RegisterStudy(client_id, consumer_id, registration, reply) => {
                let _ =
                    reply.send(self.handle_register_study(client_id, consumer_id, registration));
            }
            super::Command::ReinitializeStudy(client_id, study_id, registration, reply) => {
                let _ =
                    reply.send(self.handle_reinitialize_study(client_id, study_id, registration));
            }
            super::Command::RemoveStudy(client_id, study_id, reply) => {
                let _ = reply.send(self.handle_remove_study(client_id, study_id));
            }
            super::Command::SetStudyChartContext(client_id, consumer_id, context, reply) => {
                let _ = reply.send(self.handle_set_study_chart_context(
                    client_id,
                    consumer_id,
                    context,
                ));
            }
            _ => {}
        }
    }

    pub(super) fn handle_register_study(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        registration: super::super::NativeStudyRegistration,
    ) -> Result<super::super::StudyInstanceId, String> {
        authorize_consumer(&self.engine, client_id, consumer_id)?;
        let study_id = self
            .studies
            .register_native_for_consumer(consumer_id, registration)
            .map_err(|error| error.to_string())?;
        let prepared = self.reconcile_study_market_data().and_then(|()| {
            self.execute_study_change(StudyChange::Ready(study_id))
                .map(|batch| !batch.executed.is_empty())
                .map_err(|error| error.to_string())
        });
        match prepared {
            Ok(executed) => {
                if executed {
                    self.publish_study_outputs(&[study_id]);
                }
            }
            Err(error) => {
                let _ = self.studies.remove_subtree(study_id);
                let _ = self.reconcile_study_market_data();
                return Err(error);
            }
        }
        Ok(study_id)
    }

    pub(super) fn handle_remove_study(
        &mut self,
        client_id: ClientId,
        study_id: super::super::StudyInstanceId,
    ) -> Result<Vec<super::super::StudyInstanceId>, String> {
        let owner = self
            .studies
            .owner(study_id)
            .ok_or_else(|| "study instance is unavailable".to_string())?;
        authorize_consumer(&self.engine, client_id, owner)?;
        let removed = self
            .studies
            .remove_subtree(study_id)
            .map_err(|error| error.to_string())?;
        if let Some(events) = self.events.get_mut(&owner) {
            events.publish_study_removed(
                owner,
                self.engine
                    .current_demand(owner)
                    .and_then(|demand| demand.generation),
                &removed,
            );
        }
        self.reconcile_study_market_data()?;
        Ok(removed)
    }

    pub(super) fn handle_set_study_chart_context(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        context: crate::study::StudyChartContext,
    ) -> Result<Vec<super::super::StudyInstanceId>, String> {
        authorize_consumer(&self.engine, client_id, consumer_id)?;
        let notified = self.studies.set_chart_context(consumer_id, context);
        let mut executed = Vec::new();
        for study_id in notified {
            if let Ok(batch) = self.execute_study_change(StudyChange::ReadySubtree(study_id)) {
                executed.extend(batch.executed);
            }
        }
        if !executed.is_empty() {
            executed.sort_unstable();
            executed.dedup();
            self.publish_study_outputs(&executed);
        }
        Ok(executed)
    }

    pub(super) fn handle_reinitialize_study(
        &mut self,
        client_id: ClientId,
        study_id: super::super::StudyInstanceId,
        registration: super::super::NativeStudyRegistration,
    ) -> Result<Vec<super::super::StudyInstanceId>, String> {
        let owner = self
            .studies
            .owner(study_id)
            .ok_or_else(|| "study instance is unavailable".to_string())?;
        authorize_consumer(&self.engine, client_id, owner)?;
        let checkpoint = self
            .studies
            .checkpoint_subtree(study_id)
            .map_err(|error| error.to_string())?;
        let affected = self
            .studies
            .reinitialize_native_for_consumer(owner, study_id, registration)
            .map_err(|error| error.to_string())?;

        let prepared = self.reconcile_study_market_data().and_then(|()| {
            self.execute_study_change(StudyChange::ReadySubtree(study_id))
                .map(|batch| batch.executed)
                .map_err(|error| error.to_string())
        });
        match prepared {
            Ok(executed) => {
                if let Some(events) = self.events.get_mut(&owner) {
                    events.publish_study_invalidated(
                        owner,
                        self.engine
                            .current_demand(owner)
                            .and_then(|demand| demand.generation),
                        &affected,
                    );
                }
                self.publish_study_outputs(&executed);
                Ok(affected)
            }
            Err(error) => {
                self.studies.restore_subtree_checkpoint(checkpoint);
                if let Err(rollback_error) = self.reconcile_study_market_data() {
                    diagnostic!("Aeris study reinitialization rollback failed: {rollback_error}");
                }
                Err(error)
            }
        }
    }

    pub(in super::super) fn publish_study_outputs(
        &mut self,
        study_ids: &[super::super::StudyInstanceId],
    ) {
        for &study_id in study_ids {
            let Some(owner) = self.studies.owner(study_id) else {
                continue;
            };
            if !self
                .engine
                .current_demand(owner)
                .is_some_and(|demand| demand.resource_class.publishes_ui())
            {
                continue;
            }
            let Some(definition) = self.studies.definition(study_id) else {
                continue;
            };
            let study_identifier = definition.identifier.clone();
            let outputs = definition.outputs.clone();
            for (output_index, output) in outputs.into_iter().enumerate() {
                let output_id = super::super::StudyOutputId {
                    study_id,
                    output_index,
                };
                let Some(series) = self.studies.output_series(output_id).cloned() else {
                    continue;
                };
                if let Some(events) = self.events.get_mut(&owner) {
                    events.publish_study_output(MarketRuntimeEvent::StudyOutputSnapshot(
                        MarketStudyOutputSnapshot {
                            consumer_id: owner,
                            generation: self
                                .engine
                                .current_demand(owner)
                                .and_then(|demand| demand.generation),
                            study_id,
                            output_id,
                            study_identifier: study_identifier.clone(),
                            output,
                            stream_requirements: self
                                .studies
                                .input_stream_requirements(study_id)
                                .unwrap_or(StreamRequirements::NONE),
                            series,
                        },
                    ));
                }
            }
            let generation = self
                .engine
                .current_demand(owner)
                .and_then(|demand| demand.generation);
            let style_overlay = self.studies.resolved_style_overlay(study_id);
            let deltas = self.studies.take_scene_deltas(study_id);
            let alerts = self.studies.take_triggered_alerts(study_id);
            if let Some(events) = self.events.get_mut(&owner) {
                if let Some(overlay) = style_overlay {
                    events.publish_study_style_overlay(MarketRuntimeEvent::StudyStyleOverlay(
                        crate::MarketStudyStyleOverlay {
                            consumer_id: owner,
                            generation,
                            study_id,
                            study_identifier: study_identifier.clone(),
                            bar_colors: overlay.bar_colors,
                            background_colors: overlay.background_colors,
                            levels: overlay.levels,
                            fills: overlay.fills,
                        },
                    ));
                }
                if !deltas.is_empty() {
                    events.publish_study_scene_delta(MarketRuntimeEvent::StudySceneDelta(
                        crate::MarketStudySceneDelta {
                            consumer_id: owner,
                            generation,
                            study_id,
                            study_identifier: study_identifier.clone(),
                            deltas,
                        },
                    ));
                }
                for alert in alerts {
                    events.publish_study_alert(MarketRuntimeEvent::StudyAlertTriggered(
                        crate::MarketStudyAlertTriggered {
                            consumer_id: owner,
                            generation,
                            study_id,
                            study_identifier: study_identifier.clone(),
                            event: alert,
                        },
                    ));
                }
            }
        }
    }

    pub(in super::super) fn execute_study_change(
        &mut self,
        change: StudyChange<'_>,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let series_live = &self.series;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, series_live, input)
        };
        match change {
            StudyChange::Ready(study_id) => {
                let executed =
                    studies.execute_ready_with_live(engine, study_id, &mut live_market)?;
                Ok(StudyExecutionBatch {
                    executed: if executed { vec![study_id] } else { Vec::new() },
                    errors: Vec::new(),
                })
            }
            StudyChange::ReadySubtree(study_id) => Ok(StudyExecutionBatch {
                executed: studies.execute_ready_subtree_with_live(
                    engine,
                    study_id,
                    &mut live_market,
                )?,
                errors: Vec::new(),
            }),
            StudyChange::Market(series) => {
                studies.execute_ready_for_market_with_live(engine, series, &mut live_market)
            }
            StudyChange::Bar(series, timestamp) => studies.execute_live_market_change_with_live(
                engine,
                series,
                timestamp,
                &mut live_market,
            ),
            StudyChange::History(series, first, last) => studies
                .execute_history_range_change_with_live(
                    engine,
                    series,
                    first,
                    last,
                    &mut live_market,
                ),
            StudyChange::NonBar(change) => {
                let mut market_ready = |input: &StudyMarketInput| {
                    study_market_series_live_ready(engine, series_live, input)
                };
                studies.execute_live_non_bar_change_with_live(
                    engine,
                    change,
                    &mut live_market,
                    &mut market_ready,
                )
            }
        }
    }

    pub(super) fn reconcile_study_market_data(&mut self) -> Result<(), String> {
        let changes = self
            .studies
            .reconcile_market_leases(&mut self.engine)
            .map_err(|error| error.to_string())?;
        self.reconcile_order_books();

        for change in &changes {
            match change.kind {
                StudyMarketLeaseChangeKind::Acquired => {
                    self.prepare_study_market_series(&change.series, change.streams)?;
                }
                StudyMarketLeaseChangeKind::Updated => {
                    self.ensure_realtime(&change.series)?;
                }
                StudyMarketLeaseChangeKind::Released => {}
            }
        }
        self.release_unused_live_market_data();
        Ok(())
    }

    fn prepare_study_market_series(
        &mut self,
        series: &BarSeriesKey,
        streams: StreamRequirements,
    ) -> Result<(), String> {
        if !self.ensure_provider_connection(&series.provider_id) {
            return Err("provider connection is not ready".into());
        }
        let provider_generation = self.provider_generation_for_series(series)?;
        self.ensure_realtime(series)?;
        if !streams.contains(MarketStream::Bars) {
            return Ok(());
        }
        if let Some(snapshot) = self.engine.series_snapshot(series) {
            self.prepare_cached_demand(series, provider_generation, &snapshot)?;
            return Ok(());
        }
        self.enqueue_history_recovery(series, provider_generation)
            .map_err(str::to_string)
    }
}
