use super::super::history::tracker::HistoryTicket;
use super::{CandleHandoffSeed, RecoveryReason, SeriesFailure, SeriesInput, SeriesLifecycle};
use crate::market_service::{
    BarSeriesKey, Coordinator, FailureStage, FormingBar, LiveModel, MarketBar, MarketStream,
    MarketTrade, ProviderGeneration, ProviderHealth, TradeHandoffSeed, id,
};
use aeris_observability::diagnostic;

impl Coordinator<'_> {
    pub(in crate::market_service) fn begin_lifecycle_session(
        &mut self,
        provider: &str,
        generation: u64,
        model: LiveModel,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current = self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation);
        if current.is_some_and(|current| generation < current) {
            return;
        }
        if current.is_none_or(|current| generation > current)
            && self
                .engine
                .begin_provider_session(provider, generation)
                .is_err()
        {
            return;
        }
        if current.is_some_and(|current| generation > current) {
            self.quotes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear_provider(provider);
            if model == LiveModel::ProviderCandles {
                self.clear_provider_display_depth(provider);
                if self
                    .providers
                    .descriptor(provider)
                    .is_some_and(|descriptor| {
                        descriptor.capabilities.candle_demand_policy
                            == crate::market_service::CandleDemandPolicy::ReconcileAfterSelection
                    })
                {
                    self.session_mut(provider).demand_dirty = true;
                }
            }
            self.invalidate_provider_live_market(provider, generation);
            self.history.cancel_provider(provider);
            let series = self
                .series
                .keys()
                .filter(|series| {
                    series.provider_id == provider
                        && (self.series.trade_contains_key(series)
                            == (model == LiveModel::TradeBuilt))
                })
                .cloned()
                .collect::<Vec<_>>();
            for selected in &series {
                if model == LiveModel::TradeBuilt {
                    if let Some(live) = self.series.trade_lifecycle_mut(selected) {
                        live.reset_trade(generation);
                    }
                } else if let Some(live) = self.series.candle_mut(selected) {
                    live.reset(generation);
                }
                self.broadcast_series_recovery_for(
                    selected,
                    "Provider live session changed; covering history is reloading",
                );
            }
            for selected in series {
                self.request_series_history_recovery(&selected, generation);
            }
        }
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Connecting)
            .is_ok()
        {
            self.broadcast_provider_for(provider, None);
        }
    }

    pub(in crate::market_service) fn mark_lifecycle_online(
        &mut self,
        provider: &str,
        generation: u64,
        model: LiveModel,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
            || self
                .engine
                .set_provider_health(provider, generation, ProviderHealth::Online)
                .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, None);
        let mut ready = Vec::new();
        let missing = self
            .series
            .iter_mut()
            .filter(|(series, _)| series.provider_id == provider)
            .filter_map(|(series, live)| {
                if live.generation != generation
                    || live.is_candle() != (model == LiveModel::ProviderCandles)
                {
                    return None;
                }
                let is_ready = if model == LiveModel::TradeBuilt {
                    live.mark_trade_online()
                } else {
                    live.connected = true;
                    live.history_ready()
                };
                if is_ready {
                    ready.push(series.clone());
                    None
                } else {
                    Some(series.clone())
                }
            })
            .collect::<Vec<_>>();
        for series in ready {
            self.series_live_if_ready(&series);
        }
        if model == LiveModel::ProviderCandles {
            if let Some(epoch) = self.provider_epochs.current(provider, generation.0.get()) {
                for series in missing {
                    self.transition_candle_series(&series, SeriesInput::Online(epoch));
                }
            }
        } else {
            for series in missing {
                if self
                    .provider_epochs
                    .current(&series.provider_id, generation.0.get())
                    .is_none_or(|epoch| self.history.inflight(&series, epoch).is_none())
                {
                    self.request_series_history_recovery(&series, generation);
                }
            }
        }
    }

    pub(in crate::market_service) fn mark_lifecycle_recovering(
        &mut self,
        provider: &str,
        generation: u64,
        model: LiveModel,
        detail: &'static str,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        self.price_alerts.reset_provider_baselines(provider);
        if model == LiveModel::ProviderCandles {
            self.clear_provider_display_depth(provider);
            self.session_mut(provider).demand_dirty = true;
        }
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Recovering)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, Some(detail));
        let candle_series = self
            .series
            .iter_mut()
            .filter(|(series, _)| series.provider_id == provider)
            .filter_map(|(series, live)| {
                live.connected = false;
                if let Some(trade) = live.trade_model_mut() {
                    trade.connected = false;
                }
                live.is_candle().then(|| series.clone())
            })
            .collect::<Vec<_>>();
        if model == LiveModel::ProviderCandles
            && let Some(epoch) = self.provider_epochs.current(provider, generation.0.get())
        {
            for series in candle_series {
                self.transition_candle_series(&series, SeriesInput::ProviderOffline(epoch));
            }
        }
        self.invalidate_provider_live_market(provider, generation);
    }

    pub(in crate::market_service) fn mark_lifecycle_failed(
        &mut self,
        provider: &str,
        generation: u64,
        detail: &str,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current = self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation);
        if current != Some(generation)
            && (current.is_some()
                || self
                    .engine
                    .begin_provider_session(provider, generation)
                    .is_err())
        {
            return;
        }
        self.price_alerts.reset_provider_baselines(provider);
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Failed)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, Some(detail));
        let candle_series = self
            .series
            .candle_keys()
            .filter(|series| series.provider_id == provider)
            .cloned()
            .collect::<Vec<_>>();
        for series in candle_series {
            self.transition_candle_series(
                &series,
                SeriesInput::Failed(SeriesFailure::Provider(detail.to_owned())),
            );
        }
        for (series, live) in self.series.iter_mut() {
            if series.provider_id == provider {
                live.connected = false;
                if let Some(model) = live.trade_model_mut() {
                    model.connected = false;
                }
            }
        }
        self.invalidate_provider_live_market(provider, generation);
    }

    pub(in crate::market_service) fn recover_lifecycle_series(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        stage: FailureStage,
        detail: &str,
    ) {
        if self.series.candle_contains_key(series) {
            self.recover_candle_series(series, generation, stage, detail);
            return;
        }
        if self
            .series
            .trade_lifecycle(series)
            .is_none_or(|live| live.generation != generation || live.awaiting_history())
        {
            return;
        }
        diagnostic!(
            "Aeris market series recovery started for {}: {detail}",
            series.instrument_id
        );
        self.cancel_inflight_history_for_live_recovery(series, generation);
        if let Some(live) = self.series.trade_lifecycle_mut(series) {
            live.recover_trade(RecoveryReason::SequenceGap);
        }
        self.broadcast_series_recovery_for(series, detail);
        if self
            .provider_epochs
            .current(&series.provider_id, generation.0.get())
            .is_none_or(|epoch| self.history.inflight(series, epoch).is_none())
        {
            self.request_series_history_recovery(series, generation);
        }
    }

    pub(in crate::market_service) fn recover_trade_sequence_gaps(
        &mut self,
        provider: &str,
        generation: u64,
        detail: &str,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let series = self
            .series
            .trade_keys()
            .filter(|series| series.provider_id == provider)
            .filter(|series| {
                self.series
                    .trade(series)
                    .is_some_and(|live| live.generation == generation)
            })
            .cloned()
            .collect::<Vec<_>>();
        for series in series {
            self.recover_lifecycle_series(&series, generation, FailureStage::Aggregation, detail);
        }
    }

    /// Classify aggregation failures before exposing point-in-time trade data
    /// to bar-aligned studies; one failing cadence must not retire its siblings.
    pub(in crate::market_service) fn accept_lifecycle_trade(
        &mut self,
        provider: &str,
        generation: u64,
        trade: &MarketTrade,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        if trade.metadata.provider_id != provider
            || trade.metadata.session_generation != generation.0.get()
        {
            self.mark_lifecycle_recovering(
                provider,
                generation.0.get(),
                LiveModel::TradeBuilt,
                "Trade-built live session identity requires recovery",
            );
            for (series, live) in self.series.trade_lifecycle_iter_mut() {
                if series.provider_id == provider {
                    live.recover_trade(RecoveryReason::ProviderReconnected);
                }
            }
            return;
        }
        self.evaluate_price_alert_trade(trade);
        let instrument_id = trade.metadata.instrument_id.clone();
        let entitlement_id = trade.metadata.entitlement_id.clone();
        let observed_unix_nanos = trade
            .metadata
            .timestamps
            .exchange_unix_nanos
            .or(trade.metadata.timestamps.provider_unix_nanos)
            .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
        let failed = self
            .series
            .trade_lifecycle_iter_mut()
            .filter(|(series, live)| {
                series.provider_id == provider
                    && live.generation == generation
                    && live.connected
                    && live.series.instrument_id == trade.metadata.instrument_id
                    && live.series.entitlement_id == trade.metadata.entitlement_id
            })
            .filter_map(|(series, live)| live.accept_trade(trade).is_err().then(|| series.clone()))
            .collect::<Vec<_>>();
        for series in failed {
            self.recover_lifecycle_series(
                &series,
                generation,
                FailureStage::Aggregation,
                "Trade-built instrument aggregation requires covering history",
            );
        }
        let trade_changed = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.clone()))
            .is_some_and(|order_book| order_book.accept_recent_trade(trade));
        if trade_changed {
            self.broadcast_order_book(provider, &instrument_id);
            self.publish_non_bar_study_change(
                provider,
                &instrument_id,
                &entitlement_id,
                MarketStream::Trades,
                observed_unix_nanos,
            );
        }
    }

    /// The engine owns completed history; the lifecycle closes only the live seam.
    pub(in crate::market_service) fn install_trade_seed(
        &mut self,
        series: &BarSeriesKey,
        seed: TradeHandoffSeed<'_>,
    ) -> Result<(), String> {
        self.ensure_realtime(series)?;
        let Some(live) = self.series.trade_lifecycle_mut(series) else {
            return Err("Trade-built lifecycle is unavailable".into());
        };
        if let Err(error) = live.seed_trade(seed) {
            self.recover_trade_series_for_seed_error(series);
            return Err(error);
        }
        Ok(())
    }

    fn recover_trade_series_for_seed_error(&mut self, series: &BarSeriesKey) {
        if let Some(live) = self.series.trade_lifecycle_mut(series) {
            live.recover_trade(RecoveryReason::SequenceGap);
        }
        self.broadcast_series_recovery_for(series, "Trade-built history/live handoff failed");
    }

    pub(in crate::market_service) fn restore_trade_from_snapshot(
        &mut self,
        series: &BarSeriesKey,
        snapshot: &aeris_market_engine::SeriesSnapshot,
    ) -> Result<(), String> {
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
        self.install_trade_seed(
            series,
            TradeHandoffSeed {
                price_scale: snapshot.price_scale,
                quantity_scale: snapshot.quantity_scale,
                bars,
                forming,
                handoff_boundary_unix_nanos: snapshot
                    .bars
                    .last()
                    .map(|bar| bar.exchange_timestamp_unix_nanos),
            },
        )
    }

    /// Seed only after the engine has installed canonical completed history.
    /// The ticket is the tracker-issued current-lane identity, not an adapter
    /// generation or a counter duplicated by the lifecycle.
    pub(in crate::market_service) fn install_candle_seed(
        &mut self,
        series: &BarSeriesKey,
        _generation: ProviderGeneration,
        seed: CandleHandoffSeed<'_>,
        ticket: Option<&HistoryTicket>,
    ) -> Result<(), String> {
        self.ensure_realtime(series)?;
        let Some(live) = self.series.candle_mut(series) else {
            return Err("Provider candle lifecycle is unavailable".into());
        };
        let previous_state = live.state.clone();
        let had_buffered = !live.buffered.is_empty();
        if let Err(error) = live.seed(
            seed.price_scale,
            seed.quantity_scale,
            seed.bars,
            seed.forming,
        ) {
            self.transition_candle_series(
                series,
                SeriesInput::Recover(RecoveryReason::SequenceGap),
            );
            self.broadcast_series_recovery_for(series, "Provider history/live handoff failed");
            return Err(error);
        }
        live.state = previous_state;
        self.transition_candle_series(series, SeriesInput::Seeded(ticket));
        if had_buffered {
            self.transition_candle_series(series, SeriesInput::Candle);
        }
        Ok(())
    }

    pub(in crate::market_service) fn restore_candle_from_snapshot(
        &mut self,
        series: &BarSeriesKey,
        snapshot: &aeris_market_engine::SeriesSnapshot,
    ) -> Result<(), String> {
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
        self.install_candle_seed(
            series,
            snapshot.provider_generation,
            CandleHandoffSeed {
                price_scale: snapshot.price_scale,
                quantity_scale: snapshot.quantity_scale,
                bars,
                forming,
            },
            None,
        )
    }

    pub(in crate::market_service) fn recover_candle_series(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        _stage: FailureStage,
        detail: &str,
    ) {
        self.recover_candle_series_for_reason(
            series,
            generation,
            RecoveryReason::SequenceGap,
            detail,
        );
    }

    pub(in crate::market_service) fn recover_candle_series_for_reason(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        reason: RecoveryReason,
        detail: &str,
    ) {
        if self
            .series
            .candle(series)
            .is_some_and(SeriesLifecycle::awaiting_history)
        {
            return;
        }
        diagnostic!(
            "Aeris market series recovery started for {}: {detail}",
            series.instrument_id
        );
        self.cancel_inflight_history_for_live_recovery(series, generation);
        if let Some(live) = self.series.candle_mut(series) {
            live.dirty = false;
            live.buffered.clear();
            live.pending_publications.clear();
        }
        self.broadcast_series_recovery_for(series, detail);
        self.transition_candle_series(series, SeriesInput::Recover(reason));
    }

    pub(in crate::market_service) fn publish_lifecycle_live(&mut self) {
        self.collect_dirty_series();
        let ready = self
            .dirty_series
            .iter()
            .filter_map(|series| {
                let live = self.series.get_mut(series)?;
                let tails = if live.trade_model().is_some() {
                    live.take_trade_publication()?
                } else {
                    live.take_publication()?
                };
                Some(LivePublication::of(live, tails))
            })
            .collect::<Vec<_>>();
        for publication in ready {
            self.install_live_publication(publication);
        }
        self.collect_dirty_series();
    }

    /// Publishes the forming bar a covering-history seed left on the live
    /// handoff right behind its covering snapshot, so a chart opens on its
    /// current candle even while the live feed is still connecting.
    pub(in crate::market_service) fn publish_seeded_handoff(&mut self, series: &BarSeriesKey) {
        let Some(publication) = self.series.get_mut(series).and_then(|live| {
            let tails = live.take_handoff_publication()?;
            Some(LivePublication::of(live, tails))
        }) else {
            return;
        };
        self.install_live_publication(publication);
    }

    fn install_live_publication(&mut self, publication: LivePublication) {
        let LivePublication {
            series,
            generation,
            price_scale,
            quantity_scale,
            tails,
            trade_built,
        } = publication;
        let published =
            self.install_live_tails(&series, generation, price_scale, quantity_scale, tails);
        if let Err(error) = published {
            diagnostic!("Aeris engine live publication failed: {error}");
            self.recover_lifecycle_series(
                &series,
                generation,
                FailureStage::Publication,
                "Live publication requires covering history",
            );
        } else if trade_built && let Some(model) = self.series.trade_mut(&series) {
            model.last_publication_at = self.clock.monotonic();
        }
    }
}

/// One drained lifecycle tail with the precision and identity it installs under.
struct LivePublication {
    series: BarSeriesKey,
    generation: ProviderGeneration,
    price_scale: u8,
    quantity_scale: u8,
    tails: Vec<MarketBar>,
    trade_built: bool,
}

impl LivePublication {
    fn of(live: &SeriesLifecycle, tails: Vec<MarketBar>) -> Self {
        let (price_scale, quantity_scale) = live
            .trade_model()
            .map_or((live.price_scale, live.quantity_scale), |model| {
                (model.price_scale, model.quantity_scale)
            });
        Self {
            series: live.series.clone(),
            generation: live.generation,
            price_scale,
            quantity_scale,
            tails,
            trade_built: live.trade_model().is_some(),
        }
    }
}
