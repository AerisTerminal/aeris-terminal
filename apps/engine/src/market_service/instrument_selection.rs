use super::{
    BarSeriesKey, ClientId, ConsumerId, Coordinator, InstallProviderInstrument,
    MAXIMUM_CATALOG_FIELD_BYTES, MAXIMUM_CATALOG_INSTRUMENTS, MarketStream, NonZeroU64, Ordering,
    ProviderCatalogCommand, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderConnectionState, ProviderGeneration, ProviderHealth, ProviderInstrumentSelection,
    Reply, ResourceMode, RithmicCatalogControl, RithmicCatalogEvent, SearchProviderInstruments,
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
    StreamRequirements::BARS
        .with(MarketStream::Trades)
        .with(MarketStream::Depth)
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
        let session = self.catalog_sessions.get(&provider).copied();
        if session.is_some_and(|current| instrument.session_generation < current) {
            return Err("provider instrument session is stale".to_string());
        }
        let newer_session = session.is_none_or(|current| instrument.session_generation > current);
        let selection = (!newer_session)
            .then(|| self.catalog_selections.get(&provider).copied())
            .flatten();
        if selection.is_some_and(|current| instrument.selection_generation < current) {
            return Err("provider instrument selection is stale".to_string());
        }
        let key = (provider.clone(), instrument.instrument_id.clone());
        if selection == Some(instrument.selection_generation) {
            return self
                .catalog
                .get(&key)
                .filter(|installed| *installed == instrument)
                .map(|_| ())
                .ok_or_else(|| "provider instrument selection conflicts".to_string());
        }
        let retained_catalog_len = if newer_session {
            self.catalog
                .keys()
                .filter(|(installed_provider, _)| installed_provider != &provider)
                .count()
        } else {
            self.catalog.len()
        };
        let key_exists_after_reset = !newer_session && self.catalog.contains_key(&key);
        if !key_exists_after_reset && retained_catalog_len >= MAXIMUM_CATALOG_INSTRUMENTS {
            return Err("provider instrument catalog capacity is exhausted".to_string());
        }
        // A newer install generation proves the provider opened a newer catalog
        // session, so the engine session must advance with it: Rithmic history
        // demands are fenced against the engine generation, and the realtime
        // worker only announces its generation after history succeeds.
        if engine_generation.is_none_or(|current| provider_generation > current) {
            self.engine
                .begin_provider_session(&provider, provider_generation)
                .map_err(|error| error.to_string())?;
        }
        if newer_session {
            for ((series, _), stop) in &self.history_cancellations {
                if series.provider_id == provider {
                    stop.store(true, Ordering::Release);
                }
            }
            self.catalog
                .retain(|(installed_provider, _), _| installed_provider != &provider);
            self.catalog_sessions
                .insert(provider.clone(), instrument.session_generation);
            self.catalog_selections.remove(&provider);
        }
        self.catalog_selections
            .insert(provider.clone(), instrument.selection_generation);
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

    /// Downgrades provider state when a background catalog refresh fails.
    ///
    /// The retained catalog keeps serving, so this only fires when the
    /// worker-driven session is Online: worker states (connecting,
    /// recovering, failed) always win over catalog staleness.
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
        self.broadcast_provider_for(
            "hyperliquid",
            ProviderConnectionState::Recovering,
            generation,
            Some(detail),
        );
    }

    /// Restores Online after a catalog success that postdates the downgrade.
    ///
    /// The session-generation check keeps a stale success from overriding a
    /// worker-driven reconnect that moved the session on meanwhile.
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
        self.broadcast_provider_for(
            "hyperliquid",
            ProviderConnectionState::Online,
            degraded,
            None,
        );
    }

    pub(super) fn handle_catalog_search(
        &mut self,
        result: axiusflow_engine_protocol::ProviderInstrumentSearchResult,
    ) {
        if result.consumer_id == 0 {
            if result.provider == "hyperliquid" {
                self.select_warm_hyperliquid_instrument(&result);
            } else {
                self.select_warm_rithmic_instrument(&result);
            }
            return;
        }
        let Ok(consumer_id) = id(result.consumer_id).map(ConsumerId) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.catalog_search = Some(envelope::Payload::ProviderInstrumentSearchResult(result));
        }
    }

    pub(super) fn select_warm_rithmic_instrument(
        &self,
        result: &axiusflow_engine_protocol::ProviderInstrumentSearchResult,
    ) {
        if result.search_generation != self.warm_rithmic_search_generation {
            return;
        }
        let Some(warm) = self
            .warm_priority
            .iter()
            .find(|series| series.provider_id == "rithmic")
            .and_then(|series| self.warm_series.get(series))
        else {
            return;
        };
        if !result.instruments.iter().any(|candidate| {
            candidate.symbol == warm.instrument.provider_symbol
                && candidate.exchange == warm.instrument.venue_id
        }) {
            return;
        }
        let _ = self.providers.dispatch_catalog(
            "rithmic",
            ProviderCatalogCommand::Select(SelectProviderInstrument {
                consumer_id: 0,
                selection_generation: result.search_generation,
                search_generation: result.search_generation,
                provider: "rithmic".to_string(),
                symbol: warm.instrument.provider_symbol.clone(),
                exchange: warm.instrument.venue_id.clone(),
                entitlement_id: warm.instrument.entitlement_id.clone(),
            }),
        );
    }

    pub(super) fn select_warm_hyperliquid_instrument(
        &self,
        result: &axiusflow_engine_protocol::ProviderInstrumentSearchResult,
    ) {
        if result.search_generation != self.warm_hyperliquid_search_generation {
            return;
        }
        let Some(warm) = self
            .warm_priority
            .iter()
            .find(|series| series.provider_id == "hyperliquid")
            .and_then(|series| self.warm_series.get(series))
        else {
            return;
        };
        if !result.instruments.iter().any(|candidate| {
            candidate.symbol == warm.instrument.provider_symbol
                && candidate.exchange == warm.instrument.venue_id
        }) {
            return;
        }
        let _ = self.providers.dispatch_catalog(
            "hyperliquid",
            ProviderCatalogCommand::Select(SelectProviderInstrument {
                consumer_id: 0,
                selection_generation: result.search_generation,
                search_generation: result.search_generation,
                provider: "hyperliquid".to_string(),
                symbol: warm.instrument.provider_symbol.clone(),
                exchange: warm.instrument.venue_id.clone(),
                entitlement_id: warm.instrument.entitlement_id.clone(),
            }),
        );
    }

    pub(super) fn handle_catalog_selection(
        &mut self,
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    ) {
        if consumer_id == 0 {
            if instrument.provider == "hyperliquid" {
                let exact = self
                    .warm_priority
                    .iter()
                    .find(|series| series.provider_id == "hyperliquid")
                    .and_then(|series| self.warm_series.get(series))
                    .is_some_and(|warm| {
                        instrument.instrument_id == warm.instrument.instrument_id
                            && instrument.provider_symbol == warm.instrument.provider_symbol
                            && instrument.venue_id == warm.instrument.venue_id
                            && instrument.entitlement_id == warm.instrument.entitlement_id
                    });
                if exact {
                    let _ = self.install_provider_instrument(&instrument);
                }
                return;
            }
            let exact = self
                .warm_priority
                .iter()
                .find(|series| series.provider_id == "rithmic")
                .and_then(|series| self.warm_series.get(series))
                .is_some_and(|warm| {
                    instrument.instrument_id == warm.instrument.instrument_id
                        && instrument.provider_symbol == warm.instrument.provider_symbol
                        && instrument.venue_id == warm.instrument.venue_id
                        && instrument.entitlement_id == warm.instrument.entitlement_id
                });
            if exact {
                let _ = self.install_provider_instrument(&instrument);
            }
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
            if self.resource_mode == ResourceMode::MarketsLive
                && (rejection.provider == "hyperliquid"
                    && self.warm_hyperliquid_search_generation < 3
                    || rejection.provider != "hyperliquid"
                        && self.warm_rithmic_search_generation < 3)
            {
                self.activate_markets_live_hot_set();
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
