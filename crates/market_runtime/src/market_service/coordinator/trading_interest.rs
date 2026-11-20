use super::{
    BTreeSet, Coordinator, InstallProviderInstrument, MarketDataLeaseId, MarketStream,
    ProviderHealth, Reply, StreamRequirements,
};
use aeris_observability::diagnostic;

impl Coordinator<'_> {
    pub(super) fn acquire_quote(
        &mut self,
        key: &super::super::InstrumentStreamKey,
        reply: &Reply<MarketDataLeaseId>,
    ) {
        match self
            .engine
            .acquire_instrument_lease(key, StreamRequirements::NONE.with(MarketStream::Quotes))
        {
            Ok(id) => {
                self.reconcile_order_books();
                self.mark_alert_demand_changed();
                self.pending_activity = true;
                self.publish_trading_session(key);
                if reply.send(Ok(id)).is_err() {
                    let _ = self.engine.release_data_lease(id);
                    self.reconcile_order_books();
                }
            }
            Err(error) => {
                let _ = reply.send(Err(error.to_string()));
            }
        }
    }

    pub(super) fn install_trading_interest(
        &mut self,
        interest: &aeris_trading::market::QuoteInterest,
    ) -> Result<(), String> {
        interest.validate().map_err(|error| error.to_string())?;
        if interest.revision <= self.trading_interest_revision {
            return Err("trading quote interest revision is stale".into());
        }
        let keys = interest
            .routes
            .iter()
            .map(|route| super::super::InstrumentStreamKey {
                provider: route.provider_id.clone(),
                instrument: route.instrument_id.clone(),
                entitlement: route.entitlement_id.clone(),
            })
            .collect::<BTreeSet<_>>();
        self.engine
            .set_instrument_leases(
                aeris_market_engine::LeaseOwner::Trading,
                keys,
                StreamRequirements::NONE.with(MarketStream::Quotes),
            )
            .map_err(|error| error.to_string())?;
        self.trading_interest_revision = interest.revision;
        self.trading_routes = interest
            .routes
            .iter()
            .map(|route| {
                (
                    (
                        route.provider_id.clone(),
                        route.instrument_id.as_str().to_string(),
                    ),
                    route.clone(),
                )
            })
            .collect();
        self.pending_trading_routes
            .retain(|key, _| self.trading_routes.contains_key(key));
        self.resolve_trading_routes();
        self.reconcile_order_books();
        // A new trading lease can be installed after initial feed attachment while
        // the provider is already online. Snapshot its current feed state and quote
        // now, rather than waiting for another provider-state transition.
        if self.trading_feed.attached() {
            self.publish_trading_feed_snapshot();
        }
        for route in &interest.routes {
            self.publish_trading_session(&super::super::InstrumentStreamKey {
                provider: route.provider_id.clone(),
                instrument: route.instrument_id.clone(),
                entitlement: route.entitlement_id.clone(),
            });
        }
        self.mark_alert_demand_changed();
        self.pending_activity = true;
        Ok(())
    }

    /// A restored instrument lease cannot drive a provider until its catalog
    /// entry has been resolved. Never synthesize terms from the stored route.
    pub(super) fn resolve_trading_routes(&mut self) {
        let routes = self.trading_routes.values().cloned().collect::<Vec<_>>();
        for route in routes {
            let key = (
                route.provider_id.clone(),
                route.instrument_id.as_str().to_string(),
            );
            if self.catalog.get(&key).is_some_and(|installed| {
                installed.provider_symbol == route.provider_symbol
                    && installed.venue_id == route.venue_id
                    && installed.entitlement_id.as_str()
                        == route.entitlement_id.as_deref().unwrap_or("")
                    && super::super::instrument_terms_from_install(installed).is_ok()
            }) {
                self.pending_trading_routes.remove(&key);
                continue;
            }
            let current = self
                .engine
                .provider_status(&route.provider_id)
                .and_then(|status| status.generation)
                .map(|generation| generation.0.get());
            if self
                .pending_trading_routes
                .get(&key)
                .is_some_and(|(_, generation, _)| current.is_none() || *generation == current)
            {
                continue;
            }
            let generation = self.next_trading_route_command.saturating_add(1);
            if generation == self.next_trading_route_command {
                self.control_retry = true;
                continue;
            }
            // Some providers require a search result before selection; the
            // bounded search also proves the saved route still exists.
            let search = super::super::SearchProviderInstruments {
                consumer_id: 0, // Reserved for the market owner's lease restoration.
                search_generation: generation,
                startup_resolve: true,
                provider: route.provider_id.clone(),
                query: route.provider_symbol.clone(),
                maximum_results: u32::try_from(super::super::MAXIMUM_CATALOG_INSTRUMENTS)
                    .unwrap_or(u32::MAX),
                categories: crate::InstrumentSearchCategories::ALL,
                exchange: None,
            };
            if let Err(error) = self.providers.dispatch_catalog(
                &route.provider_id,
                super::super::ProviderCatalogCommand::Search(search),
            ) {
                diagnostic!("Trading route catalog resolution deferred: {error}");
                self.control_retry = true;
                continue;
            }
            self.next_trading_route_command = generation;
            self.pending_trading_routes
                .insert(key, (generation, current, false));
        }
    }

    pub(in super::super) fn finish_trading_route_search(
        &mut self,
        result: &crate::ProviderInstrumentSearchResult,
    ) {
        let route = self.trading_routes.values().find(|route| {
            result.provider_generation != 0
                && route.provider_id == result.provider
                && self
                    .pending_trading_routes
                    .get(&(
                        route.provider_id.clone(),
                        route.instrument_id.as_str().to_string(),
                    ))
                    .is_some_and(|(generation, expected, sent)| {
                        *generation == result.search_generation
                            && !sent
                            && expected
                                .is_none_or(|generation| generation == result.provider_generation)
                    })
        });
        let Some(route) = route.cloned() else {
            return;
        };
        if !result.instruments.iter().any(|instrument| {
            instrument.symbol == route.provider_symbol && instrument.exchange == route.venue_id
        }) {
            self.reject_trading_route_resolution(&route.provider_id, result.search_generation);
            self.control_retry = true;
            return;
        }
        let selection = super::super::SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: result.search_generation,
            search_generation: result.search_generation,
            startup_resolve: true,
            provider: route.provider_id.clone(),
            symbol: route.provider_symbol.clone(),
            exchange: route.venue_id.clone(),
            entitlement_id: route.entitlement_id.clone().unwrap_or_default(),
        };
        match self.providers.dispatch_catalog(
            &route.provider_id,
            super::super::ProviderCatalogCommand::Select(selection),
        ) {
            Ok(()) => {
                if let Some(pending) = self
                    .pending_trading_routes
                    .get_mut(&(route.provider_id, route.instrument_id.as_str().to_string()))
                {
                    pending.1 = Some(result.provider_generation);
                    pending.2 = true;
                }
            }
            Err(error) => {
                diagnostic!("Trading route selection deferred: {error}");
                self.reject_trading_route_resolution(&result.provider, result.search_generation);
                self.control_retry = true;
            }
        }
    }

    pub(in super::super) fn finish_trading_route_resolution(
        &mut self,
        command_generation: u64,
        instrument: &InstallProviderInstrument,
    ) {
        let key = (
            instrument.provider.clone(),
            instrument.instrument_id.clone(),
        );
        let Some(route) = self.trading_routes.get(&key) else {
            return;
        };
        let Some(&(pending, expected_generation, selection_sent)) =
            self.pending_trading_routes.get(&key)
        else {
            return;
        };
        let current_generation = self
            .engine
            .provider_status(&route.provider_id)
            .and_then(|status| status.generation)
            .map(|generation| generation.0.get());
        if !selection_sent
            || pending != command_generation
            || expected_generation != Some(instrument.session_generation)
            || current_generation
                .is_some_and(|generation| generation != instrument.session_generation)
            || route.provider_symbol != instrument.provider_symbol
            || route.venue_id != instrument.venue_id
            || route.entitlement_id.as_deref() != Some(instrument.entitlement_id.as_str())
            || route.price_scale != u8::try_from(instrument.price_scale).unwrap_or(u8::MAX)
            || route.quantity_scale != u8::try_from(instrument.quantity_scale).unwrap_or(u8::MAX)
        {
            return;
        }
        if super::super::instrument_terms_from_install(instrument).is_ok()
            && self.install_provider_instrument(instrument).is_ok()
        {
            self.pending_trading_routes.remove(&key);
        }
    }

    pub(in super::super) fn reject_trading_route_resolution(
        &mut self,
        provider: &str,
        generation: u64,
    ) {
        self.pending_trading_routes
            .retain(|key, (pending, _, _)| key.0 != provider || *pending != generation);
    }

    pub(in super::super) fn publish_trading_session(
        &self,
        key: &super::super::InstrumentStreamKey,
    ) {
        use aeris_instruments::CalendarSourceKind;
        use aeris_trading::market::{InstrumentSession, MarketFeedEvent};

        if !self.trading_feed.attached() {
            return;
        }
        let Some(provider_generation) = self
            .engine
            .provider_status(&key.provider)
            .and_then(|status| status.generation)
            .map(|generation| generation.0.get())
        else {
            return;
        };
        let Some(at) = self.clock.wall_nanos() else {
            return;
        };
        let status = self
            .calendar
            .status(key.instrument.as_str(), at.div_euclid(1_000_000));
        let source = match status.source {
            crate::MarketSessionSource::ProviderCalendar => CalendarSourceKind::ProviderCalendar,
            crate::MarketSessionSource::ProviderHours => CalendarSourceKind::ContractHours,
            crate::MarketSessionSource::Continuous => CalendarSourceKind::AlwaysOpen,
            crate::MarketSessionSource::Unavailable => CalendarSourceKind::Unknown,
        };
        let session = InstrumentSession {
            instrument_id: key.instrument.clone(),
            provider_generation,
            phase: self
                .calendar
                .phase(key.instrument.as_str(), at.div_euclid(1_000_000)),
            source,
            session_end_unix_nanos: status.session_end_unix_nanos,
            next_open_unix_nanos: status.next_open_unix_nanos,
        };
        self.trading_feed.publish(MarketFeedEvent::Session(session));
    }

    pub(super) fn publish_trading_feed_snapshot(&self) {
        for instrument in self.catalog.values() {
            if let Ok(terms) = super::super::instrument_terms_from_install(instrument) {
                self.trading_feed
                    .publish(aeris_trading::market::MarketFeedEvent::Terms(terms));
            }
        }
        for (key, _) in self.engine.instrument_subscriptions() {
            self.publish_trading_session(&key);
            if let Some(generation) = self
                .engine
                .provider_status(&key.provider)
                .and_then(|status| status.generation)
            {
                let state = if self
                    .engine
                    .provider_status(&key.provider)
                    .is_some_and(|status| status.health == ProviderHealth::Online)
                {
                    aeris_trading::market::FeedState::Live
                } else {
                    aeris_trading::market::FeedState::Stale {
                        reason: aeris_trading::market::FeedReason::Disconnected,
                    }
                };
                self.trading_feed
                    .publish(aeris_trading::market::MarketFeedEvent::Feed {
                        instrument_id: key.instrument.clone(),
                        state,
                        provider_generation: generation.0.get(),
                    });
            }
            // Publish dependencies first even if the feed worker drains
            // between individual calls; key ordering covers queued snapshots.
            if let Some(quote) = self
                .quotes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .latest(&key)
                .filter(|state| {
                    self.engine
                        .provider_status(&key.provider)
                        .and_then(|status| status.generation)
                        .is_some_and(|generation| {
                            generation.0.get() == state.quote.provider_generation
                        })
                })
            {
                self.trading_feed
                    .publish(aeris_trading::market::MarketFeedEvent::Quote(quote.quote));
            }
        }
    }

    pub(in super::super) fn publish_provider_feed(
        &self,
        provider: &str,
        generation: u64,
        state: aeris_trading::market::FeedState,
    ) {
        if !self.trading_feed.attached() {
            return;
        }
        for (key, _) in self.engine.instrument_subscriptions() {
            if key.provider == provider {
                self.trading_feed
                    .publish(aeris_trading::market::MarketFeedEvent::Feed {
                        instrument_id: key.instrument.clone(),
                        state,
                        provider_generation: generation,
                    });
            }
        }
    }

    /// Republishes the per-generation trading session contract for one provider's leased
    /// instruments. A provider session replacement clears the trading owner's
    /// per-generation state, so `Connected` must restore sessions before the `Feed::Live`
    /// publication resumes quotes; otherwise every later order reads a missing session.
    pub(in super::super) fn republish_trading_sessions(&self, provider: &str) {
        if !self.trading_feed.attached() {
            return;
        }
        let keys = self
            .engine
            .instrument_subscriptions()
            .map(|(key, _)| key)
            .filter(|key| key.provider == provider)
            .collect::<Vec<_>>();
        for key in &keys {
            self.publish_trading_session(key);
        }
    }
}
