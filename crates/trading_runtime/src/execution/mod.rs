use super::{
    ClientOrderId, Fill, FillId, FillMemoryUpdate, FixedPoint, ManagedBracketStatus, ModifyOrder,
    Order, OrderEvent, OrderEventId, OrderEventKind, OrderId, OrderStatus, OrderStep,
    RevisionedCancelOrder, RevisionedModifyOrder, SimulatedMarketObservation, TimeInForce,
    TradingAccountId, TradingCore, TradingInstrument, VenueRoute, is_simulated, planned_leg,
};

impl TradingCore {
    pub(super) fn modify_order_from_market(
        &mut self,
        mut order: ModifyOrder,
    ) -> Result<Order, String> {
        self.state
            .orders
            .by_client(&order.client_order_id)
            .ok_or_else(|| "practice order is not registered".to_string())
            .and_then(|current| self.market_observation(&current.instrument_id))
            .and_then(|quote| {
                order.provenance = quote.provenance;
                self.modify_order(order)
            })
    }

    pub(super) fn checked_order(
        &self,
        client_order_id: &ClientOrderId,
        expected_revision: Option<u32>,
    ) -> Result<Result<(), Box<Order>>, String> {
        let current = self
            .state
            .orders
            .by_client(client_order_id)
            .ok_or("order client identifier is not registered")?;
        Ok(crate::book::lifecycle::expect_revision(
            current,
            expected_revision,
        ))
    }

    pub(super) fn modify_order_checked_internal(
        &mut self,
        command: RevisionedModifyOrder,
    ) -> Result<Result<Order, Box<Order>>, String> {
        match self.checked_order(&command.order.client_order_id, command.expected_revision)? {
            Ok(()) => self.modify_order(command.order).map(Ok),
            Err(current) => Ok(Err(current)),
        }
    }

    pub(super) fn modify_order_from_market_checked_internal(
        &mut self,
        mut command: RevisionedModifyOrder,
    ) -> Result<Result<Order, Box<Order>>, String> {
        match self.checked_order(&command.order.client_order_id, command.expected_revision)? {
            Ok(()) => {
                let current = self
                    .state
                    .orders
                    .by_client(&command.order.client_order_id)
                    .ok_or("order client identifier is not registered")?;
                let quote = self.market_observation(&current.instrument_id)?;
                command.order.provenance = quote.provenance;
                self.modify_order(command.order).map(Ok)
            }
            Err(current) => Ok(Err(current)),
        }
    }

    pub(super) fn cancel_order_checked_internal(
        &mut self,
        command: &RevisionedCancelOrder,
    ) -> Result<Result<Order, Box<Order>>, String> {
        match self.checked_order(&command.client_order_id, command.expected_revision)? {
            Ok(()) => self.cancel_order(&command.client_order_id).map(Ok),
            Err(current) => Ok(Err(current)),
        }
    }

    pub(super) fn cancel_order(
        &mut self,
        client_order_id: &ClientOrderId,
    ) -> Result<Order, String> {
        self.cancel_order_internal(client_order_id, true)
    }

    pub(super) fn cancel_order_internal(
        &mut self,
        client_order_id: &ClientOrderId,
        reconcile_bracket: bool,
    ) -> Result<Order, String> {
        let order = self
            .state
            .orders
            .by_client(client_order_id)
            .cloned()
            .ok_or_else(|| "order client identifier is not registered".to_string())?;
        match self.route(&order.account_id)? {
            VenueRoute::Simulated => {}
            VenueRoute::CtraderDemo => return self.cancel_broker_order(order),
        }
        if !order.status.is_open() {
            return Ok(order);
        }
        let mut step = OrderStep::new(self.state.next_sequence);
        let cancelled = self.stage_cancellation(&mut step, order, reconcile_bracket)?;
        self.commit_order_step(step, Vec::new())?;
        Ok(cancelled)
    }

    fn stage_cancellation(
        &self,
        step: &mut OrderStep,
        order: Order,
        reconcile_bracket: bool,
    ) -> Result<Order, String> {
        let sequence = step.next_sequence;
        let provenance = order.provenance.clone();
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Cancelled,
            reason: Some(aeris_trading::OrderEventReason::UserCancel),
            event_unix_nanos: self.command_now,
            detail: Some("cancelled by local command".to_string()),
            provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        step.next_sequence += 1;
        let cancelled = crate::book::lifecycle::cancel(order, self.command_now)
            .map_err(|error| error.to_string())?;
        step.updated.push((cancelled.clone(), event));
        if reconcile_bracket {
            let affected = self.state.managed_brackets.iter().find_map(|(_, bracket)| {
                let entry_cancelled = bracket.status == ManagedBracketStatus::AwaitingEntry
                    && bracket.entry_client_order_id == cancelled.client_order_id;
                let stop_cancelled = bracket.status == ManagedBracketStatus::Active
                    && bracket.stop_client_order_id.as_ref() == Some(&cancelled.client_order_id);
                (entry_cancelled || stop_cancelled).then(|| {
                    (
                        bracket.clone(),
                        stop_cancelled.then(|| bracket.target_client_order_ids.clone()),
                    )
                })
            });
            if let Some((mut bracket, targets)) = affected {
                bracket.status = ManagedBracketStatus::Cancelled;
                step.brackets.push(bracket);
                for target in targets.into_iter().flatten() {
                    let target_order = self
                        .state
                        .orders
                        .by_client(&target)
                        .cloned()
                        .ok_or_else(|| "managed target disappeared".to_string())?;
                    if target_order.status.is_open() {
                        self.stage_cancellation(step, target_order, false)?;
                    }
                }
            }
        }
        Ok(cancelled)
    }

    pub(super) fn modify_order(&mut self, mut command: ModifyOrder) -> Result<Order, String> {
        let order = self
            .state
            .orders
            .by_client(&command.client_order_id)
            .cloned()
            .ok_or_else(|| "order client identifier is not registered".to_string())?;
        let account = self
            .state
            .accounts
            .get(&order.account_id)
            .ok_or_else(|| "trading account is not registered".to_string())?;
        match VenueRoute::from_account(account)? {
            VenueRoute::Simulated => {}
            VenueRoute::CtraderDemo => return self.modify_broker_order(order, &command),
        }
        command.provenance.venue_id.clone_from(&account.venue_id);
        if !matches!(
            order.status,
            OrderStatus::Working | OrderStatus::PartiallyFilled
        ) {
            return Err("only working orders can be modified".to_string());
        }
        let instrument = self
            .state
            .instruments
            .get(&order.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        if command.modified_unix_nanos <= 0 {
            return Err("order modification timestamp must be positive".to_string());
        }
        command
            .provenance
            .validate()
            .map_err(|error| error.to_string())?;
        if command
            .limit_price
            .into_iter()
            .chain(command.stop_price)
            .any(|price| price.scale() != instrument.price_scale)
        {
            return Err("modified order scales do not match the instrument".to_string());
        }
        let modified = Order {
            time_in_force: command.time_in_force,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            provenance: command.provenance.clone(),
            ..order.clone()
        };
        let modified = crate::book::lifecycle::modify(modified, self.command_now)
            .map_err(|error| error.to_string())?;
        modified.validate().map_err(|error| error.to_string())?;
        // A pending bracket's legs keep their distances and follow the entry.
        if let Some(bracket) = self.awaiting_bracket(&order.client_order_id) {
            planned_leg::planned_legs(bracket, &modified, instrument)?;
        }
        let sequence = self.state.next_sequence;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Modified,
            reason: None,
            event_unix_nanos: command.modified_unix_nanos,
            detail: Some("modified by local command".to_string()),
            provenance: command.provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        let mut step = OrderStep::new(sequence + 1);
        step.updated.push((modified.clone(), event));
        self.commit_order_step(step, Vec::new())?;
        Ok(modified)
    }

    pub(super) fn cancel_all(
        &mut self,
        account_id: Option<&TradingAccountId>,
    ) -> Result<Vec<Order>, String> {
        let (cancelled, failure) = self.cancel_open_orders(account_id)?;
        failure.map_or(Ok(cancelled), Err)
    }

    /// Cancels every open order in scope: simulated orders at once, broker orders by
    /// request. A broker order that cannot be requested (for example with its venue
    /// disconnected) never stops the others; it is reported in the returned summary.
    pub(super) fn cancel_open_orders(
        &mut self,
        account_id: Option<&TradingAccountId>,
    ) -> Result<(Vec<Order>, Option<String>), String> {
        if let Some(account_id) = account_id {
            self.route(account_id)?;
        }
        // Globally, accounts Aeris cannot trade (data-only live accounts) are skipped.
        let open_orders = if let Some(account_id) = account_id {
            self.state.orders.open_for_account(account_id)
        } else {
            self.state.orders.open_orders().collect()
        };
        let candidates = open_orders
            .into_iter()
            .filter_map(|order| {
                self.route(&order.account_id)
                    .ok()
                    .map(|route| (order.client_order_id.clone(), route))
            })
            .collect::<Vec<_>>();
        let mut cancelled = Vec::new();
        let mut step = OrderStep::new(self.state.next_sequence);
        let mut broker_candidates = Vec::new();
        let mut failed = 0_usize;
        let mut last_error = None;
        for (client_order_id, route) in candidates {
            if route == VenueRoute::Simulated {
                let order = self
                    .state
                    .orders
                    .by_client(&client_order_id)
                    .cloned()
                    .ok_or("order client identifier is not registered")?;
                if order.status.is_open()
                    && !step
                        .updated
                        .iter()
                        .any(|(updated, _)| updated.id == order.id)
                {
                    cancelled.push(self.stage_cancellation(&mut step, order, true)?);
                }
            } else {
                broker_candidates.push(client_order_id);
            }
        }
        self.commit_order_step(step, Vec::new())?;
        // A broker's asynchronous requests cannot join the simulated SQLite step.
        for client_order_id in broker_candidates {
            match self.cancel_order(&client_order_id) {
                Ok(order) => cancelled.push(order),
                Err(error) => {
                    failed += 1;
                    last_error = Some(error);
                }
            }
        }
        let failure = last_error
            .map(|error| format!("{failed} cTrader order(s) could not be cancelled: {error}"));
        Ok((cancelled, failure))
    }

    pub(super) fn execute_market_fills(
        &mut self,
        candidates: Vec<(OrderId, FixedPoint)>,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<Vec<Fill>, String> {
        let mut fills = Vec::with_capacity(candidates.len());
        for (order_id, price) in candidates {
            let order = self
                .state
                .orders
                .get(&order_id)
                .cloned()
                .ok_or_else(|| "working order disappeared".to_string())?;
            let sequence = self.state.next_sequence;
            let fill = Fill {
                id: FillId::try_new(format!("sim-fill-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: order.id.clone(),
                account_id: order.account_id.clone(),
                instrument_id: order.instrument_id.clone(),
                side: order.side,
                price,
                quantity: order.quantity,
                execution_unix_nanos: observation.provenance.observed_unix_nanos,
                provenance: observation.provenance.clone(),
            };
            fill.validate().map_err(|error| error.to_string())?;
            let event = OrderEvent {
                id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: order.id.clone(),
                sequence,
                kind: OrderEventKind::Filled,
                reason: Some(aeris_trading::OrderEventReason::TouchFill),
                event_unix_nanos: fill.execution_unix_nanos,
                detail: Some("touch fill".to_string()),
                provenance: fill.provenance.clone(),
            };
            let position_key = (order.account_id.clone(), order.instrument_id.clone());
            let previous_position = self.state.positions.get(&position_key).cloned();
            let (position, policy_update) = self.apply_fill(
                &fill,
                previous_position.as_ref(),
                self.state
                    .accounts
                    .get(&order.account_id)
                    .ok_or_else(|| "fill account disappeared".to_string())?,
                instrument,
            )?;
            let filled = crate::book::lifecycle::fill_simulated(order, self.command_now)
                .map_err(|error| error.to_string())?;
            self.apply_fill_to_memory(
                filled,
                event,
                &fill,
                FillMemoryUpdate {
                    position,
                    position_key,
                    policy_update,
                    next_sequence: sequence + 1,
                },
            );
            fills.push(fill);
        }
        Ok(fills)
    }

    pub(super) fn cancel_unfilled_immediate_orders(
        &mut self,
        observation: &SimulatedMarketObservation,
        candidate_ids: &std::collections::BTreeSet<OrderId>,
    ) -> Result<bool, String> {
        let immediate_or_cancel = self
            .state
            .orders
            .executable_for(&observation.instrument_id)
            .filter(|order| {
                is_simulated(&self.state.accounts, &order.account_id)
                    && matches!(
                        order.time_in_force,
                        TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill
                    )
                    && !candidate_ids.contains(&order.id)
            })
            .map(|order| order.client_order_id.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for client_order_id in immediate_or_cancel {
            self.cancel_order(&client_order_id)?;
            changed = true;
        }
        Ok(changed)
    }
}
