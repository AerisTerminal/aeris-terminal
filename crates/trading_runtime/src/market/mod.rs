pub(crate) mod feed;
pub(crate) mod gating;
#[cfg(test)]
mod placement_atomicity_tests;

use super::{Command, respond};
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
        // Legacy route-less DAY rows were fenced at startup. Verified Terms
        // establish the missing calendar provenance before any later quote can
        // execute them. Deadline backfill and expiry remain durable owner steps.
        self.recover_day_deadlines()
            .map_err(MarketConsumerError::Persistence)?;
        self.expire_due_day_orders()
            .map_err(MarketConsumerError::Persistence)?;
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
        staged: &[InstrumentId],
    ) -> Result<(), aeris_trading::RejectReason> {
        let mut ids = self.quote_interest_ids();
        ids.extend(self.ticket_interest.keys().cloned());
        ids.extend(staged.iter().cloned());
        ids.insert(id.clone());
        let routed = ids
            .iter()
            .filter(|id| self.state.market_routes.contains_key(*id))
            .count();
        if routed > MAXIMUM_QUOTE_INTEREST {
            return Err(aeris_trading::RejectReason::CapacityExceeded);
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
        for group in self.state.order_groups.values() {
            if group.kind != crate::core::GroupKind::Bracket
                || !matches!(
                    group.status,
                    crate::core::GroupStatus::Pending | crate::core::GroupStatus::Active
                )
            {
                continue;
            }
            if self
                .state
                .accounts
                .get(&group.account_id)
                .is_some_and(|account| account.environment == AccountEnvironment::Simulated)
            {
                ids.insert(group.instrument_id.clone());
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
        self.apply_market_batch(batch);
    }

    /// Applies one drained feed batch; the replay harness drives this directly because it owns
    /// a `TradingCore` without the service inbox.
    pub(crate) fn apply_market_batch(&mut self, batch: crate::core::market::feed::MarketFeedBatch) {
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
        self.pending_equity_samples.clear();
        if self.mark_only_quote(observation)? {
            let instrument = self
                .state
                .instruments
                .get(&observation.instrument_id)
                .ok_or("trading instrument is not registered")?
                .clone();
            self.update_mark_to_market(observation, &instrument)?;
            return Ok(Vec::new());
        }
        let working_state = self.state.clone();
        let original = std::mem::replace(&mut self.state, working_state);
        self.quote_planning = true;
        let result = self.plan_market_quote(observation);
        self.quote_planning = false;
        let planned = std::mem::replace(&mut self.state, original);
        if result.is_err() {
            self.pending_equity_samples.clear();
        }
        let fills = result?;
        if fills.is_empty()
            && planned.revision == self.state.revision
            && planned.orders.iter().eq(self.state.orders.iter())
            && planned.order_events == self.state.order_events
            && planned.order_groups == self.state.order_groups
            && planned.risk_rule_states == self.state.risk_rule_states
            && planned.discipline_states == self.state.discipline_states
            && planned.risk_locks == self.state.risk_locks
        {
            self.state = planned;
            return Ok(fills);
        }
        if let Err(error) =
            self.store
                .commit_quote_step(&self.state, &planned, &self.pending_equity_samples)
        {
            self.pending_equity_samples.clear();
            self.quote_commit_failed = true;
            return Err(error);
        }
        self.pending_equity_samples.clear();
        self.state = planned;
        self.enforce_memory_retention();
        Ok(fills)
    }

    /// Bypass speculative state cloning only when no quote-driven durable effect is possible.
    /// Brackets and live risk always use the full planner, even on a non-crossing quote.
    fn mark_only_quote(&self, observation: &SimulatedMarketObservation) -> Result<bool, String> {
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
            .ok_or("trading instrument is not registered")?;
        if observation.bid.scale() != instrument.price_scale {
            return Err("market observation scale does not match the instrument".to_string());
        }
        self.validate_mark_to_market(observation, instrument)?;
        // Group-owned brackets (pending or active) keep the full planner on this instrument.
        Ok(!self.state.order_groups.values().any(|group| {
            group.instrument_id == observation.instrument_id
                && matches!(
                    group.status,
                    super::GroupStatus::Pending | super::GroupStatus::Active
                )
        }) && !self
            .state
            .risk_profiles
            .values()
            .any(|profile| profile.enabled)
            && !self
                .state
                .orders
                .crossing_for(
                    &observation.instrument_id,
                    observation.bid.units(),
                    observation.ask.units(),
                )
                .into_iter()
                .any(|order| {
                    super::is_simulated(&self.state.accounts, &order.account_id)
                        && (super::touch_price(order, observation).is_some()
                            || super::stop_limit_crossed(order, observation))
                })
            && !self
                .state
                .orders
                .executable_for(&observation.instrument_id)
                .any(|order| {
                    super::is_simulated(&self.state.accounts, &order.account_id)
                        && matches!(
                            order.time_in_force,
                            super::TimeInForce::ImmediateOrCancel | super::TimeInForce::FillOrKill
                        )
                }))
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
        self.update_owned_stops(observation, &instrument)?;
        let mut candidates =
            market_fill_candidates(&self.state.orders, &self.state.accounts, observation);
        let mut stop_limits = self
            .state
            .orders
            .crossing_for(
                &observation.instrument_id,
                observation.bid.units(),
                observation.ask.units(),
            )
            .into_iter()
            .filter(|order| {
                super::is_simulated(&self.state.accounts, &order.account_id)
                    && super::stop_limit_crossed(order, observation)
            })
            .cloned()
            .collect::<Vec<_>>();
        if self.check_practice_market(&instrument).is_err() {
            candidates.clear();
            stop_limits.clear();
        } else {
            candidates.retain(|(order_id, _)| {
                self.state
                    .orders
                    .get(order_id)
                    .is_some_and(|order| self.quote_order_eligible(order, observation))
            });
            stop_limits.retain(|order| self.quote_order_eligible(order, observation));
        }
        let triggered = !stop_limits.is_empty();
        for order in stop_limits {
            self.trigger_simulated_stop(order, observation)?;
        }
        let candidate_ids = candidates
            .iter()
            .map(|(order_id, _)| order_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        if triggered || !candidates.is_empty() {
            self.bump_revision()?;
        }
        let mut fills = Vec::with_capacity(candidates.len());
        let candidates = candidates
            .into_iter()
            .filter_map(|(id, price)| {
                self.state
                    .orders
                    .get(&id)
                    .map(|order| (id, price, order.revision))
            })
            .collect::<Vec<_>>();
        for (order_id, original_price, revision) in candidates {
            // A preceding fill may complete a group and cancel this order. Re-read both
            // membership and price after its reactions, never execute a stale candidate.
            let Some(order) = self.state.orders.get(&order_id) else {
                continue;
            };
            if !order.status.is_executable() || !self.quote_order_eligible(order, observation) {
                continue;
            }
            let price = if order.revision == revision {
                Some(original_price)
            } else {
                super::touch_price(order, observation)
            };
            let Some(price) = price else {
                continue;
            };
            let executed = self.execute_market_fills(
                vec![(order_id, price)],
                observation,
                &instrument,
                false,
            )?;
            self.reconcile_order_groups(&executed, observation, &instrument)?;
            self.align_position_protection(&executed, observation)?;
            fills.extend(executed);
        }
        self.update_mark_to_market(observation, &instrument)?;
        self.evaluate_live_risk(observation.provenance.observed_unix_nanos)?;
        self.cancel_unfilled_immediate_orders(observation, &candidate_ids)?;
        Ok(fills)
    }

    fn quote_order_eligible(
        &self,
        order: &Order,
        observation: &SimulatedMarketObservation,
    ) -> bool {
        // A legacy DAY row without its acceptance-session deadline cannot be
        // executed until recovery has assigned a verified calendar boundary.
        if order.time_in_force == aeris_trading::TimeInForce::Day
            && order.expires_at_unix_nanos.is_none()
        {
            return false;
        }
        #[cfg(test)]
        if !self
            .state
            .market_routes
            .contains_key(&observation.instrument_id)
        {
            return true;
        }
        observation.provenance.observed_unix_nanos
            > order
                .eligible_after_unix_nanos
                .max(self.recovery_quote_floor)
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
            let basis = self
                .state
                .cost_basis
                .get(&(current.account_id.clone(), current.instrument_id.clone()))
                .copied();
            let _ = unrealized_pnl(basis, current, mark, point_value).map_err(|error| {
                format!(
                    "unrealized PnL failed for {}/{}: {error}",
                    current.account_id.as_str(),
                    current.instrument_id.as_str()
                )
            })?;
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
            let unrealized = unrealized_pnl(
                self.state.cost_basis.get(&key).copied(),
                &current,
                mark,
                point_value,
            )?;
            if current.unrealized_pnl != unrealized {
                let updated = Position {
                    unrealized_pnl: unrealized,
                    ..current
                };
                self.state.positions.insert(key, updated);
                positions_changed = true;
            }
        }
        Ok(positions_changed)
    }
}
/// Unrealized P&L from the exact cost basis when one is stored: signed mark value minus
/// basis, times point value, rounded once into the account currency. A position without a
/// basis row (a defensive legacy projection) keeps the average-price computation.
fn unrealized_pnl(
    basis: Option<crate::core::ledger::CostBasis>,
    current: &Position,
    mark: FixedPoint,
    point_value: FixedPoint,
) -> Result<FixedPoint, String> {
    let currency_scale = current.realized_pnl.scale();
    let Some(basis) = basis else {
        return project_unrealized_pnl(
            current
                .average_entry_price
                .ok_or_else(|| "position average is unavailable for unrealized PnL".to_string())?,
            mark,
            current.net_quantity,
            point_value,
            currency_scale,
        )
        .map_err(|error| error.to_string());
    };
    let quantity_scale = current.net_quantity.scale();
    let combined_scale = mark
        .scale()
        .checked_add(quantity_scale)
        .ok_or_else(|| "unrealized PnL scale overflowed".to_string())?;
    let basis = basis.rescaled(combined_scale)?;
    let mark_value = i128::from(mark.units())
        .checked_mul(i128::from(current.net_quantity.units().unsigned_abs()))
        .ok_or_else(|| "unrealized PnL overflowed".to_string())?;
    let gross = mark_value
        .checked_sub(basis.units)
        .and_then(|value| value.checked_mul(i128::from(current.net_quantity.units().signum())))
        .and_then(|value| value.checked_mul(i128::from(point_value.units())))
        .ok_or_else(|| "unrealized PnL overflowed".to_string())?;
    let raw_scale = combined_scale
        .checked_add(point_value.scale())
        .ok_or_else(|| "unrealized PnL scale overflowed".to_string())?;
    aeris_trading::fixed_point_from_i128_rounded(gross, raw_scale, currency_scale)
        .map_err(|error| error.to_string())
}
