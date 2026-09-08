use super::{
    BarSeriesKey, ClientId, ConsumerId, Coordinator, InstallProviderInstrument,
    MAXIMUM_CATALOG_FIELD_BYTES, MAXIMUM_CATALOG_INSTRUMENTS, MarketStream, NonZeroU64,
    ProviderCatalogCommand, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderGeneration, ProviderHealth, ProviderInstrumentSelection, Reply, ResourceMode,
    RithmicCatalogControl, RithmicCatalogEvent, SearchProviderInstruments,
    SelectProviderInstrument, StreamRequirements, SyncSender, TrySendError, authorize_consumer,
    envelope,
};
use crate::hyperliquid_realtime::{HyperliquidCatalogControl, HyperliquidCatalogEvent};

pub(super) fn id(value: u64) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| "market identity must be non-zero".to_string())
}

fn supported_catalog_provider(provider: &str) -> bool {
    provider == "rithmic" || provider == "hyperliquid"
}

pub(super) fn validate_provider_search(search: &SearchProviderInstruments) -> Result<(), String> {
    id(search.consumer_id)?;
    id(search.search_generation)?;
    if !supported_catalog_provider(&search.provider)
        || search.maximum_results == 0
        || usize::try_from(search.maximum_results).unwrap_or(usize::MAX)
            > MAXIMUM_CATALOG_INSTRUMENTS
        || (!search.query.trim().is_empty() && !valid_catalog_field(&search.query))
    {
        return Err("provider instrument search is invalid".to_string());
    }
    Ok(())
}

pub(super) fn validate_provider_selection(
    selection: &SelectProviderInstrument,
) -> Result<(), String> {
    id(selection.consumer_id)?;
    id(selection.selection_generation)?;
    id(selection.search_generation)?;
    if !supported_catalog_provider(&selection.provider)
        || ![
            &selection.symbol,
            &selection.exchange,
            &selection.entitlement_id,
        ]
        .into_iter()
        .all(|value| valid_catalog_field(value))
    {
        return Err("provider instrument selection is invalid".to_string());
    }
    Ok(())
}

pub(super) fn valid_catalog_field(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAXIMUM_CATALOG_FIELD_BYTES
        && !value.chars().any(char::is_control)
}

pub(super) fn try_send_rithmic_catalog(
    controls: &SyncSender<RithmicCatalogControl>,
    control: RithmicCatalogControl,
    provider_id: &str,
) -> Result<(), String> {
    controls.try_send(control).map_err(|error| match error {
        TrySendError::Full(_) => {
            format!("{provider_id} catalog command capacity is exhausted")
        }
        TrySendError::Disconnected(_) => {
            format!("{provider_id} catalog worker is unavailable")
        }
    })
}

pub(super) fn try_send_hyperliquid_catalog(
    controls: &SyncSender<HyperliquidCatalogControl>,
    control: HyperliquidCatalogControl,
    provider_id: &str,
) -> Result<(), String> {
    controls.try_send(control).map_err(|error| match error {
        TrySendError::Full(_) => {
            format!("{provider_id} catalog command capacity is exhausted")
        }
        TrySendError::Disconnected(_) => {
            format!("{provider_id} catalog worker is unavailable")
        }
    })
}

pub(super) fn chart_stream_requirements(_series: &BarSeriesKey) -> StreamRequirements {
    StreamRequirements::BARS.with(MarketStream::Depth)
}

pub(super) fn validate_provider_instrument(
    instrument: &InstallProviderInstrument,
) -> Result<(), String> {
    if !supported_catalog_provider(&instrument.provider)
        || instrument.session_generation == 0
        || instrument.selection_generation == 0
    {
        return Err("provider instrument generation must be non-zero".to_string());
    }
    for value in [
        &instrument.provider,
        &instrument.instrument_id,
        &instrument.provider_symbol,
        &instrument.display_symbol,
        &instrument.venue_id,
        &instrument.entitlement_id,
    ] {
        if !valid_catalog_field(value) {
            return Err("provider instrument identity is invalid".to_string());
        }
    }
    if instrument.price_scale > 18 || instrument.quantity_scale > 18 {
        return Err("provider instrument precision is invalid".to_string());
    }
    Ok(())
}

impl Coordinator<'_> {
    pub(super) fn handle_provider_search(
        &self,
        client_id: ClientId,
        search: SearchProviderInstruments,
        reply: &Reply<()>,
    ) {
        let consumer_id = search.consumer_id;
        let provider = search.provider.clone();
        let result = self
            .authorize_catalog_consumer(client_id, consumer_id)
            .and_then(|()| {
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Search(search))
            });
        let _ = reply.send(result);
    }

    pub(super) fn handle_provider_selection(
        &self,
        client_id: ClientId,
        selection: SelectProviderInstrument,
        reply: &Reply<()>,
    ) {
        let consumer_id = selection.consumer_id;
        let provider = selection.provider.clone();
        let result = self
            .authorize_catalog_consumer(client_id, consumer_id)
            .and_then(|()| {
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Select(selection))
            });
        let _ = reply.send(result);
    }

    pub(super) fn authorize_catalog_consumer(
        &self,
        client_id: ClientId,
        consumer_id: u64,
    ) -> Result<(), String> {
        authorize_consumer(&self.engine, client_id, ConsumerId(id(consumer_id)?))
    }

    pub(super) fn install_provider_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_instrument(instrument)?;
        let provider = instrument.provider.clone();
        let provider_generation = ProviderGeneration(
            NonZeroU64::new(instrument.session_generation)
                .ok_or_else(|| "provider instrument session is invalid".to_string())?,
        );
        let engine_generation = self
            .engine
            .provider_status(&provider)
            .and_then(|status| status.generation);
        let catalog_session = self.catalog_sessions.get(&provider).copied();
        let engine_session = engine_generation.map(|generation| generation.0.get());
        let current_session = catalog_session.into_iter().chain(engine_session).max();
        if current_session.is_some_and(|current| instrument.session_generation < current) {
            return Err("provider instrument session is stale".to_string());
        }
        let key = (provider.clone(), instrument.instrument_id.clone());
        if let Some(installed) = self.catalog.get(&key) {
            if instrument.session_generation < installed.session_generation
                || (instrument.session_generation == installed.session_generation
                    && instrument.selection_generation < installed.selection_generation)
            {
                return Err("provider instrument selection is stale".to_string());
            }
            if instrument.session_generation == installed.session_generation
                && instrument.selection_generation == installed.selection_generation
            {
                return (installed == instrument)
                    .then_some(())
                    .ok_or_else(|| "provider instrument selection conflicts".to_string());
            }
        } else if self.catalog.len() >= MAXIMUM_CATALOG_INSTRUMENTS {
            return Err("provider instrument catalog capacity is exhausted".to_string());
        }
        // Provider session generation dominates selection generation. Selection
        // counters are consumer/command-local and may restart or overlap across
        // instruments, while a newer provider session retires every older
        // callback. Retained instrument metadata remains valid across that
        // transport change and is refreshed per instrument when/if selected.
        if engine_generation.is_none_or(|current| provider_generation > current) {
            self.engine
                .begin_provider_session(&provider, provider_generation)
                .map_err(|error| error.to_string())?;
        }
        self.catalog_sessions
            .entry(provider.clone())
            .and_modify(|current| *current = (*current).max(instrument.session_generation))
            .or_insert(instrument.session_generation);
        let instrument_id = instrument.instrument_id.clone();
        self.catalog.insert(key, instrument.clone());
        self.reconcile_order_books();
        self.activate_retained_instrument(instrument, provider_generation);
        if self.resource_mode == ResourceMode::MarketsLive
            && let Some(series) = self
                .warm_priority
                .iter()
                .find(|series| {
                    series.provider_id == provider && series.instrument_id == instrument_id
                })
                .cloned()
        {
            self.retained_live.insert(series.clone());
            let _ = self.enqueue_history(&series, provider_generation);
        }
        Ok(())
    }

    pub(super) fn activate_retained_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
        generation: ProviderGeneration,
    ) {
        let series = self
            .retained_history
            .keys()
            .filter(|series| {
                series.provider_id == instrument.provider
                    && series.instrument_id == instrument.instrument_id
                    && series.entitlement_id == instrument.entitlement_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let (Ok(price_scale), Ok(quantity_scale)) = (
            u8::try_from(instrument.price_scale),
            u8::try_from(instrument.quantity_scale),
        ) else {
            return;
        };
        for series in series {
            let Some(stored) = self.retained_history.remove(&series) else {
                continue;
            };
            if self
                .engine
                .install_retained_history(
                    generation,
                    &series,
                    price_scale,
                    quantity_scale,
                    stored.bars,
                )
                .is_ok()
            {
                self.prewarmed.insert(series.clone());
                self.local_loaded.insert((series, generation));
            }
        }
    }

    pub(super) fn handle_rithmic_catalog(&mut self, event: RithmicCatalogEvent) {
        match event {
            RithmicCatalogEvent::SearchCompleted(result) => self.handle_catalog_search(result),
            RithmicCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => self.handle_catalog_selection(consumer_id, command_generation, instrument),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection,
            } => self.handle_catalog_rejection(rejection, selection),
        }
    }

    pub(super) fn handle_hyperliquid_catalog(&mut self, event: HyperliquidCatalogEvent) {
        match event {
            HyperliquidCatalogEvent::SearchCompleted(result) => {
                self.restore_hyperliquid_catalog_health(&result.provider);
                self.handle_catalog_search(result);
            }
            HyperliquidCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => {
                self.restore_hyperliquid_catalog_health(&instrument.provider);
                self.handle_catalog_selection(consumer_id, command_generation, instrument);
            }
            HyperliquidCatalogEvent::Rejected {
                rejection,
                selection,
            } => self.handle_catalog_rejection(rejection, selection),
            HyperliquidCatalogEvent::RefreshFailed { detail } => {
                self.degrade_hyperliquid_catalog_health(&detail);
            }
        }
    }

    /// Records a background catalog refresh failure without changing realtime
    /// transport health. The retained catalog remains usable, so a catalog
    /// problem must never paint an online WebSocket as reconnecting.
    pub(super) fn degrade_hyperliquid_catalog_health(&mut self, detail: &str) {
        let current = self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation);
        let Some(generation) = current else {
            return;
        };
        let healthy = self
            .engine
            .provider_status("hyperliquid")
            .is_some_and(|status| status.health == ProviderHealth::Online);
        if !healthy || self.hyperliquid_catalog_degraded.is_some() {
            return;
        }
        self.hyperliquid_catalog_degraded = Some(generation);
        eprintln!("Axiusflow engine Hyperliquid catalog refresh degraded: {detail}");
    }

    /// Clears catalog degradation after a success from the same provider
    /// generation. This intentionally publishes no provider state: only the
    /// realtime worker owns transport Online/Recovering transitions.
    pub(super) fn restore_hyperliquid_catalog_health(&mut self, provider: &str) {
        if provider != "hyperliquid" {
            return;
        }
        let Some(degraded) = self.hyperliquid_catalog_degraded else {
            return;
        };
        let current = self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation);
        if current != Some(degraded) {
            self.hyperliquid_catalog_degraded = None;
            return;
        }
        self.hyperliquid_catalog_degraded = None;
    }

    pub(super) fn handle_catalog_search(
        &mut self,
        result: axiusflow_engine_protocol::ProviderInstrumentSearchResult,
    ) {
        if result.consumer_id == 0 {
            self.select_warm_instrument(&result);
            return;
        }
        let Ok(consumer_id) = id(result.consumer_id).map(ConsumerId) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.catalog_search = Some(envelope::Payload::ProviderInstrumentSearchResult(result));
        }
    }

    pub(super) fn select_warm_instrument(
        &mut self,
        result: &axiusflow_engine_protocol::ProviderInstrumentSearchResult,
    ) {
        if self.warm_search_generations.get(&result.provider).copied()
            != Some(result.search_generation)
        {
            return;
        }
        let Some(series) = self.warm_restore_pending.get(&result.provider).cloned() else {
            return;
        };
        let Some(warm) = self.warm_series.get(&series).cloned() else {
            return;
        };
        if !result.instruments.iter().any(|candidate| {
            candidate.symbol == warm.instrument.provider_symbol
                && candidate.exchange == warm.instrument.venue_id
        }) {
            self.warm_restore_pending.remove(&result.provider);
            self.warm_restore_skipped.insert(series);
            self.activate_markets_live_provider_hot_set(&result.provider);
            return;
        }
        match self.providers.dispatch_catalog(
            &result.provider,
            ProviderCatalogCommand::Select(SelectProviderInstrument {
                consumer_id: 0,
                selection_generation: result.search_generation,
                search_generation: result.search_generation,
                provider: result.provider.clone(),
                symbol: warm.instrument.provider_symbol.clone(),
                exchange: warm.instrument.venue_id.clone(),
                entitlement_id: warm.instrument.entitlement_id.clone(),
            }),
        ) {
            Ok(()) => {}
            Err(error) if error.ends_with("catalog command capacity is exhausted") => {
                // A full bounded control lane is transient. Re-run the search on
                // a later coordinator tick instead of turning queue pressure into
                // a permanent saved-demand rejection.
                self.warm_restore_pending.remove(&result.provider);
            }
            Err(_) => {
                self.warm_restore_pending.remove(&result.provider);
                self.warm_restore_skipped.insert(series);
                self.activate_markets_live_provider_hot_set(&result.provider);
            }
        }
    }

    pub(super) fn handle_catalog_selection(
        &mut self,
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    ) {
        if consumer_id == 0 {
            if self
                .warm_search_generations
                .get(&instrument.provider)
                .copied()
                != Some(command_generation)
            {
                return;
            }
            let provider = instrument.provider.clone();
            let Some(series) = self.warm_restore_pending.get(&provider).cloned() else {
                return;
            };
            let exact = self.warm_series.get(&series).is_some_and(|warm| {
                instrument.instrument_id == warm.instrument.instrument_id
                    && instrument.provider_symbol == warm.instrument.provider_symbol
                    && instrument.venue_id == warm.instrument.venue_id
                    && instrument.entitlement_id == warm.instrument.entitlement_id
            });
            self.warm_restore_pending.remove(&provider);
            if !exact || self.install_provider_instrument(&instrument).is_err() {
                self.warm_restore_skipped.insert(series);
            }
            self.activate_markets_live_provider_hot_set(&provider);
            return;
        }
        let Ok(id) = id(consumer_id).map(ConsumerId) else {
            return;
        };
        let provider = instrument.provider.clone();
        let publication = match self.install_provider_instrument(&instrument) {
            Ok(()) => envelope::Payload::ProviderInstrumentSelection(ProviderInstrumentSelection {
                consumer_id,
                instrument: Some(instrument),
                command_generation,
            }),
            Err(_) => envelope::Payload::ProviderCatalogRejected(ProviderCatalogRejected {
                consumer_id,
                provider,
                provider_generation: Some(instrument.session_generation),
                command_generation,
                reason: ProviderCatalogRejectionReason::SubscriptionRejected as i32,
            }),
        };
        if let Some(events) = self.events.get_mut(&id) {
            events.catalog_selection = Some(publication);
        }
    }

    pub(super) fn handle_catalog_rejection(
        &mut self,
        rejection: ProviderCatalogRejected,
        selection: bool,
    ) {
        if rejection.consumer_id == 0 {
            if self
                .warm_search_generations
                .get(&rejection.provider)
                .copied()
                == Some(rejection.command_generation)
                && let Some(series) = self.warm_restore_pending.remove(&rejection.provider)
            {
                self.warm_restore_skipped.insert(series);
                self.activate_markets_live_provider_hot_set(&rejection.provider);
            }
            return;
        }
        let Ok(consumer_id) = id(rejection.consumer_id).map(ConsumerId) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            let slot = if selection {
                &mut events.catalog_selection
            } else {
                &mut events.catalog_search
            };
            *slot = Some(envelope::Payload::ProviderCatalogRejected(rejection));
        }
    }
}
