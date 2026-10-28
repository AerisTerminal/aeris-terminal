use super::descriptors::{
    InstrumentDescriptor, InstrumentRequest, ResolvedSeries, SelectionError, SelectionOutcome,
};
use super::{
    ClientId, ConsumerId, Coordinator, InstallProviderInstrument, MAXIMUM_CATALOG_FIELD_BYTES,
    MAXIMUM_CATALOG_INSTRUMENTS, NonZeroU64, ProviderCatalogCommand, ProviderCatalogEvent,
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderGeneration, ProviderHealth,
    ProviderMarketScreen, Reply, RithmicCatalogControl, ScreenProviderMarkets,
    SearchProviderInstruments, SelectProviderInstrument, SyncSender, TrySendError,
    authorize_consumer,
};
use crate::hyperliquid_realtime::HyperliquidCatalogControl;
use crate::{MarketProviderInstrumentSelection, MarketRuntimeEvent};
use aeris_observability::diagnostic;

pub(super) fn id(value: u64) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| "market identity must be non-zero".to_string())
}

pub(super) fn validate_provider_search(search: &SearchProviderInstruments) -> Result<(), String> {
    id(search.consumer_id)?;
    id(search.search_generation)?;
    if !valid_catalog_field(&search.provider)
        || search.maximum_results == 0
        || usize::try_from(search.maximum_results).unwrap_or(usize::MAX)
            > MAXIMUM_CATALOG_INSTRUMENTS
        || (!search.query.trim().is_empty() && !valid_catalog_field(&search.query))
        || search
            .exchange
            .as_deref()
            .is_some_and(|exchange| !valid_catalog_field(exchange))
    {
        return Err("provider instrument search is invalid".to_string());
    }
    Ok(())
}

pub(super) fn validate_provider_screen(screen: &ScreenProviderMarkets) -> Result<(), String> {
    id(screen.consumer_id)?;
    id(screen.screen_generation)?;
    if !valid_catalog_field(&screen.provider)
        || screen.maximum_results == 0
        || usize::try_from(screen.maximum_results).unwrap_or(usize::MAX)
            > MAXIMUM_CATALOG_INSTRUMENTS
    {
        return Err("provider market screen is invalid".to_string());
    }
    Ok(())
}

pub(super) fn validate_provider_selection(
    selection: &SelectProviderInstrument,
) -> Result<(), String> {
    id(selection.consumer_id)?;
    id(selection.selection_generation)?;
    id(selection.search_generation)?;
    if ![
        &selection.provider,
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

pub(super) fn validate_provider_instrument(
    instrument: &InstallProviderInstrument,
) -> Result<(), String> {
    if instrument.session_generation == 0 || instrument.selection_generation == 0 {
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
    /// Starts a bounded selection. Catalog IO is never performed on the
    /// coordinator: the provider's fenced callback finishes the command.
    pub(super) fn begin_selection(
        &mut self,
        client_id: ClientId,
        request: super::SelectionRequest,
    ) -> Result<(), String> {
        const MAXIMUM_SELECTION_OUTCOMES: usize = 1024;
        const MAXIMUM_SELECTION_EPOCHS: usize = 1024;
        let consumer = ConsumerId(id(request.consumer_id)?);
        authorize_consumer(&self.engine, client_id, consumer)?;
        let generation = super::GenerationId(id(request.generation)?);
        let events = self
            .events
            .get(&consumer)
            .ok_or("consumer outbox is unavailable")?;
        if events.selection_outcomes.len()
            + usize::from(self.pending_selections.contains_key(&consumer))
            >= MAXIMUM_SELECTION_OUTCOMES
        {
            return Err("selection outcome capacity is exhausted".into());
        }
        let seen = self.selection_epochs.entry(consumer).or_default();
        if seen.contains(&request.generation) {
            return Ok(());
        }
        if seen.len() >= MAXIMUM_SELECTION_EPOCHS {
            let _ = seen.pop_first();
        }
        seen.insert(request.generation);
        if self
            .engine
            .current_demand(consumer)
            .and_then(|demand| demand.generation)
            .is_some_and(|active| request.generation < active.0.get())
        {
            self.publish_selection_outcome(
                consumer,
                SelectionOutcome::Failed {
                    generation,
                    error: SelectionError::Superseded,
                },
            );
            return Ok(());
        }
        if let Some((_, old, _)) = self.pending_selections.get(&consumer) {
            if request.generation <= old.generation {
                self.publish_selection_outcome(
                    consumer,
                    SelectionOutcome::Failed {
                        generation,
                        error: SelectionError::Superseded,
                    },
                );
                return Ok(());
            }
            self.retire_pending_selection(consumer)?;
        }
        let default = super::descriptors::default_instrument();
        for (pending_consumer, pending_generation, _) in self
            .command_fences
            .pending_for(&default.provider, super::CommandKind::StartupResolve)
        {
            if pending_consumer == consumer {
                self.command_fences.take(
                    consumer,
                    &default.provider,
                    super::CommandKind::StartupResolve,
                    pending_generation,
                );
            }
        }
        // A known instrument and the ungated public default need no network
        // callback to show a chart. Contract terms must still be resolved
        // asynchronously before the default instrument can be paper traded.
        let provider = match &request.instrument {
            InstrumentRequest::Default => default.provider.clone(),
            InstrumentRequest::Ref(reference) => reference.provider_id.clone(),
            InstrumentRequest::CatalogSymbol { provider_id, .. } => provider_id.clone(),
        };
        if matches!(request.instrument, InstrumentRequest::Default)
            || matches!(&request.instrument, InstrumentRequest::Ref(reference)
                if self.catalog.contains_key(&(reference.provider_id.clone(), reference.instrument_id.clone())))
        {
            let outcome = self.select_and_demand(client_id, &request);
            if matches!(outcome, SelectionOutcome::Selected { .. })
                && matches!(request.instrument, InstrumentRequest::Default)
                && self
                    .catalog
                    .get(&(default.provider.clone(), default.instrument_id.clone()))
                    .is_none_or(|instrument| {
                        super::instrument_terms_from_install(instrument).is_err()
                    })
                && let Err(error) = self.resolve_default_contract_terms(consumer, &request, default)
            {
                diagnostic!("Default market contract terms could not be requested: {error}");
            }
            self.publish_selection_outcome(consumer, outcome);
            return Ok(());
        }
        self.dispatch_unresolved_selection(client_id, consumer, generation, request, provider);
        Ok(())
    }

    fn resolve_default_contract_terms(
        &mut self,
        consumer: ConsumerId,
        request: &super::SelectionRequest,
        default: InstallProviderInstrument,
    ) -> Result<(), String> {
        let provider = default.provider;
        let selection = SelectProviderInstrument {
            consumer_id: request.consumer_id,
            selection_generation: request.generation,
            search_generation: request.generation,
            startup_resolve: true,
            provider: provider.clone(),
            symbol: default.provider_symbol,
            exchange: default.venue_id,
            entitlement_id: default.entitlement_id,
        };
        validate_provider_selection(&selection)?;
        self.providers
            .dispatch_catalog(&provider, ProviderCatalogCommand::Select(selection))?;
        self.command_fences.record_operation(
            consumer,
            provider,
            super::CommandKind::StartupResolve,
            request.generation,
            true,
            self.clock.monotonic(),
        );
        Ok(())
    }

    fn retire_pending_selection(&mut self, consumer: ConsumerId) -> Result<(), String> {
        let Some((_, old, provider)) = self.pending_selections.remove(&consumer) else {
            return Ok(());
        };
        let old_generation = super::GenerationId(id(old.generation)?);
        let kind = if matches!(old.instrument, InstrumentRequest::Default) {
            super::CommandKind::StartupResolve
        } else {
            super::CommandKind::Selection
        };
        let _ = self
            .command_fences
            .take(consumer, &provider, kind, old.generation);
        self.publish_selection_outcome(
            consumer,
            SelectionOutcome::Failed {
                generation: old_generation,
                error: SelectionError::Superseded,
            },
        );
        Ok(())
    }

    fn dispatch_unresolved_selection(
        &mut self,
        client_id: ClientId,
        consumer: ConsumerId,
        generation: super::GenerationId,
        request: super::SelectionRequest,
        provider: String,
    ) {
        if let InstrumentRequest::CatalogSymbol {
            provider_id,
            symbol,
            exchange,
            entitlement_id,
            search_generation,
        } = &request.instrument
        {
            if !self.current_catalog_search(consumer, &provider, *search_generation) {
                self.publish_selection_outcome(
                    consumer,
                    SelectionOutcome::Failed {
                        generation,
                        error: SelectionError::Superseded,
                    },
                );
                return;
            }
            let selection = SelectProviderInstrument {
                consumer_id: request.consumer_id,
                selection_generation: request.generation,
                search_generation: *search_generation,
                startup_resolve: false,
                provider: provider_id.clone(),
                symbol: symbol.clone(),
                exchange: exchange.clone(),
                entitlement_id: entitlement_id.clone(),
            };
            let dispatched = validate_provider_selection(&selection).and_then(|()| {
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Select(selection))
            });
            match dispatched {
                Ok(()) => {
                    self.command_fences.record_operation(
                        consumer,
                        provider.clone(),
                        super::CommandKind::Selection,
                        request.generation,
                        true,
                        self.clock.monotonic(),
                    );
                    self.pending_selections
                        .insert(consumer, (client_id, request, provider));
                }
                Err(error) => self.publish_selection_outcome(
                    consumer,
                    SelectionOutcome::Failed {
                        generation,
                        error: SelectionError::DemandRejected(error),
                    },
                ),
            }
            return;
        }
        let InstrumentRequest::Ref(reference) = &request.instrument else {
            unreachable!("default resolved before catalog dispatch")
        };
        let query = reference.query.clone().unwrap_or_else(|| {
            reference
                .instrument_id
                .rsplit(':')
                .next()
                .unwrap_or(&reference.instrument_id)
                .to_owned()
        });
        let kind = super::CommandKind::Selection;
        let search = SearchProviderInstruments {
            consumer_id: request.consumer_id,
            search_generation: request.generation,
            startup_resolve: false,
            provider: provider.clone(),
            query,
            maximum_results: 32,
            categories: crate::InstrumentSearchCategories::ALL,
            exchange: None,
        };
        let dispatched = self
            .providers
            .dispatch_catalog(&provider, ProviderCatalogCommand::Search(search));
        match dispatched {
            Ok(()) => {
                self.command_fences.record(
                    consumer,
                    provider.clone(),
                    kind,
                    request.generation,
                    self.clock.monotonic(),
                );
                self.pending_selections
                    .insert(consumer, (client_id, request, provider));
            }
            Err(error) => self.publish_selection_outcome(
                consumer,
                SelectionOutcome::Failed {
                    generation,
                    error: SelectionError::DemandRejected(error),
                },
            ),
        }
    }

    pub(super) fn publish_catalog_ready(&mut self, provider: &str) {
        for events in self.events.values_mut() {
            events.catalog_ready = Some(MarketRuntimeEvent::CatalogReady {
                provider: provider.to_owned(),
            });
        }
    }

    fn current_catalog_search(
        &self,
        consumer: ConsumerId,
        provider: &str,
        search_generation: u64,
    ) -> bool {
        search_generation == 0
            || self.command_fences.accepts(
                consumer,
                provider,
                super::CommandKind::Search,
                search_generation,
            )
    }

    fn publish_selection_outcome(&mut self, consumer: ConsumerId, outcome: SelectionOutcome) {
        if let SelectionOutcome::Failed { error, .. } = &outcome {
            diagnostic!(
                "Market selection failed for consumer {}: {error:?}",
                consumer.0.get()
            );
        }
        if let Some(events) = self.events.get_mut(&consumer) {
            events
                .selection_outcomes
                .push_back(MarketRuntimeEvent::SelectionOutcome(outcome));
        }
    }

    pub(super) fn select_and_demand(
        &mut self,
        client_id: ClientId,
        request: &super::SelectionRequest,
    ) -> SelectionOutcome {
        let generation = super::GenerationId(
            super::NonZeroU64::new(request.generation).expect("validated selection generation"),
        );
        let consumer_id = ConsumerId(
            super::NonZeroU64::new(request.consumer_id).expect("validated selection consumer"),
        );
        let previous_demand = self.engine.current_demand(consumer_id).cloned();
        let previous_events = self.events.get(&consumer_id).cloned();
        let outcome = self
            .resolve_selection(client_id, consumer_id, request)
            .and_then(|(descriptor, series)| {
                if matches!(request.instrument, InstrumentRequest::Default)
                    && !self.catalog.contains_key(&(
                        descriptor.instrument.provider.clone(),
                        descriptor.instrument.instrument_id.clone(),
                    ))
                {
                    self.install_provider_instrument(&descriptor.instrument)
                        .map_err(SelectionError::DemandRejected)?;
                    self.provisional_default = Some((
                        descriptor.instrument.provider.clone(),
                        descriptor.instrument.instrument_id.clone(),
                    ));
                }
                let (reply, received) = std::sync::mpsc::sync_channel(1);
                self.handle_demand(
                    client_id,
                    &series.key,
                    request.streams,
                    super::DemandWaiter {
                        consumer_id,
                        generation,
                        started_at: std::time::Instant::now(),
                    },
                    &reply,
                );
                received
                    .recv()
                    .map_err(|_| SelectionError::DemandRejected("selection reply lost".into()))?
                    .map_err(SelectionError::DemandRejected)?;
                if let Some(viewport) = request.viewport {
                    self.engine
                        .set_viewport(consumer_id, generation, viewport)
                        .map_err(|error| SelectionError::DemandRejected(error.to_string()))?;
                    // Viewport history is an optional paged request. Its failure
                    // never revokes a successfully installed live series.
                    if let Err(error) =
                        self.request_viewport_history(consumer_id, generation, viewport)
                    {
                        aeris_observability::diagnostic!(
                            "Market viewport backfill deferred: {error}"
                        );
                    }
                }
                Ok((descriptor, series))
            });
        match outcome {
            Ok((descriptor, series)) => SelectionOutcome::Selected {
                generation,
                descriptor: Box::new(descriptor),
                series,
            },
            Err(error) => {
                self.rollback_selection(consumer_id, generation, previous_demand, previous_events);
                SelectionOutcome::Failed { generation, error }
            }
        }
    }

    fn resolve_selection(
        &self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        request: &super::SelectionRequest,
    ) -> Result<(InstrumentDescriptor, ResolvedSeries), SelectionError> {
        authorize_consumer(&self.engine, client_id, consumer_id)
            .map_err(SelectionError::DemandRejected)?;
        if request
            .viewport
            .is_some_and(|viewport| viewport.start_unix_nanos >= viewport.end_unix_nanos)
        {
            return Err(SelectionError::InvalidViewport);
        }
        let instrument = match &request.instrument {
            InstrumentRequest::Ref(reference) => self
                .catalog
                .get(&(
                    reference.provider_id.clone(),
                    reference.instrument_id.clone(),
                ))
                .filter(|instrument| instrument.entitlement_id == reference.entitlement_id)
                .cloned(),
            InstrumentRequest::Default => {
                let mut default = super::descriptors::default_instrument();
                if let Some(epoch) = self
                    .engine
                    .provider_status(&default.provider)
                    .and_then(|status| status.generation)
                {
                    default.session_generation = epoch.0.get();
                }
                Some(
                    self.catalog
                        .get(&(default.provider.clone(), default.instrument_id.clone()))
                        .cloned()
                        .unwrap_or(default),
                )
            }
            InstrumentRequest::CatalogSymbol { .. } => None,
        }
        .ok_or(SelectionError::InstrumentUnavailable)?;
        let provider = self
            .providers
            .descriptor(&instrument.provider)
            .ok_or(SelectionError::InstrumentUnavailable)?;
        let series = ResolvedSeries::resolve(&instrument, request.interval, provider.presentation)
            .map_err(|_| SelectionError::UnsupportedInterval)?;
        let descriptor = InstrumentDescriptor::from_install(
            &instrument,
            provider.presentation,
            provider.capabilities.quotes.is_some(),
        )
        .map_err(|_| SelectionError::InstrumentUnavailable)?;
        Ok((descriptor, series))
    }

    fn rollback_selection(
        &mut self,
        consumer_id: ConsumerId,
        generation: super::GenerationId,
        previous_demand: Option<aeris_market_engine::ConsumerDemand>,
        previous_events: Option<super::ConsumerEvents>,
    ) {
        let Some(previous) = previous_demand else {
            return;
        };
        if !self
            .engine
            .current_demand(consumer_id)
            .is_some_and(|current| {
                current.generation == Some(generation) && previous.generation != Some(generation)
            })
        {
            return;
        }
        if let Err(error) = self
            .engine
            .rollback_series_demand(consumer_id, generation, &previous)
        {
            aeris_observability::diagnostic!("Market selection rollback failed: {error}");
            return;
        }
        if let (Some(previous_events), Some(events)) =
            (previous_events, self.events.get_mut(&consumer_id))
        {
            *events = previous_events;
        }
        self.prune_unused_live_series();
        self.prune_history_tracking();
        self.reconcile_order_books();
    }

    pub(super) fn handle_provider_search(
        &mut self,
        client_id: ClientId,
        search: SearchProviderInstruments,
        reply: &Reply<()>,
    ) {
        let raw_consumer_id = search.consumer_id;
        let provider = search.provider.clone();
        let generation = search.search_generation;
        let kind = if search.startup_resolve {
            super::CommandKind::StartupResolve
        } else {
            super::CommandKind::Search
        };
        let result = self
            .authorize_catalog_consumer(client_id, raw_consumer_id)
            .and_then(|()| {
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Search(search))
            });
        if result.is_ok()
            && let Ok(consumer_id) = id(raw_consumer_id).map(ConsumerId)
        {
            self.command_fences.record(
                consumer_id,
                provider.clone(),
                kind,
                generation,
                self.clock.monotonic(),
            );
            if let Some(events) = self.events.get_mut(&consumer_id) {
                events.set_catalog_search(kind, &provider, None);
            }
        }
        let _ = reply.send(result);
    }

    pub(super) fn handle_provider_selection(
        &mut self,
        client_id: ClientId,
        selection: SelectProviderInstrument,
        reply: &Reply<()>,
    ) {
        let raw_consumer_id = selection.consumer_id;
        let provider = selection.provider.clone();
        let generation = selection.selection_generation;
        let kind = if selection.startup_resolve {
            super::CommandKind::StartupResolve
        } else {
            super::CommandKind::Select
        };
        let result = self
            .authorize_catalog_consumer(client_id, raw_consumer_id)
            .and_then(|()| {
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Select(selection))
            });
        if result.is_ok()
            && let Ok(consumer_id) = id(raw_consumer_id).map(ConsumerId)
        {
            self.command_fences.record_operation(
                consumer_id,
                provider,
                kind,
                generation,
                true,
                self.clock.monotonic(),
            );
            if let Some(events) = self.events.get_mut(&consumer_id) {
                *events.catalog_selection_slot(kind) = None;
            }
        }
        let _ = reply.send(result);
    }

    pub(super) fn handle_provider_screen(
        &mut self,
        client_id: ClientId,
        screen: ScreenProviderMarkets,
        reply: &Reply<()>,
    ) {
        let raw_consumer_id = screen.consumer_id;
        let provider = screen.provider.clone();
        let generation = screen.screen_generation;
        let result = self
            .authorize_catalog_consumer(client_id, raw_consumer_id)
            .and_then(|()| {
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Screen(screen))
            });
        if result.is_ok()
            && let Ok(consumer_id) = id(raw_consumer_id).map(ConsumerId)
        {
            self.command_fences.record(
                consumer_id,
                provider,
                super::CommandKind::Screen,
                generation,
                self.clock.monotonic(),
            );
            if let Some(events) = self.events.get_mut(&consumer_id) {
                events.catalog_screen = None;
            }
        }
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
                && self.provisional_default.as_ref() != Some(&key)
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
        if self.provisional_default.as_ref() == Some(&key) {
            self.provisional_default = None;
        }
        self.catalog.insert(key, instrument.clone());
        self.calendar.resolve(instrument);
        // A chart-free instrument lease (for example trading ticket interest) is
        // installed before its route resolves, so a demand flush while the catalog
        // entry was missing cleared the session's dirty flag. This install unblocks
        // that demand; mark the session dirty so the active turn pushes it.
        if self.engine.instrument_subscriptions().any(|(stream, _)| {
            stream.provider == provider && stream.instrument.as_str() == instrument.instrument_id
        }) {
            self.session_mut(&provider).demand_dirty = true;
        }
        if self.trading_feed.attached()
            && let Ok(terms) = super::instrument_terms_from_install(instrument)
        {
            self.trading_feed
                .publish(aeris_trading::market::MarketFeedEvent::Terms(terms));
        }
        self.reconcile_order_books();
        Ok(())
    }

    fn bind_catalog_instrument_session(&self, instrument: &mut InstallProviderInstrument) {
        if self
            .providers
            .descriptor(&instrument.provider)
            .is_none_or(|descriptor| {
                descriptor.capabilities.catalog_session_binding
                    != super::CatalogSessionBinding::CurrentSession
            })
        {
            return;
        }
        let engine_session = self
            .engine
            .provider_status(&instrument.provider)
            .and_then(|status| status.generation)
            .map(|generation| generation.0.get());
        let catalog_session = self.catalog_sessions.get(&instrument.provider).copied();
        if let Some(current) = engine_session.into_iter().chain(catalog_session).max() {
            instrument.session_generation = instrument.session_generation.max(current);
        }
    }

    pub(super) fn handle_provider_catalog(&mut self, provider: &str, event: ProviderCatalogEvent) {
        match event {
            ProviderCatalogEvent::SearchCompleted(result) => {
                self.restore_provider_catalog_health(&result.provider);
                if result.consumer_id == 0 {
                    self.finish_trading_route_search(&result);
                } else {
                    self.handle_catalog_search(result);
                }
            }
            ProviderCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                mut instrument,
            } => {
                self.restore_provider_catalog_health(&instrument.provider);
                self.bind_catalog_instrument_session(&mut instrument);
                if consumer_id == 0 {
                    self.finish_trading_route_resolution(command_generation, &instrument);
                    return;
                }
                if !self.finish_selection(consumer_id, command_generation, &instrument) {
                    self.handle_catalog_selection(consumer_id, command_generation, instrument);
                }
            }
            ProviderCatalogEvent::Rejected {
                rejection,
                selection,
            } => {
                if rejection.consumer_id == 0 && selection {
                    self.reject_trading_route_resolution(
                        &rejection.provider,
                        rejection.command_generation,
                    );
                    self.control_retry = true;
                } else {
                    self.handle_catalog_rejection(rejection, selection);
                }
            }
            ProviderCatalogEvent::RefreshFailed(detail) => {
                self.degrade_provider_catalog_health(provider, &detail);
            }
            ProviderCatalogEvent::ScreenCompleted(screen) => self.handle_catalog_screen(screen),
            ProviderCatalogEvent::ScreenRejected(rejection) => {
                self.handle_catalog_screen_rejection(rejection);
            }
        }
    }

    /// Takes the pending screen generation for the consumer/provider pair when it
    /// matches exactly; superseded or unrequested screens are dropped.
    fn take_pending_screen(
        &mut self,
        consumer_id: u64,
        provider: &str,
        generation: u64,
    ) -> Option<ConsumerId> {
        let consumer_id = id(consumer_id).map(ConsumerId).ok()?;
        self.command_fences.take(
            consumer_id,
            provider,
            super::CommandKind::Screen,
            generation,
        )?;
        Some(consumer_id)
    }

    pub(super) fn handle_catalog_screen(&mut self, screen: ProviderMarketScreen) {
        let Some(consumer_id) = self.take_pending_screen(
            screen.consumer_id,
            &screen.provider,
            screen.screen_generation,
        ) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.catalog_screen = Some(MarketRuntimeEvent::ProviderMarketScreen(screen));
        }
    }

    pub(super) fn handle_catalog_screen_rejection(&mut self, rejection: ProviderCatalogRejected) {
        let Some(consumer_id) = self.take_pending_screen(
            rejection.consumer_id,
            &rejection.provider,
            rejection.command_generation,
        ) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.catalog_screen =
                Some(MarketRuntimeEvent::ProviderMarketScreenRejected(rejection));
        }
    }

    /// Records a background catalog refresh failure without changing realtime
    /// transport health. The retained catalog remains usable, so a catalog
    /// problem must never paint an online WebSocket as reconnecting.
    pub(super) fn degrade_provider_catalog_health(&mut self, provider: &str, detail: &str) {
        let current = self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation);
        let Some(generation) = current else {
            return;
        };
        let healthy = self
            .engine
            .provider_status(provider)
            .is_some_and(|status| status.health == ProviderHealth::Online);
        if !healthy
            || self
                .provider_registry
                .has_control(provider)
                .then(|| self.session(provider))
                .is_none_or(|session| session.catalog_degraded.is_some())
        {
            return;
        }
        self.session_mut(provider).catalog_degraded = Some(generation);
        diagnostic!("Aeris engine {provider} catalog refresh degraded: {detail}");
    }

    /// Clears catalog degradation after a success from the same provider
    /// generation. This intentionally publishes no provider state: only the
    /// realtime worker owns transport Online/Recovering transitions.
    pub(super) fn restore_provider_catalog_health(&mut self, provider: &str) {
        let Some(degraded) = self
            .provider_registry
            .has_control(provider)
            .then(|| self.session(provider))
            .and_then(|session| session.catalog_degraded)
        else {
            return;
        };
        let current = self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation);
        if current != Some(degraded) {
            self.session_mut(provider).catalog_degraded = None;
            return;
        }
        self.session_mut(provider).catalog_degraded = None;
    }

    pub(super) fn handle_catalog_search(&mut self, result: crate::ProviderInstrumentSearchResult) {
        if result.consumer_id == 0 {
            return;
        }
        let Ok(consumer_id) = id(result.consumer_id).map(ConsumerId) else {
            return;
        };
        if self
            .pending_selections
            .get(&consumer_id)
            .is_some_and(|(_, request, provider)| {
                request.generation == result.search_generation && *provider == result.provider
            })
        {
            if self
                .command_fences
                .take(
                    consumer_id,
                    &result.provider,
                    super::CommandKind::Selection,
                    result.search_generation,
                )
                .is_none()
            {
                return;
            }
            let Some((_, request, _)) = self.pending_selections.get(&consumer_id) else {
                return;
            };
            let InstrumentRequest::Ref(reference) = &request.instrument else {
                return;
            };
            let query = reference
                .query
                .as_deref()
                .unwrap_or_else(|| reference.instrument_id.rsplit(':').next().unwrap_or(""));
            let normalized = query.strip_prefix('/').unwrap_or(query);
            let selected = result.instruments.iter().find(|candidate| {
                [candidate.symbol.as_str(), candidate.display_symbol.as_str()]
                    .into_iter()
                    .any(|value| {
                        value == query || value.strip_prefix('/').unwrap_or(value) == normalized
                    })
            });
            let dispatched = selected
                .ok_or_else(|| "instrument was not found in provider catalog".to_owned())
                .and_then(|candidate| {
                    self.providers.dispatch_catalog(
                        &result.provider,
                        ProviderCatalogCommand::Select(SelectProviderInstrument {
                            consumer_id: result.consumer_id,
                            selection_generation: result.search_generation,
                            search_generation: result.search_generation,
                            startup_resolve: false,
                            provider: result.provider.clone(),
                            symbol: candidate.symbol.clone(),
                            exchange: candidate.exchange.clone(),
                            entitlement_id: reference.entitlement_id.clone(),
                        }),
                    )
                });
            match dispatched {
                Ok(()) => {
                    self.command_fences.record_operation(
                        consumer_id,
                        result.provider,
                        super::CommandKind::Selection,
                        result.search_generation,
                        true,
                        self.clock.monotonic(),
                    );
                }
                Err(error) => {
                    self.fail_pending_selection(consumer_id, SelectionError::DemandRejected(error));
                }
            }
            return;
        }
        let Some(kind) = self.command_fences.kind_for(
            consumer_id,
            &result.provider,
            result.search_generation,
            false,
        ) else {
            return;
        };
        self.complete_catalog_search_fence(
            consumer_id,
            &result.provider,
            kind,
            result.search_generation,
        );
        if let Some(events) = self.events.get_mut(&consumer_id) {
            let provider = result.provider.clone();
            events.set_catalog_search(
                kind,
                &provider,
                Some(MarketRuntimeEvent::ProviderInstrumentSearchResult(result)),
            );
        }
    }

    fn complete_catalog_search_fence(
        &mut self,
        consumer: ConsumerId,
        provider: &str,
        kind: super::CommandKind,
        generation: u64,
    ) {
        if kind == super::CommandKind::Search {
            // Keep the identity until replacement or retirement for a chart
            // selection, but never publish a duplicate search response.
            self.command_fences
                .complete_search(consumer, provider, generation);
        } else {
            self.command_fences
                .take(consumer, provider, kind, generation);
        }
    }

    pub(super) fn handle_catalog_selection(
        &mut self,
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    ) {
        if consumer_id == 0 {
            return;
        }
        let Ok(id) = id(consumer_id).map(ConsumerId) else {
            return;
        };
        let provider = instrument.provider.clone();
        let Some(kind) = self
            .command_fences
            .kind_for(id, &provider, command_generation, true)
        else {
            return;
        };
        self.command_fences
            .take(id, &provider, kind, command_generation);
        let publication = match self.install_provider_instrument(&instrument) {
            Ok(()) => {
                MarketRuntimeEvent::ProviderInstrumentSelection(MarketProviderInstrumentSelection {
                    consumer_id: id,
                    instrument,
                    command_generation,
                })
            }
            Err(error) => {
                diagnostic!("Market catalog selection from {provider} could not install: {error}");
                MarketRuntimeEvent::ProviderCatalogRejected {
                    rejection: ProviderCatalogRejected {
                        consumer_id,
                        provider,
                        provider_generation: Some(instrument.session_generation),
                        command_generation,
                        reason: ProviderCatalogRejectionReason::SubscriptionRejected,
                    },
                    selection: true,
                }
            }
        };
        if let Some(events) = self.events.get_mut(&id) {
            *events.catalog_selection_slot(kind) = Some(publication);
        }
    }

    fn finish_selection(
        &mut self,
        raw_consumer: u64,
        generation: u64,
        instrument: &InstallProviderInstrument,
    ) -> bool {
        let Ok(consumer) = id(raw_consumer).map(ConsumerId) else {
            return false;
        };
        let Some((client, request, provider)) = self.pending_selections.get(&consumer).cloned()
        else {
            return false;
        };
        if provider != instrument.provider
            || request.generation != generation
            || !self.command_fences.accepts(
                consumer,
                &provider,
                super::CommandKind::Selection,
                generation,
            )
        {
            return false;
        }
        self.command_fences.take(
            consumer,
            &provider,
            super::CommandKind::Selection,
            generation,
        );
        self.pending_selections.remove(&consumer);
        let expected_entitlement = match &request.instrument {
            InstrumentRequest::Ref(reference) => &reference.entitlement_id,
            InstrumentRequest::CatalogSymbol { entitlement_id, .. } => entitlement_id,
            InstrumentRequest::Default => return true,
        };
        let outcome = if matches!(&request.instrument, InstrumentRequest::Ref(reference)
            if instrument.instrument_id != reference.instrument_id)
            || instrument.entitlement_id != *expected_entitlement
        {
            SelectionOutcome::Failed {
                generation: super::GenerationId(id(generation).expect("validated selection epoch")),
                error: SelectionError::InstrumentUnavailable,
            }
        } else {
            match self.install_provider_instrument(instrument) {
                Ok(()) => {
                    let mut resolved = request;
                    resolved.instrument =
                        InstrumentRequest::Ref(super::descriptors::InstrumentRef {
                            provider_id: instrument.provider.clone(),
                            instrument_id: instrument.instrument_id.clone(),
                            entitlement_id: instrument.entitlement_id.clone(),
                            query: None,
                        });
                    self.select_and_demand(client, &resolved)
                }
                Err(error) => SelectionOutcome::Failed {
                    generation: super::GenerationId(
                        id(generation).expect("validated selection epoch"),
                    ),
                    error: SelectionError::DemandRejected(error),
                },
            }
        };
        self.publish_selection_outcome(consumer, outcome);
        true
    }

    fn fail_pending_selection(&mut self, consumer: ConsumerId, error: SelectionError) {
        if let Some((_, request, provider)) = self.pending_selections.remove(&consumer) {
            let _ = self.command_fences.take(
                consumer,
                &provider,
                super::CommandKind::Selection,
                request.generation,
            );
            self.publish_selection_outcome(
                consumer,
                SelectionOutcome::Failed {
                    generation: super::GenerationId(
                        id(request.generation).expect("validated selection epoch"),
                    ),
                    error,
                },
            );
        }
    }

    pub(super) fn reject_overflowed_catalog(&mut self, provider: &str) {
        let screens = self
            .command_fences
            .pending_for(provider, super::CommandKind::Screen);
        for (consumer_id, command_generation, _) in screens {
            self.handle_catalog_screen_rejection(ProviderCatalogRejected {
                consumer_id: consumer_id.0.get(),
                provider: provider.to_string(),
                provider_generation: None,
                command_generation,
                reason: ProviderCatalogRejectionReason::DispatchUnavailable,
            });
        }
        for kind in [
            super::CommandKind::Search,
            super::CommandKind::Select,
            super::CommandKind::Selection,
            super::CommandKind::StartupResolve,
        ] {
            for (consumer_id, command_generation, selection) in
                self.command_fences.pending_for(provider, kind)
            {
                self.handle_catalog_rejection(
                    ProviderCatalogRejected {
                        consumer_id: consumer_id.0.get(),
                        provider: provider.to_string(),
                        provider_generation: None,
                        command_generation,
                        reason: ProviderCatalogRejectionReason::DispatchUnavailable,
                    },
                    selection,
                );
            }
        }
    }

    /// Rejects every search and selection whose provider callback is overdue,
    /// so a lost callback reaches its consumer as an explicit timeout.
    pub(super) fn expire_catalog_commands(&mut self) {
        for expired in self.command_fences.expired(self.clock.monotonic()) {
            let (operation, reason) = if expired.selection {
                (
                    "selection",
                    ProviderCatalogRejectionReason::SelectionTimedOut,
                )
            } else {
                ("search", ProviderCatalogRejectionReason::SearchTimedOut)
            };
            aeris_observability::diagnostic!(
                "Market catalog {operation} timed out for provider {}",
                expired.provider
            );
            self.reject_catalog_command(
                expired.consumer,
                expired.kind,
                ProviderCatalogRejected {
                    consumer_id: expired.consumer.0.get(),
                    provider: expired.provider.clone(),
                    provider_generation: None,
                    command_generation: expired.wire_generation,
                    reason,
                },
                expired.selection,
            );
            // An overdue fence must never stay armed, or its past deadline
            // would wake the coordinator continuously.
            self.command_fences.take(
                expired.consumer,
                &expired.provider,
                expired.kind,
                expired.wire_generation,
            );
        }
    }

    pub(super) fn handle_catalog_rejection(
        &mut self,
        rejection: ProviderCatalogRejected,
        selection: bool,
    ) {
        if rejection.consumer_id == 0 {
            return;
        }
        let Ok(consumer_id) = id(rejection.consumer_id).map(ConsumerId) else {
            return;
        };
        let Some(kind) = self.command_fences.kind_for(
            consumer_id,
            &rejection.provider,
            rejection.command_generation,
            selection,
        ) else {
            return;
        };
        diagnostic!(
            "Market catalog {} rejected by {}: {:?}",
            if selection { "selection" } else { "search" },
            rejection.provider,
            rejection.reason
        );
        self.reject_catalog_command(consumer_id, kind, rejection, selection);
    }

    fn reject_catalog_command(
        &mut self,
        consumer_id: ConsumerId,
        kind: super::CommandKind,
        rejection: ProviderCatalogRejected,
        selection: bool,
    ) {
        if kind == super::CommandKind::Selection {
            self.fail_pending_selection(
                consumer_id,
                SelectionError::DemandRejected(format!("{:?}", rejection.reason)),
            );
            return;
        }
        self.command_fences.take(
            consumer_id,
            &rejection.provider,
            kind,
            rejection.command_generation,
        );
        if let Some(events) = self.events.get_mut(&consumer_id) {
            if selection {
                *events.catalog_selection_slot(kind) =
                    Some(MarketRuntimeEvent::ProviderCatalogRejected {
                        rejection,
                        selection,
                    });
            } else {
                let provider = rejection.provider.clone();
                events.set_catalog_search(
                    kind,
                    &provider,
                    Some(MarketRuntimeEvent::ProviderCatalogRejected {
                        rejection,
                        selection,
                    }),
                );
            }
        }
    }
}
