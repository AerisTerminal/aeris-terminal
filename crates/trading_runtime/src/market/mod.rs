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
        let require_new_quote =
            self.require_practice_market_session(&instrument, self.command_now)?;
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
        if require_new_quote {
            candidates.retain(|(order_id, _)| {
                self.restored_orders.contains(order_id)
                    || self.state.orders.get(order_id).is_some_and(|order| {
                        observation_revision(observation)
                            > (
                                order.provenance.session_generation,
                                order.provenance.source_sequence,
                            )
                    })
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
            self.store.enforce_retention()?;
            self.enforce_memory_retention();
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
                self.store.update_position(&updated)?;
                self.state.positions.insert(key, updated);
                positions_changed = true;
            }
        }
        Ok(positions_changed)
    }
}
