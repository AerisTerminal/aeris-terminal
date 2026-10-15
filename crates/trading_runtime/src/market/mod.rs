pub(crate) mod feed;
pub(crate) mod gating;

use super::{Command, ManagedBracketStatus, respond};
use aeris_instruments::{ContractMetadata, InstrumentId, InstrumentMetadataProvenance, PriceGrid};
use aeris_trading::AccountEnvironment;
use aeris_trading::market::{
    InstrumentTerms, MAXIMUM_QUOTE_INTEREST, MarketFeedError, MarketFeedEvent, MarketRoute,
    QuoteInterest,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const MAXIMUM_TICKET_INTEREST: usize = 64;

#[derive(Debug)]
pub(crate) enum MarketConsumerError {
    Feed(MarketFeedError),
    InvalidContract(String),
    ProviderChanged,
    TicketLimit,
    DuplicateTicket,
    InterestLimit,
    RevisionExhausted,
    Persistence(String),
}

impl fmt::Display for MarketConsumerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Feed(error) => write!(f, "{error}"),
            Self::InvalidContract(error) | Self::Persistence(error) => f.write_str(error),
            Self::ProviderChanged => {
                f.write_str("verified terms cannot change an instrument's provider identity")
            }
            Self::TicketLimit => f.write_str("ticket quote interest limit reached"),
            Self::DuplicateTicket => f.write_str("duplicate ticket route"),
            Self::InterestLimit => f.write_str("trading quote interest limit reached"),
            Self::RevisionExhausted => f.write_str("quote interest revision exhausted"),
        }
    }
}

impl std::error::Error for MarketConsumerError {}

impl From<MarketFeedError> for MarketConsumerError {
    fn from(error: MarketFeedError) -> Self {
        Self::Feed(error)
    }
}

impl TradingCore {
    pub(super) fn check_practice_session(
        &self,
        instrument: &TradingInstrument,
    ) -> Result<(), aeris_trading::RejectReason> {
        #[cfg(test)]
        if !self
            .state
            .market_routes
            .contains_key(&instrument.instrument_id)
        {
            // Direct-observation fixtures have no provider feed; real Terms always carry a route.
            return Ok(());
        }
        self.quote_book
            .check_session(&instrument.instrument_id, self.command_now)
    }

    pub(super) fn check_practice_market(
        &self,
        instrument: &TradingInstrument,
    ) -> Result<(), aeris_trading::RejectReason> {
        self.check_practice_session(instrument)?;
        #[cfg(test)]
        if !self
            .state
            .market_routes
            .contains_key(&instrument.instrument_id)
        {
            return Ok(());
        }
        self.quote_book.check_quote(
            &instrument.instrument_id,
            self.command_now,
            instrument.contract.instrument_class,
            self.execution,
        )
    }

    pub(crate) fn dispatch_market_command(&mut self, command: Command) {
        match command {
            Command::AttachMarketFeed(sink, reply) => {
                self.interest_sink = Some(sink);
                self.published_interest.clear();
                respond(
                    &reply,
                    self.recompute_quote_interest()
                        .map_err(|error| error.to_string()),
                );
            }
            Command::SetTicketInterest(routes, reply) => {
                respond(
                    &reply,
                    self.replace_ticket_interest(routes)
                        .map_err(|error| error.to_string()),
                );
            }
            Command::DrainMarketFeed => self.drain_market_feed(),
            #[cfg(test)]
            Command::Observe(observation, reply) => {
                let previous_cursor = self
                    .observation_cursors
                    .get(&observation.instrument_id)
                    .copied();
                self.advance_observation_cursor(&observation);
                let result = self.observe_market(&observation);
                if self.quote_commit_failed {
                    if let Some(previous) = previous_cursor {
                        self.observation_cursors
                            .insert(observation.instrument_id.clone(), previous);
                    } else {
                        self.observation_cursors.remove(&observation.instrument_id);
                    }
                }
                if !self.quote_commit_failed {
                    self.market_observation_error = result.as_ref().err().cloned();
                }
                let _ = reply.send(result);
            }
            _ => unreachable!("only market commands are dispatched here"),
        }
    }

    pub(crate) fn register_verified_terms(
        &mut self,
        terms: InstrumentTerms,
    ) -> Result<(), MarketConsumerError> {
        terms.validate()?;
        let id = terms.route.instrument_id.clone();
        if self
            .state
            .market_routes
            .get(&id)
            .is_some_and(|route| route.provider_id != terms.route.provider_id)
        {
            return Err(MarketConsumerError::ProviderChanged);
        }
        if self.state.instruments.get(&id).is_some_and(|old| {
            old.contract.provenance.provider_id == terms.route.provider_id
                && old.contract.provenance.session_generation > terms.session_generation
        }) {
            return Ok(());
        }
        let contract = ContractMetadata {
            tick_size: match terms.price_grid {
                PriceGrid::Fixed(tick) => Some(tick),
                _ => None,
            },
            price_grid: terms.price_grid,
            instrument_class: terms.instrument_class,
            point_value: terms.point_value,
            order_quantity_increment: terms.quantity_increment,
            currency: terms.currency,
            expiry: terms.expiry,
            first_notice: terms.first_notice,
            last_trade: terms.last_trade,
            session_hours: terms.session_hours,
            provenance: InstrumentMetadataProvenance {
                provider_id: terms.route.provider_id.clone(),
                provider_symbol: terms.route.provider_symbol.clone(),
                display_symbol: terms.route.display_symbol.clone(),
                session_generation: terms.session_generation,
            },
        };
        let instrument = TradingInstrument {
            instrument_id: id.clone(),
            price_scale: terms.route.price_scale,
            quantity_scale: terms.route.quantity_scale,
            contract,
        };
        instrument
            .contract
            .validate()
            .map_err(|error| MarketConsumerError::InvalidContract(error.to_string()))?;
        if self.interested_in(&id)
            && !self.state.market_routes.contains_key(&id)
            && self.collect_quote_interest(&self.ticket_interest)?.len() == MAXIMUM_QUOTE_INTEREST
        {
            return Err(MarketConsumerError::InterestLimit);
        }
        let needs_quote = self.interested_in(&id);
        let route_changed = self
            .state
            .market_routes
            .get(&id)
            .is_some_and(|route| route != &terms.route);
        self.store
            .put_verified_instrument(&instrument, &terms.route)
            .map_err(MarketConsumerError::Persistence)?;
        self.state.instruments.insert(id.clone(), instrument);
        self.state.market_routes.insert(id.clone(), terms.route);
        if route_changed {
            self.quote_book.retire_route(&id);
        }
        if needs_quote {
            self.quote_book
                .set_generation_floor(&id, terms.session_generation);
        }
        self.bump_revision()
            .map_err(MarketConsumerError::Persistence)
    }

    pub(crate) fn replace_ticket_interest(
        &mut self,
        routes: Vec<MarketRoute>,
    ) -> Result<(), MarketConsumerError> {
        if routes.len() > MAXIMUM_TICKET_INTEREST {
            return Err(MarketConsumerError::TicketLimit);
        }
        let mut tickets = BTreeMap::new();
        for route in routes {
            route.validate()?;
            if tickets.insert(route.instrument_id.clone(), route).is_some() {
                return Err(MarketConsumerError::DuplicateTicket);
            }
        }
        // Check the union before changing the published demand.
        self.collect_quote_interest(&tickets)?;
        self.ticket_interest = tickets;
        Ok(())
    }

    pub(crate) fn interested_in(&self, id: &InstrumentId) -> bool {
        self.quote_interest_ids().contains(id) || self.ticket_interest.contains_key(id)
    }

    pub(crate) fn check_new_order_interest(
        &self,
        id: &InstrumentId,
    ) -> Result<(), MarketConsumerError> {
        if !self.interested_in(id)
            && self.state.market_routes.contains_key(id)
            && self.collect_quote_interest(&self.ticket_interest)?.len() == MAXIMUM_QUOTE_INTEREST
        {
            return Err(MarketConsumerError::InterestLimit);
        }
        Ok(())
    }

    fn quote_interest_ids(&self) -> BTreeSet<InstrumentId> {
        let mut ids = BTreeSet::new();
        for order in self.state.orders.open_orders() {
            if self
                .state
                .accounts
                .get(&order.account_id)
                .is_some_and(|account| account.environment == AccountEnvironment::Simulated)
            {
                ids.insert(order.instrument_id.clone());
            }
        }
        for position in self.state.positions.values() {
            if position.net_quantity.units() != 0
                && self
                    .state
                    .accounts
                    .get(&position.account_id)
                    .is_some_and(|account| account.environment == AccountEnvironment::Simulated)
            {
                ids.insert(position.instrument_id.clone());
            }
        }
        for group in self.state.managed_brackets.values() {
            if !matches!(
                group.status,
                ManagedBracketStatus::AwaitingEntry | ManagedBracketStatus::Active
            ) {
                continue;
            }
            let Some(entry) = self.state.orders.by_client(&group.entry_client_order_id) else {
                continue;
            };
            if self
                .state
                .accounts
                .get(&entry.account_id)
                .is_some_and(|account| account.environment == AccountEnvironment::Simulated)
            {
                ids.insert(entry.instrument_id.clone());
            }
        }
        ids
    }

    fn collect_quote_interest(
        &self,
        tickets: &BTreeMap<InstrumentId, MarketRoute>,
    ) -> Result<Vec<MarketRoute>, MarketConsumerError> {
        let mut routes = self
            .quote_interest_ids()
            .into_iter()
            .filter_map(|id| {
                self.state
                    .market_routes
                    .get(&id)
                    .cloned()
                    .map(|route| (id, route))
            })
            .collect::<BTreeMap<_, _>>();
        routes.extend(
            tickets
                .iter()
                .map(|(id, route)| (id.clone(), route.clone())),
        );
        if routes.len() > MAXIMUM_QUOTE_INTEREST {
            return Err(MarketConsumerError::InterestLimit);
        }
        Ok(routes.into_values().collect())
    }

    pub(crate) fn recompute_quote_interest(&mut self) -> Result<(), MarketConsumerError> {
        let Some(sink) = self.interest_sink.as_ref() else {
            return Ok(());
        };
        let routes = self.collect_quote_interest(&self.ticket_interest)?;
        if routes == self.published_interest {
            return Ok(());
        }
        let revision = self
            .interest_revision
            .checked_add(1)
            .ok_or(MarketConsumerError::RevisionExhausted)?;
        sink.set_interest(QuoteInterest {
            revision,
            routes: routes.clone(),
        })
        .map_err(MarketConsumerError::Feed)?;
        self.interest_revision = revision;
        self.published_interest = routes;
        self.quote_book.retain(
            &self
                .published_interest
                .iter()
                .map(|route| route.instrument_id.clone())
                .collect(),
        );
        Ok(())
    }

    pub(crate) fn drain_market_feed(&mut self) {
        let batch = self.market_feed.drain();
        let mut changed = BTreeSet::new();
        for event in batch.controls {
            match event {
                MarketFeedEvent::Terms(terms) => {
                    if let Err(error) = self.register_verified_terms(terms) {
                        self.market_observation_error = Some(error.to_string());
                    }
                }
                event => {
                    let id = match &event {
                        MarketFeedEvent::Feed { instrument_id, .. } => instrument_id.clone(),
                        MarketFeedEvent::Session(session) => session.instrument_id.clone(),
                        MarketFeedEvent::Quote(quote) => quote.instrument_id.clone(),
                        MarketFeedEvent::Terms(_) => unreachable!(),
                    };
                    match self.quote_book.accept(event, &self.state.market_routes) {
                        Ok(true) => {
                            changed.insert(id);
                        }
                        Err(error) => self.market_observation_error = Some(error.to_string()),
                        Ok(false) => {}
                    }
                }
            }
        }
        for quote in batch.quotes {
            let id = quote.instrument_id.clone();
            match self
                .quote_book
                .accept(MarketFeedEvent::Quote(quote), &self.state.market_routes)
            {
                Ok(true) => {
                    changed.insert(id);
                }
                Err(error) => self.market_observation_error = Some(error.to_string()),
                Ok(false) => {}
            }
        }
        let observations = changed
            .iter()
            .filter_map(|id| self.quote_book.observation(id))
            .collect();
        self.on_market(observations, self.command_now);
        if batch.resync {
            // A changed revision requests a full snapshot, including lost Terms/Feed/Session.
            self.published_interest.clear();
        }
    }
}
use super::{
    ClientOrderId, Fill, FixedPoint, ObservationCursor, Order, Position,
    SimulatedMarketObservation, TradingCore, TradingInstrument, market_fill_candidates,
    observation_revision, project_unrealized_pnl,
};

impl TradingCore {
    /// Records a published observation and reports whether it is newer than the
    /// last one applied. Unregistered instruments cannot hold simulated orders.
    pub(super) fn advance_observation_cursor(
        &mut self,
        observation: &SimulatedMarketObservation,
    ) -> bool {
        if !self
            .state
            .instruments
            .contains_key(&observation.instrument_id)
        {
            return false;
        }
        let (session_generation, source_sequence) = observation_revision(observation);
        let cursor = ObservationCursor {
            session_generation,
            source_sequence,
            bid_units: observation.bid.units(),
            ask_units: observation.ask.units(),
        };
        let is_newer = self
            .observation_cursors
            .get(&observation.instrument_id)
            .is_none_or(|current| {
                let current_revision = (current.session_generation, current.source_sequence);
                (session_generation, source_sequence) > current_revision
                    || ((session_generation, source_sequence) == current_revision
                        && (cursor.bid_units, cursor.ask_units)
                            != (current.bid_units, current.ask_units))
            });
        if is_newer {
            self.observation_cursors
                .insert(observation.instrument_id.clone(), cursor);
        }
        is_newer
    }

    pub(super) fn observe_market(
        &mut self,
        observation: &SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        self.quote_commit_failed = false;
        let working_state = self.state.clone();
        let working_eligibility = self.eligible_after.clone();
        let original = std::mem::replace(&mut self.state, working_state);
        let original_eligibility = std::mem::replace(&mut self.eligible_after, working_eligibility);
        self.quote_planning = true;
        let result = self.plan_market_quote(observation);
        self.quote_planning = false;
        let planned = std::mem::replace(&mut self.state, original);
        let planned_eligibility = std::mem::replace(&mut self.eligible_after, original_eligibility);
        let fills = result?;
        if let Err(error) = self.store.commit_quote_step(&self.state, &planned) {
            self.quote_commit_failed = true;
            return Err(error);
        }
        self.state = planned;
        self.eligible_after = planned_eligibility;
        self.enforce_memory_retention();
        Ok(fills)
    }

    fn plan_market_quote(
        &mut self,
        observation: &SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        observation
            .provenance
            .validate()
            .map_err(|error| error.to_string())?;
        if observation.bid.units() <= 0
            || observation.ask.units() <= observation.bid.units()
            || observation.bid.scale() != observation.ask.scale()
        {
            return Err("simulated market observation is invalid".to_string());
        }
        let instrument = self
            .state
            .instruments
            .get(&observation.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?
            .clone();
        if observation.bid.scale() != instrument.price_scale {
            return Err("market observation scale does not match the instrument".to_string());
        }
        // Validate every existing open-position projection before any order can fill. A broken
        // legacy/cache projection must never allow a durable fill to commit and only then turn the
        // command into an error; that produces invisible exposure and encourages repeated clicks.
        self.validate_mark_to_market(observation, &instrument)?;
        self.recover_managed_brackets(observation, &instrument)?;
        self.update_managed_stops(observation, &instrument)?;
        let mut candidates =
            market_fill_candidates(&self.state.orders, &self.state.accounts, observation);
        if self.check_practice_market(&instrument).is_err() {
            candidates.clear();
        } else {
            candidates.retain(|(order_id, _)| {
                #[cfg(test)]
                if !self
                    .state
                    .market_routes
                    .contains_key(&observation.instrument_id)
                {
                    return true;
                }
                let after = self
                    .eligible_after
                    .get(order_id)
                    .copied()
                    .or_else(|| {
                        self.state
                            .orders
                            .get(order_id)
                            .map(|order| order.submitted_unix_nanos)
                    })
                    .unwrap_or(i64::MAX);
                observation.provenance.observed_unix_nanos > after
            });
        }
        let candidate_ids = candidates
            .iter()
            .map(|(order_id, _)| order_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let fills = self.execute_market_fills(candidates, observation, &instrument)?;
        self.reconcile_managed_brackets(&fills, observation, &instrument)?;
        self.align_position_protection(&fills, observation)?;
        let mut positions_changed = self.update_mark_to_market(observation, &instrument)?;
        self.evaluate_live_risk(observation.provenance.observed_unix_nanos)?;
        positions_changed |= self.cancel_unfilled_immediate_orders(observation, &candidate_ids)?;
        if positions_changed {
            self.bump_revision()?;
        }
        Ok(fills)
    }

    pub(super) fn validate_mark_to_market(
        &self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let point_value = instrument
            .contract
            .point_value
            .ok_or_else(|| "point value is unavailable for unrealized PnL".to_string())?;
        let point_value = FixedPoint::try_new(point_value.units(), point_value.scale())
            .map_err(|error| error.to_string())?;
        for current in self.state.positions.values().filter(|position| {
            position.instrument_id == observation.instrument_id
                && position.net_quantity.units() != 0
        }) {
            let mark = if current.net_quantity.units() > 0 {
                observation.bid
            } else {
                observation.ask
            };
            project_unrealized_pnl(
                current.average_entry_price.ok_or_else(|| {
                    format!(
                        "position average is unavailable for unrealized PnL ({}/{})",
                        current.account_id.as_str(),
                        current.instrument_id.as_str()
                    )
                })?,
                mark,
                current.net_quantity,
                point_value,
                current.realized_pnl.scale(),
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(super) fn order_by_client_id(
        &self,
        client_order_id: &ClientOrderId,
    ) -> Result<&Order, String> {
        self.state
            .orders
            .by_client(client_order_id)
            .ok_or_else(|| "managed bracket order disappeared".to_string())
    }

    pub(super) fn update_mark_to_market(
        &mut self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<bool, String> {
        let mut positions_changed = false;
        let position_keys = self
            .state
            .positions
            .keys()
            .filter(|(_, instrument_id)| instrument_id == &observation.instrument_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in position_keys {
            let Some(current) = self.state.positions.get(&key).cloned() else {
                continue;
            };
            if current.net_quantity.units() == 0 {
                continue;
            }
            let mark = if current.net_quantity.units() > 0 {
                observation.bid
            } else {
                observation.ask
            };
            let point_value = instrument
                .contract
                .point_value
                .ok_or_else(|| "point value is unavailable for unrealized PnL".to_string())?;
            let point_value = FixedPoint::try_new(point_value.units(), point_value.scale())
                .map_err(|error| error.to_string())?;
            let unrealized = project_unrealized_pnl(
                current.average_entry_price.ok_or_else(|| {
                    "position average is unavailable for unrealized PnL".to_string()
                })?,
                mark,
                current.net_quantity,
                point_value,
                current.realized_pnl.scale(),
            )
            .map_err(|error| error.to_string())?;
            if current.unrealized_pnl != unrealized {
                let updated = Position {
                    unrealized_pnl: unrealized,
                    ..current
                };
                if !self.quote_planning {
                    self.store.update_position(&updated)?;
                }
                self.state.positions.insert(key, updated);
                positions_changed = true;
            }
        }
        Ok(positions_changed)
    }
}
