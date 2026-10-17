use super::{
    ClientOrderId, EconomicEventRiskAction, EconomicEventRiskOutcome, EconomicEventRiskTrigger,
    Fill, FillId, FillMemoryUpdate, FixedPoint, FlattenOutcome, InstrumentId, Liquidity, Order,
    OrderId, OrderSide, OrderStatus, OrderType, PlaceOrder, SimulatedMarketObservation,
    TimeInForce, TradingAccountId, TradingCore, VenueRoute, fill_policy_persistence,
    flatten_acceptance_event, flatten_fill_event,
};

impl TradingCore {
    pub(super) fn kill_switch(
        &mut self,
        account_id: Option<&TradingAccountId>,
        reason: &str,
        locked_at_unix_nanos: i64,
    ) -> Result<usize, String> {
        let accounts = self
            .state
            .accounts
            .keys()
            .filter(|candidate| {
                account_id
                    .as_ref()
                    .is_none_or(|account_id| *candidate == *account_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return Err("kill switch account is not registered".to_string());
        }
        // A targeted command on a data-only live account is refused before any change; a
        // global one skips such accounts, which cannot trade, and protects the rest.
        if let Some(account_id) = account_id {
            self.route(account_id)?;
        }
        let tradable = accounts
            .into_iter()
            .filter(|account| self.route(account).is_ok())
            .collect::<Vec<_>>();
        // Locking is local and always possible, so it happens first: no new order can be
        // placed even when a broker venue is disconnected and its orders cannot be cancelled.
        for account in &tradable {
            self.lock_account(account.clone(), reason.to_string(), locked_at_unix_nanos)?;
        }
        let (cancelled, failures) = self.cancel_open_orders(account_id)?;
        if let Some(failure) = failures {
            return Err(format!("accounts are locked; {failure}"));
        }
        Ok(cancelled.len())
    }

    pub(super) fn flatten_account(
        &mut self,
        account_id: &TradingAccountId,
        observation: &SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        if !self.state.accounts.contains_key(account_id) {
            return Err("flatten account is not registered".to_string());
        }
        match self.route(account_id)? {
            VenueRoute::Simulated => {}
            VenueRoute::CtraderDemo => {
                return self.flatten_broker_account(account_id, &observation.instrument_id);
            }
        }
        let mut fills = self.observe_market(observation)?;
        self.cancel_all(Some(account_id))?;
        fills.extend(self.flatten_positions(account_id, observation)?);
        Ok(FlattenOutcome {
            fills,
            pending_close_requests: Vec::new(),
            incomplete: None,
        })
    }

    pub(super) fn reverse_position(
        &mut self,
        account_id: &TradingAccountId,
        observation: &SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        match self.route(account_id)? {
            VenueRoute::Simulated => {}
            VenueRoute::CtraderDemo => {
                return self.reverse_broker_position(
                    account_id,
                    &observation.instrument_id,
                    &observation.provenance,
                );
            }
        }
        let key = (account_id.clone(), observation.instrument_id.clone());
        let position = self
            .state
            .positions
            .get(&key)
            .filter(|position| position.net_quantity.units() != 0)
            .cloned()
            .ok_or_else(|| "reverse requires an open position".to_string())?;
        let side = if position.net_quantity.units() > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let quantity = FixedPoint::try_new(
            i64::try_from(position.net_quantity.units().unsigned_abs())
                .map_err(|_| "reverse quantity overflowed".to_string())?,
            position.net_quantity.scale(),
        )
        .map_err(|error| error.to_string())?;
        let mut outcome = self.flatten_account(account_id, observation)?;
        let client_order_id =
            ClientOrderId::try_new(format!("sim-reverse-{}", self.state.next_sequence))
                .map_err(|error| error.to_string())?;
        let order = PlaceOrder {
            client_order_id,
            account_id: account_id.clone(),
            instrument_id: observation.instrument_id.clone(),
            side,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::Day,
            quantity,
            limit_price: None,
            stop_price: None,
            submitted_unix_nanos: observation.provenance.observed_unix_nanos,
            provenance: observation.provenance.clone(),
        };
        self.place_order(&order)?;
        outcome.fills.extend(self.observe_market(observation)?);
        Ok(outcome)
    }

    pub(super) fn apply_economic_event_risk(
        &mut self,
        event: &EconomicEventRiskTrigger,
        observation: Option<&SimulatedMarketObservation>,
    ) -> Result<EconomicEventRiskOutcome, String> {
        const EVENT_GRACE_NANOS: i64 = 15 * 60 * 1_000_000_000;
        event.validate()?;
        let profiles = self
            .state
            .risk_profiles
            .values()
            .filter_map(|profile| {
                profile
                    .enabled
                    .then_some(profile.economic_event_rule)
                    .flatten()
                    .map(|rule| (profile.clone(), rule))
            })
            .collect::<Vec<_>>();
        for (profile, rule) in &profiles {
            if rule.action == EconomicEventRiskAction::Flatten
                && self.route(&profile.account_id)? != VenueRoute::Simulated
            {
                return Err("economic-event flatten is unavailable for broker accounts".into());
            }
        }
        let mut outcome = EconomicEventRiskOutcome::default();
        for (profile, rule) in profiles {
            if event.importance < rule.minimum_importance {
                continue;
            }
            let lead_nanos = i64::from(rule.lead_seconds)
                .checked_mul(1_000_000_000)
                .ok_or_else(|| "economic event lead time overflowed".to_string())?;
            let window_start = event
                .scheduled_unix_nanos
                .checked_sub(lead_nanos)
                .ok_or_else(|| "economic event lead window overflowed".to_string())?;
            let window_end = event
                .scheduled_unix_nanos
                .checked_add(EVENT_GRACE_NANOS)
                .ok_or_else(|| "economic event grace window overflowed".to_string())?;
            if !(window_start..=window_end).contains(&event.observed_unix_nanos)
                || self.store.economic_event_action_exists(
                    &profile.account_id,
                    &event.event_id,
                    profile.version,
                )?
            {
                continue;
            }
            match rule.action {
                EconomicEventRiskAction::Lock => {
                    let reason = format!("Economic event lock: {} ({})", event.title, event.source);
                    self.kill_switch(
                        Some(&profile.account_id),
                        &reason,
                        event.observed_unix_nanos,
                    )?;
                    outcome.locked_accounts += 1;
                }
                EconomicEventRiskAction::Flatten => {
                    let observation = observation.ok_or_else(|| {
                        "economic event flatten requires a current market observation".to_string()
                    })?;
                    if self
                        .state
                        .positions
                        .iter()
                        .any(|((account_id, instrument_id), position)| {
                            account_id == &profile.account_id
                                && position.net_quantity.units() != 0
                                && instrument_id != &observation.instrument_id
                        })
                    {
                        return Err(
                            "economic event flatten requires current observations for every open instrument"
                                .to_string(),
                        );
                    }
                    outcome.fills.extend(
                        self.flatten_account(&profile.account_id, observation)?
                            .fills,
                    );
                    outcome.flattened_accounts += 1;
                }
            }
            self.store.put_economic_event_action(
                &profile.account_id,
                &event.event_id,
                profile.version,
                rule.action,
                event.observed_unix_nanos,
            )?;
        }
        Ok(outcome)
    }

    pub(super) fn flatten_all(
        &mut self,
        observation: &SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        if self.state.accounts.is_empty() {
            return Err("flatten requires at least one registered account".to_string());
        }
        let mut fills = self.observe_market(observation)?;
        // A broker account never blocks flattening the others: what cannot be requested
        // for it is reported, and everything else still runs.
        let (_, mut incomplete) = self.cancel_open_orders(None)?;
        let mut pending_close_requests = Vec::new();
        let accounts = self.state.accounts.keys().cloned().collect::<Vec<_>>();
        for account_id in accounts {
            // Live accounts are data-only and hold nothing Aeris can close.
            let Ok(route) = self.route(&account_id) else {
                continue;
            };
            match route {
                VenueRoute::Simulated => {
                    fills.extend(self.flatten_positions(&account_id, observation)?);
                }
                VenueRoute::CtraderDemo => {
                    match self.close_broker_positions_on(&account_id, &observation.instrument_id) {
                        Ok(closes) => pending_close_requests.extend(closes),
                        Err(error) => {
                            incomplete.get_or_insert(format!(
                                "cTrader positions were not closed: {error}"
                            ));
                        }
                    }
                }
            }
        }
        Ok(FlattenOutcome {
            fills,
            pending_close_requests,
            incomplete,
        })
    }

    pub(super) fn flatten_positions(
        &mut self,
        account_id: &TradingAccountId,
        observation: &SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        let position_keys = self
            .state
            .positions
            .keys()
            .filter(|(candidate, instrument_id)| {
                candidate == account_id && instrument_id == &observation.instrument_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut fills = Vec::new();
        for key in position_keys {
            if let Some(fill) = self.flatten_position(account_id, &key, observation)? {
                fills.push(fill);
            }
        }
        Ok(fills)
    }

    pub(super) fn flatten_position(
        &mut self,
        account_id: &TradingAccountId,
        key: &(TradingAccountId, InstrumentId),
        observation: &SimulatedMarketObservation,
    ) -> Result<Option<Fill>, String> {
        let Some(current) = self.state.positions.get(key).cloned() else {
            return Ok(None);
        };
        if current.net_quantity.units() == 0 {
            return Ok(None);
        }
        let instrument = self
            .state
            .instruments
            .get(&observation.instrument_id)
            .ok_or_else(|| "flatten instrument is not registered".to_string())?
            .clone();
        let account = self
            .state
            .accounts
            .get(account_id)
            .ok_or_else(|| "flatten account is not registered".to_string())?
            .clone();
        let sequence = self.state.next_sequence;
        let side = if current.net_quantity.units() > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let quantity = FixedPoint::try_new(
            i64::try_from(current.net_quantity.units().unsigned_abs())
                .map_err(|_| "flatten quantity overflowed".to_string())?,
            current.net_quantity.scale(),
        )
        .map_err(|error| error.to_string())?;
        let price = if side == OrderSide::Sell {
            observation.bid
        } else {
            observation.ask
        };
        let provenance = observation.provenance.clone();
        let order = flatten_order(
            account_id,
            observation,
            sequence,
            side,
            quantity,
            self.command_now,
        )?;
        let accepted = flatten_acceptance_event(&order, sequence)?;
        self.store
            .insert_order(&order, &accepted, sequence + 1, false)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(accepted);
        self.state.orders.insert(order.id.clone(), order.clone());
        let fill = Fill {
            id: FillId::try_new(format!("sim-flatten-fill-{}", sequence + 1))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            account_id: account_id.clone(),
            instrument_id: observation.instrument_id.clone(),
            side,
            price,
            quantity,
            execution_unix_nanos: provenance.observed_unix_nanos,
            provenance: provenance.clone(),
        };
        fill.validate().map_err(|error| error.to_string())?;
        let event = flatten_fill_event(&order, &fill, sequence + 1, provenance)?;
        let (position, cost_basis, policy_update) = self.apply_fill(
            &fill,
            Some(&current),
            &account,
            &instrument,
            Liquidity::Taker,
        )?;
        let filled = crate::book::lifecycle::fill_simulated(order, self.command_now)
            .map_err(|error| error.to_string())?;
        self.store.insert_fill(
            &filled,
            &event,
            &fill,
            &position,
            fill_policy_persistence(&policy_update, cost_basis),
            sequence + 2,
        )?;
        self.apply_fill_to_memory(
            filled,
            event,
            &fill,
            FillMemoryUpdate {
                position,
                cost_basis,
                position_key: key.clone(),
                policy_update,
                next_sequence: sequence + 2,
            },
        );
        Ok(Some(fill))
    }
}

fn flatten_order(
    account_id: &TradingAccountId,
    observation: &SimulatedMarketObservation,
    sequence: u64,
    side: OrderSide,
    quantity: FixedPoint,
    command_now: i64,
) -> Result<Order, String> {
    let order = Order {
        id: OrderId::try_new(format!("sim-flatten-order-{sequence}"))
            .map_err(|error| error.to_string())?,
        client_order_id: ClientOrderId::try_new(format!("sim-flatten-{sequence}"))
            .map_err(|error| error.to_string())?,
        account_id: account_id.clone(),
        instrument_id: observation.instrument_id.clone(),
        side,
        order_type: OrderType::Market,
        time_in_force: TimeInForce::ImmediateOrCancel,
        quantity,
        filled_quantity: FixedPoint::try_new(0, quantity.scale())
            .map_err(|error| error.to_string())?,
        limit_price: None,
        stop_price: None,
        status: OrderStatus::Working,
        submitted_unix_nanos: observation.provenance.observed_unix_nanos,
        revision: 0,
        accepted_unix_nanos: 0,
        eligible_after_unix_nanos: 0,
        triggered_unix_nanos: None,
        expires_at_unix_nanos: None,
        updated_unix_nanos: 0,
        terminal_unix_nanos: None,
        provenance: observation.provenance.clone(),
    };
    let accepted =
        crate::book::lifecycle::accept(order, command_now).map_err(|error| error.to_string())?;
    accepted.validate().map_err(|error| error.to_string())?;
    Ok(accepted)
}
