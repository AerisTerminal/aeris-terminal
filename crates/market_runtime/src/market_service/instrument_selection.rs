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
    fn ensure_catalog_session(&mut self, provider: &str) -> Result<(), String> {
        if !self.provider_registry.catalog_uses_session(provider) {
            return Ok(());
        }
        if self.ensure_provider_connection(provider)
            && self.provider_registry.catalog_ready(provider)
        {
            Ok(())
        } else {
            Err("Provider catalog session is recovering".into())
        }
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
                self.ensure_catalog_session(&provider)?;
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Search(search))
            });
        if result.is_ok()
            && let Ok(consumer_id) = id(raw_consumer_id).map(ConsumerId)
        {
            self.command_fences
                .record(consumer_id, provider, kind, generation);
            if let Some(events) = self.events.get_mut(&consumer_id) {
                *events.catalog_search_slot(kind) = None;
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
                self.ensure_catalog_session(&provider)?;
                self.providers
                    .dispatch_catalog(&provider, ProviderCatalogCommand::Select(selection))
            });
        if result.is_ok()
            && let Ok(consumer_id) = id(raw_consumer_id).map(ConsumerId)
        {
            self.command_fences
                .record_operation(consumer_id, provider, kind, generation, true);
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
                self.ensure_catalog_session(&provider)?;
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
        self.catalog.insert(key, instrument.clone());
        self.calendar.resolve(instrument, None);
        self.reconcile_order_books();
        Ok(())
    }

    pub(super) fn handle_provider_catalog(&mut self, provider: &str, event: ProviderCatalogEvent) {
        if self
            .sessions
            .get(provider)
            .is_some_and(|session| session.suspended)
        {
            return;
        }
        match event {
            ProviderCatalogEvent::Calendar {
                generation,
                instrument_id,
                status,
                valid_from_utc_ms,
                valid_until_utc_ms,
            } => self.handle_provider_event(super::WireProviderEvent {
                provider: match self.providers.descriptor(provider) {
                    Some(descriptor) => descriptor.id,
                    None => return,
                },
                generation,
                kind: super::ProviderEventKind::Calendar {
                    instrument_id,
                    status,
                    valid_from_utc_ms,
                    valid_until_utc_ms,
                },
            }),
            ProviderCatalogEvent::SearchCompleted(result) => {
                self.restore_provider_catalog_health(&result.provider);
                self.handle_catalog_search(result);
            }
            ProviderCatalogEvent::SearchPreliminary(result) => {
                self.handle_catalog_search_preview(result);
            }
            ProviderCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => {
                self.restore_provider_catalog_health(&instrument.provider);
                self.handle_catalog_selection(consumer_id, command_generation, instrument);
            }
            ProviderCatalogEvent::Rejected {
                rejection,
                selection,
            } => self.handle_catalog_rejection(rejection, selection),
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
                .sessions
                .get(provider)
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
            .sessions
            .get(provider)
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

    pub(super) fn handle_catalog_search_preview(
        &mut self,
        result: aeris_contracts::ProviderInstrumentSearchResult,
    ) {
        if result.consumer_id == 0 {
            return;
        }
        let Ok(consumer_id) = id(result.consumer_id).map(ConsumerId) else {
            return;
        };
        let Some(kind) = self.command_fences.kind_for(
            consumer_id,
            &result.provider,
            result.search_generation,
            false,
        ) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            *events.catalog_search_slot(kind) =
                Some(MarketRuntimeEvent::ProviderInstrumentSearchPreview(result));
        }
    }

    pub(super) fn handle_catalog_search(
        &mut self,
        result: aeris_contracts::ProviderInstrumentSearchResult,
    ) {
        if result.consumer_id == 0 {
            return;
        }
        let Ok(consumer_id) = id(result.consumer_id).map(ConsumerId) else {
            return;
        };
        let Some(kind) = self.command_fences.kind_for(
            consumer_id,
            &result.provider,
            result.search_generation,
            false,
        ) else {
            return;
        };
        self.command_fences.take(
            consumer_id,
            &result.provider,
            kind,
            result.search_generation,
        );
        if let Some(events) = self.events.get_mut(&consumer_id) {
            *events.catalog_search_slot(kind) =
                Some(MarketRuntimeEvent::ProviderInstrumentSearchResult(result));
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
            Err(_) => MarketRuntimeEvent::ProviderCatalogRejected(ProviderCatalogRejected {
                consumer_id,
                provider,
                provider_generation: Some(instrument.session_generation),
                command_generation,
                reason: ProviderCatalogRejectionReason::SubscriptionRejected,
            }),
        };
        if let Some(events) = self.events.get_mut(&id) {
            *events.catalog_selection_slot(kind) = Some(publication);
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
        self.command_fences.take(
            consumer_id,
            &rejection.provider,
            kind,
            rejection.command_generation,
        );
        if let Some(events) = self.events.get_mut(&consumer_id) {
            let slot = if selection {
                events.catalog_selection_slot(kind)
            } else {
                events.catalog_search_slot(kind)
            };
            *slot = Some(MarketRuntimeEvent::ProviderCatalogRejected(rejection));
        }
    }
}
