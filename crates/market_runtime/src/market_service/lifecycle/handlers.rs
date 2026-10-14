use super::super::history::tracker::HistoryTicket;
use super::{CandleHandoffSeed, RecoveryReason, SeriesInput, SeriesLifecycle};
use crate::market_service::{
    BarSeriesKey, Coordinator, FailureStage, FormingBar, LiveSeriesPublication, ProviderGeneration,
    ProviderHealth, id,
};
use aeris_observability::diagnostic;

impl Coordinator<'_> {
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

    pub(in crate::market_service) fn begin_provider_candle_session(
        &mut self,
        provider: &str,
        generation: u64,
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
            self.clear_provider_display_depth(provider);
            self.invalidate_provider_live_market(provider, generation);
            if self
                .providers
                .descriptor(provider)
                .is_some_and(|descriptor| {
                    descriptor.candle_demand_policy
                        == crate::market_service::CandleDemandPolicy::ReconcileAfterSelection
                })
            {
                self.session_mut(provider).demand_dirty = true;
            }
            self.history.cancel_provider(provider);
            let series = self
                .series
                .candle_keys()
                .filter(|series| series.provider_id == provider)
                .cloned()
                .collect::<Vec<_>>();
            for selected in &series {
                if let Some(live) = self.series.candle_mut(selected) {
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
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, None);
    }

    pub(in crate::market_service) fn mark_provider_candle_online(
        &mut self,
        provider: &str,
        generation: u64,
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
        if self
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
            .candle_iter_mut()
            .filter(|(series, _)| series.provider_id == provider)
            .filter_map(|(series, live)| {
                if live.generation != generation {
                    return None;
                }
                live.connected = true;
                if live.history_ready() {
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
        if let Some(epoch) = self.provider_epochs.current(provider, generation.0.get()) {
            for series in missing {
                self.transition_candle_series(&series, SeriesInput::Online(epoch));
            }
        }
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
            crate::market_service::LoggedInstrument(&series.instrument_id)
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

    pub(in crate::market_service) fn mark_provider_candle_recovering(
        &mut self,
        provider: &str,
        generation: u64,
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
        self.clear_provider_display_depth(provider);
        self.session_mut(provider).demand_dirty = true;
        if self.price_alerts.has_active_provider(provider) {
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
        let series = self
            .series
            .candle_iter_mut()
            .filter(|(series, _)| series.provider_id == provider)
            .map(|(series, live)| {
                live.connected = false;
                series.clone()
            })
            .collect::<Vec<_>>();
        if let Some(epoch) = self.provider_epochs.current(provider, generation.0.get()) {
            for series in series {
                self.transition_candle_series(&series, SeriesInput::ProviderOffline(epoch));
            }
        }
        self.invalidate_provider_live_market(provider, generation);
    }

    pub(in crate::market_service) fn publish_lifecycle_candles(&mut self) {
        let ready = self
            .series
            .candle_values_mut()
            .filter_map(|live| {
                let update = live.take_publication()?;
                Some((
                    live.series.clone(),
                    live.generation,
                    live.price_scale,
                    live.quantity_scale,
                    update,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            let published = match update {
                LiveSeriesPublication::Tails(bars) => {
                    self.install_live_tails(&series, generation, price_scale, quantity_scale, bars)
                }
                LiveSeriesPublication::CompletedCorrection(bar) => self
                    .install_completed_live_correction(
                        &series,
                        generation,
                        price_scale,
                        quantity_scale,
                        bar,
                    ),
            };
            if let Err(error) = published {
                diagnostic!("Aeris engine live publication failed: {error}");
                self.recover_candle_series(
                    &series,
                    generation,
                    FailureStage::Publication,
                    "Live publication requires covering history",
                );
            }
        }
    }
}
