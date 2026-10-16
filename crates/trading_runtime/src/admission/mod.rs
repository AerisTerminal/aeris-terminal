use super::{
    AccountEnvironment, CreatePracticeAccount, DEMO_VENUE_UNAVAILABLE, FixedPoint,
    MAXIMUM_OPEN_ORDERS, MAXIMUM_SNAPSHOT_ITEMS, ManagedBracket, Order, OrderEvent, OrderEventId,
    OrderEventKind, OrderId, OrderStatus, OrderStep, OrderType, PlaceOrder, PlaceProtectiveOrder,
    ProtectiveOrder, ProtectiveOrderRole, TradeCopyDispatch, TradingAccount, TradingAccountId,
    TradingCore, TradingInstrument, VenueRequestReceiver, VenueRoute, check_quantity_increment,
    mirrored_client_order_id, multiply_quantity, order_acceptance_detail, venue,
};
use aeris_trading::RejectReason;

/// Client identity belongs to the order book, not the venue. Ignore only audit fields
/// stamped at submission; an instruction change must never silently reuse the old order.
fn existing_client_order(existing: &Order, command: &PlaceOrder) -> Result<Order, RejectReason> {
    if existing.account_id == command.account_id
        && existing.instrument_id == command.instrument_id
        && existing.side == command.side
        && existing.order_type == command.order_type
        && existing.time_in_force == command.time_in_force
        && existing.quantity == command.quantity
        && existing.limit_price == command.limit_price
        && existing.stop_price == command.stop_price
    {
        Ok(existing.clone())
    } else {
        Err(RejectReason::ClientIdReused)
    }
}

impl TradingCore {
    pub(super) fn register_account(&mut self, account: TradingAccount) -> Result<(), String> {
        account.validate().map_err(|error| error.to_string())?;
        self.store.put_account(&account)?;
        self.state.accounts.insert(account.id.clone(), account);
        self.bump_revision()
    }

    pub(super) fn route(&self, account_id: &TradingAccountId) -> Result<VenueRoute, String> {
        VenueRoute::from_account(
            self.state
                .accounts
                .get(account_id)
                .ok_or_else(|| "trading account is not registered".to_string())?,
        )
    }

    pub(super) fn attach_venue(&mut self) -> Result<(u64, VenueRequestReceiver), String> {
        let generation = self
            .state
            .venue_generation
            .checked_add(1)
            .ok_or("cTrader venue generation is exhausted")?;
        let (outbound, receiver) =
            aeris_trading::venue_outbox::bounded_venue_requests(venue::OUTBOUND_CAPACITY);
        // Every attach starts with a reconcile of each demo broker account, so state left
        // by a restart or a dropped session is settled against the broker first.
        let reconciles = self
            .state
            .accounts
            .values()
            .filter(|account| account.environment == AccountEnvironment::Demo)
            .filter_map(|account| self.reconcile_request(account))
            .collect::<Vec<_>>();
        for request in reconciles {
            outbound
                .try_send(request)
                .map_err(|_| "cTrader venue reconcile requests exceed the outbound queue")?;
        }
        self.store.set_venue_generation(generation)?;
        self.venue_outbound = Some(outbound);
        self.state.venue_generation = generation;
        self.venue_ingestion = 0;
        self.connected_broker_accounts.clear();
        self.bump_revision()?;
        Ok((generation, receiver))
    }

    pub(super) fn require_broker_route(&self, account_id: &TradingAccountId) -> Result<(), String> {
        match self.route(account_id)? {
            VenueRoute::Simulated => Err("broker position requires a cTrader account".into()),
            VenueRoute::CtraderDemo => Ok(()),
        }
    }

    pub(super) fn create_practice_account(
        &mut self,
        request: &CreatePracticeAccount,
    ) -> Result<TradingAccount, String> {
        let name = request.display_name.trim();
        if name.is_empty() {
            return Err("practice account name must not be empty".to_string());
        }
        let starting_equity = request
            .starting_equity
            .exact_rescale(2)
            .map_err(|error| error.to_string())?;
        if starting_equity.units() <= 0 {
            return Err("practice account equity must be positive".to_string());
        }
        if self.state.accounts.len() >= MAXIMUM_SNAPSHOT_ITEMS {
            return Err("practice account limit reached".to_string());
        }
        let sequence = self
            .state
            .revision
            .checked_add(1)
            .ok_or_else(|| "practice account identity overflowed".to_string())?;
        let account = TradingAccount {
            id: TradingAccountId::try_new(format!("aeris-practice-{sequence}"))
                .map_err(|error| error.to_string())?,
            display_name: format!("SIM • {name}"),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".to_string(),
            broker_ref: None,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: Some(starting_equity),
        };
        self.register_account(account.clone())?;
        Ok(account)
    }

    pub(super) fn delete_practice_account(
        &mut self,
        account_id: &TradingAccountId,
    ) -> Result<TradingAccount, String> {
        let account = self.deletable_practice_account(account_id)?;
        let (account_order_ids, account_client_order_ids, managed_bracket_ids) =
            self.practice_account_order_identity(account_id);
        self.store
            .delete_practice_account(account_id, &managed_bracket_ids)?;
        self.purge_practice_account_state(
            account_id,
            &account_order_ids,
            &account_client_order_ids,
            &managed_bracket_ids,
        );
        self.bump_revision()?;
        Ok(account)
    }

    pub(super) fn deletable_practice_account(
        &self,
        account_id: &TradingAccountId,
    ) -> Result<TradingAccount, String> {
        let account = self
            .state
            .accounts
            .get(account_id)
            .cloned()
            .ok_or_else(|| "practice account was not found".to_string())?;
        if account.environment != AccountEnvironment::Simulated {
            return Err("only simulated practice accounts can be deleted".to_string());
        }
        Ok(account)
    }

    pub(super) fn practice_account_order_identity(
        &self,
        account_id: &TradingAccountId,
    ) -> (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
        Vec<String>,
    ) {
        let account_order_ids = self
            .state
            .orders
            .values()
            .filter(|order| &order.account_id == account_id)
            .map(|order| order.id.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let account_client_order_ids = self
            .state
            .orders
            .values()
            .filter(|order| &order.account_id == account_id)
            .map(|order| order.client_order_id.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let managed_bracket_ids = self
            .state
            .managed_brackets
            .values()
            .filter(|bracket| {
                account_client_order_ids.contains(bracket.entry_client_order_id.as_str())
                    || bracket
                        .stop_client_order_id
                        .as_ref()
                        .is_some_and(|id| account_client_order_ids.contains(id.as_str()))
                    || bracket
                        .target_client_order_ids
                        .iter()
                        .any(|id| account_client_order_ids.contains(id.as_str()))
            })
            .map(|bracket| bracket.bracket_id.clone())
            .collect::<Vec<_>>();
        (
            account_order_ids,
            account_client_order_ids,
            managed_bracket_ids,
        )
    }

    pub(super) fn purge_practice_account_state(
        &mut self,
        account_id: &TradingAccountId,
        account_order_ids: &std::collections::BTreeSet<String>,
        account_client_order_ids: &std::collections::BTreeSet<String>,
        managed_bracket_ids: &[String],
    ) {
        self.state.accounts.remove(account_id);
        self.state.orders.remove_account(account_id);
        self.eligible_after
            .retain(|order_id, _| !account_order_ids.contains(order_id.as_str()));
        self.state
            .order_events
            .retain(|event| !account_order_ids.contains(event.order_id.as_str()));
        for fill in self
            .state
            .fills
            .iter()
            .filter(|fill| &fill.account_id == account_id)
        {
            self.state.fill_accounting.remove(&fill.id);
            self.state.broker_fill_positions.remove(&fill.id);
        }
        self.state
            .fills
            .retain(|fill| &fill.account_id != account_id);
        self.state
            .positions
            .retain(|(stored_account, _), _| stored_account != account_id);
        self.state.risk_profiles.remove(account_id);
        self.state.risk_locks.remove(account_id);
        self.state.risk_rule_states.remove(account_id);
        self.state
            .trade_cycles
            .retain(|(stored_account, _), _| stored_account != account_id);
        self.state.session_plans.remove(account_id);
        self.state.discipline_states.remove(account_id);
        self.state.managed_brackets.retain(|bracket_id, _| {
            !managed_bracket_ids
                .iter()
                .any(|deleted| deleted == bracket_id)
        });
        self.state.protective_orders.retain(|client_order_id, _| {
            !account_client_order_ids.contains(client_order_id.as_str())
        });
        self.state.trade_copiers.remove(account_id);
        for config in self.state.trade_copiers.values_mut() {
            config
                .targets
                .retain(|target| &target.account_id != account_id);
        }
        self.state
            .trade_copiers
            .retain(|_, config| !config.targets.is_empty());
        self.copy_dispatches.retain(|dispatch| {
            &dispatch.target_account_id != account_id
                && !account_client_order_ids.contains(&dispatch.source_client_order_id)
        });
    }

    pub(super) fn register_instrument(
        &mut self,
        instrument: TradingInstrument,
    ) -> Result<(), String> {
        if instrument.price_scale > 18 || instrument.quantity_scale > 18 {
            return Err("instrument trading scale exceeds 18".to_string());
        }
        if instrument
            .contract
            .order_quantity_increment
            .is_some_and(|increment| {
                increment.units() <= 0 || increment.scale() != instrument.quantity_scale
            })
        {
            return Err("instrument order quantity increment is invalid".to_string());
        }
        instrument
            .contract
            .validate()
            .map_err(|error| error.to_string())?;
        self.store.put_instrument(&instrument)?;
        self.state
            .instruments
            .insert(instrument.instrument_id.clone(), instrument);
        self.bump_revision()
    }

    pub(super) fn place_order(&mut self, command: &PlaceOrder) -> Result<Order, String> {
        match self.route(&command.account_id)? {
            VenueRoute::Simulated => {}
            VenueRoute::CtraderDemo => {
                if command.client_order_id.as_str().len() > 50 {
                    return Err("cTrader ClientOrderId must be at most 50 characters".into());
                }
                if let Some(existing) = self.state.orders.by_client(&command.client_order_id) {
                    return existing_client_order(existing, command)
                        .map_err(|error| error.to_string());
                }
                return self.place_broker_order(command);
            }
        }
        if let Some(existing) = self.state.orders.by_client(&command.client_order_id) {
            return existing_client_order(existing, command).map_err(|error| error.to_string());
        }
        self.check_new_order_interest(&command.instrument_id)
            .map_err(|error| error.to_string())?;
        let copier = self
            .state
            .trade_copiers
            .get(&command.account_id)
            .filter(|config| config.enabled)
            .cloned();
        if let Some(config) = &copier {
            for target in config.targets.iter().filter(|target| target.enabled) {
                if self.route(&target.account_id)? != VenueRoute::Simulated {
                    return Err("trade copier is unavailable for broker accounts".into());
                }
            }
        }
        let mut step = OrderStep::new(self.state.next_sequence);
        let source = self.stage_single_order(&mut step, command.clone(), true)?;
        let mut dispatches = Vec::new();
        if let Some(copier) = copier {
            for target in copier.targets.into_iter().filter(|target| target.enabled) {
                let client_id =
                    mirrored_client_order_id(&command.client_order_id, &target.account_id);
                let mirrored_client_order_id = client_id
                    .as_ref()
                    .map_or_else(|_| String::new(), |id| id.as_str().to_string());
                let result = client_id.and_then(|client_order_id| {
                    let quantity = multiply_quantity(command.quantity, target.quantity_multiplier)?;
                    self.stage_single_order(
                        &mut step,
                        PlaceOrder {
                            client_order_id,
                            account_id: target.account_id.clone(),
                            quantity,
                            ..command.clone()
                        },
                        true,
                    )
                });
                let dispatch = TradeCopyDispatch {
                    source_client_order_id: command.client_order_id.as_str().to_string(),
                    target_account_id: target.account_id,
                    mirrored_client_order_id,
                    accepted: result.is_ok(),
                    detail: result.as_ref().err().cloned(),
                };
                dispatches.push(dispatch);
            }
        }
        self.commit_order_step(step, dispatches)?;
        Ok(source)
    }

    fn stage_single_order(
        &mut self,
        step: &mut OrderStep,
        command: PlaceOrder,
        enforce_risk: bool,
    ) -> Result<Order, String> {
        let (order, event) = self.prepare_single_order(
            command,
            enforce_risk,
            step.next_sequence,
            step.inserted.len(),
        )?;
        step.next_sequence += 1;
        step.inserted.push((order.clone(), event));
        Ok(order)
    }

    pub(super) fn place_protective_order(
        &mut self,
        command: PlaceProtectiveOrder,
    ) -> Result<Order, String> {
        if self.route(&command.order.account_id)? != VenueRoute::Simulated {
            return Err("managed protective orders are unavailable for broker accounts".into());
        }
        if let Some(existing) = self.state.orders.by_client(&command.order.client_order_id) {
            let existing = existing_client_order(existing, &command.order)
                .map_err(|error| error.to_string())?;
            let role_matches = self
                .state
                .protective_orders
                .get(&command.order.client_order_id)
                .is_some_and(|protective| protective.role == command.role);
            return role_matches.then_some(existing).ok_or_else(|| {
                "protective client identifier has conflicting semantics".to_string()
            });
        }
        let shape_matches = matches!(
            (command.role, command.order.order_type),
            (ProtectiveOrderRole::StopLoss, OrderType::Stop)
                | (ProtectiveOrderRole::TakeProfit, OrderType::Limit)
        );
        if !shape_matches {
            return Err("protective order role does not match its order type".to_string());
        }
        let sequence = self.state.next_sequence;
        let (order, mut event) = self.prepare_single_order(command.order, false, sequence, 0)?;
        event.detail = Some(format!(
            "{} standalone {}",
            ManagedBracket::MANAGEMENT_LABEL,
            command.role.as_str()
        ));
        event.validate().map_err(|error| error.to_string())?;
        let protective = ProtectiveOrder {
            client_order_id: order.client_order_id.clone(),
            role: command.role,
        };
        let mut step = OrderStep::new(sequence + 1);
        step.inserted.push((order.clone(), event));
        step.protective.push(protective);
        self.commit_order_step(step, Vec::new())?;
        Ok(order)
    }

    pub(super) fn prepare_single_order(
        &mut self,
        mut command: PlaceOrder,
        enforce_risk: bool,
        sequence: u64,
        additional_open_orders: usize,
    ) -> Result<(Order, OrderEvent), String> {
        let account = self
            .state
            .accounts
            .get(&command.account_id)
            .ok_or_else(|| "trading account is not registered".to_string())?;
        match VenueRoute::from_account(account)? {
            VenueRoute::Simulated => {}
            VenueRoute::CtraderDemo => return Err(DEMO_VENUE_UNAVAILABLE.into()),
        }
        command.provenance.venue_id.clone_from(&account.venue_id);
        let account_currency = account.currency.clone();
        let instrument = self
            .state
            .instruments
            .get(&command.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?
            .clone();
        self.check_practice_market(&instrument)
            .map_err(|reason| reason.to_string())?;
        let risk_warnings = if enforce_risk {
            self.evaluate_order_risk(&command)?.warnings
        } else {
            self.validate_managed_exit(&command)?;
            Vec::new()
        };
        if instrument.contract.currency != account_currency {
            return Err("Practice contract currency differs from the account currency".to_string());
        }
        if command.quantity.scale() != instrument.quantity_scale
            || command
                .limit_price
                .into_iter()
                .chain(command.stop_price)
                .any(|price| price.scale() != instrument.price_scale)
        {
            return Err("order scales do not match the instrument".to_string());
        }
        check_quantity_increment(&instrument, command.quantity)?;
        if self
            .state
            .orders
            .open_count()
            .saturating_add(additional_open_orders)
            >= MAXIMUM_OPEN_ORDERS
        {
            return Err("simulated venue open-order limit reached".to_string());
        }
        let order = Order {
            id: OrderId::try_new(format!("sim-order-{sequence}"))
                .map_err(|error| error.to_string())?,
            client_order_id: command.client_order_id,
            account_id: command.account_id,
            instrument_id: command.instrument_id,
            side: command.side,
            order_type: command.order_type,
            time_in_force: command.time_in_force,
            quantity: command.quantity,
            filled_quantity: FixedPoint::try_new(0, command.quantity.scale())
                .map_err(|error| error.to_string())?,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            status: OrderStatus::Working,
            submitted_unix_nanos: command.submitted_unix_nanos,
            provenance: command.provenance.clone(),
        };
        order.validate().map_err(|error| error.to_string())?;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Accepted,
            event_unix_nanos: command.submitted_unix_nanos,
            detail: Some(order_acceptance_detail(&risk_warnings)),
            provenance: command.provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        Ok((order, event))
    }

    pub(super) fn validate_managed_exit(&self, command: &PlaceOrder) -> Result<(), String> {
        let position = self
            .state
            .positions
            .get(&(command.account_id.clone(), command.instrument_id.clone()))
            .ok_or_else(|| "managed exit requires an open position".to_string())?;
        let net_units = position
            .net_quantity
            .exact_rescale(command.quantity.scale())
            .map_err(|error| error.to_string())?
            .units();
        if net_units == 0
            || net_units.signum() == command.side.sign()
            || command.quantity.units().unsigned_abs() > net_units.unsigned_abs()
        {
            return Err("managed child order must reduce the open position".to_string());
        }
        Ok(())
    }
}
