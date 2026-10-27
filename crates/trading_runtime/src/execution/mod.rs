use super::{
    ClientOrderId, Fill, FillId, FillMemoryUpdate, FixedPoint, ModifyOrder, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderId, OrderStatus, OrderStep, OrderType,
    RevisionedCancelOrder, RevisionedModifyOrder, SimulatedMarketObservation, TimeInForce,
    TradingAccountId, TradingCore, TradingInstrument, VenueRoute, is_simulated,
};
use aeris_trading::Liquidity as LiquiditySide;

// The simulated venue's execution boundary has only one implementation of each policy:
// full size and the observed touch. Neither policy models depth or slippage.
pub(crate) trait Liquidity {
    fn executable_quantity(&self, requested: FixedPoint) -> FixedPoint;
}

pub(crate) struct TouchLiquidity;

impl Liquidity for TouchLiquidity {
    fn executable_quantity(&self, requested: FixedPoint) -> FixedPoint {
        requested
    }
}

pub(crate) trait FillPrice {
    fn price(&self, touch: FixedPoint) -> FixedPoint;
}

pub(crate) struct TouchPrice;

impl FillPrice for TouchPrice {
    fn price(&self, touch: FixedPoint) -> FixedPoint {
        touch
    }
}

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
            VenueRoute::BrokerDemo => return self.cancel_broker_order(order),
        }
        if !order.status.is_open() {
            return Ok(order);
        }
        let mut step = OrderStep::new(self.state.next_sequence);
        let cancelled = self.stage_cancellation(
            &mut step,
            order,
            reconcile_bracket,
            crate::book::lifecycle::CancellationCause::User,
        )?;
        self.commit_order_step(step, Vec::new())?;
        Ok(cancelled)
    }

    pub(super) fn stage_cancellation(
        &self,
        step: &mut OrderStep,
        order: Order,
        reconcile_bracket: bool,
        cause: crate::book::lifecycle::CancellationCause,
    ) -> Result<Order, String> {
        let sequence = step.next_sequence;
        let provenance = order.provenance.clone();
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Cancelled,
            reason: Some(cause.reason()),
            event_unix_nanos: self.command_now,
            detail: Some(match cause {
                crate::book::lifecycle::CancellationCause::NoQuote => {
                    "no quote within five seconds".to_string()
                }
                crate::book::lifecycle::CancellationCause::Cascade => {
                    "cancelled with its bracket".to_string()
                }
                _ => "cancelled by local command".to_string(),
            }),
            provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        step.next_sequence += 1;
        let cancelled = if cause == crate::book::lifecycle::CancellationCause::User
            && order.status != OrderStatus::Inactive
        {
            crate::book::lifecycle::cancel(order, self.command_now)
        } else {
            // A user cancel of a held group leg takes the cause path: the plain user guard
            // refuses inactive orders, but cancel-all must still reach them.
            crate::book::lifecycle::cancel_with_cause(order, cause, self.command_now)
                .map(|(order, _)| order)
        }
        .map_err(|error| error.to_string())?;
        step.updated.push((cancelled.clone(), event));
        if reconcile_bracket {
            self.stage_bracket_termination(step, &cancelled)?;
        }
        Ok(cancelled)
    }

    fn stage_bracket_termination(
        &self,
        step: &mut OrderStep,
        terminal: &Order,
    ) -> Result<(), String> {
        // Group-owned brackets: a terminal pending entry (or one of its held legs) cancels the
        // group, the still-working entry and every other held leg; a terminal active stop
        // cancels the open targets. Cancelling one active target removes only that target.
        let owned = self.state.order_groups.values().find_map(|group| {
            if group.kind != super::GroupKind::Bracket {
                return None;
            }
            let leg_terminated = group
                .legs
                .iter()
                .position(|leg| leg.order_id == terminal.id);
            if group.status == super::GroupStatus::Pending
                && (group.entry.as_ref() == Some(&terminal.id) || leg_terminated.is_some())
            {
                return Some((group.clone(), leg_terminated, false));
            }
            if group.status == super::GroupStatus::Active && leg_terminated == Some(0) {
                return Some((group.clone(), leg_terminated, true));
            }
            None
        });
        if let Some((mut group, terminated_leg, targets_only)) = owned {
            group.status = super::GroupStatus::Cancelled;
            group.revision = group
                .revision
                .checked_add(1)
                .ok_or_else(|| "order group revision overflowed".to_string())?;
            let legs = group.legs.clone();
            let entry_id = group.entry.clone();
            step.groups.push(group);
            for (index, leg) in legs.iter().enumerate() {
                if Some(index) == terminated_leg {
                    continue;
                }
                if targets_only && !matches!(leg.role, super::LegRole::TakeProfit { .. }) {
                    continue;
                }
                let Some(order) = self.state.orders.get(&leg.order_id).cloned() else {
                    continue;
                };
                if order.status.is_open()
                    && !step
                        .updated
                        .iter()
                        .any(|(updated, _)| updated.id == order.id)
                {
                    self.stage_cancellation(
                        step,
                        order,
                        false,
                        crate::book::lifecycle::CancellationCause::Cascade,
                    )?;
                }
            }
            // A pending group terminated through one of its held legs must also cancel the
            // still-working entry; otherwise the naked entry can fill without protection.
            if !targets_only
                && let Some(entry_id) = entry_id
                && let Some(entry) = self.state.orders.get(&entry_id).cloned()
                && entry.status.is_open()
                && !step
                    .updated
                    .iter()
                    .any(|(updated, _)| updated.id == entry.id)
            {
                self.stage_cancellation(
                    step,
                    entry,
                    false,
                    crate::book::lifecycle::CancellationCause::Cascade,
                )?;
            }
        }
        // A standalone protection group retires when its only order is terminal; a fill
        // retires it in the quote path instead.
        let protection = self.state.order_groups.values().find_map(|group| {
            (group.kind == super::GroupKind::Protection
                && group.status == super::GroupStatus::Active
                && group.legs.iter().any(|leg| leg.order_id == terminal.id))
            .then(|| group.clone())
        });
        if let Some(mut group) = protection {
            group.status = super::GroupStatus::Cancelled;
            group.revision = group
                .revision
                .checked_add(1)
                .ok_or_else(|| "order group revision overflowed".to_string())?;
            step.groups.push(group);
        }
        self.stage_oco_retirement(step, terminal)
    }

    /// Explicit OCO pairs cancel siblings only on a fill; an externally terminated member
    /// leaves its sibling working, and the group retires once no member remains open.
    fn stage_oco_retirement(&self, step: &mut OrderStep, terminal: &Order) -> Result<(), String> {
        let Some(mut group) = self
            .state
            .order_groups
            .values()
            .find(|group| {
                group.kind == super::GroupKind::Oco
                    && group.status == super::GroupStatus::Active
                    && group.legs.iter().any(|leg| leg.order_id == terminal.id)
            })
            .cloned()
        else {
            return Ok(());
        };
        let any_open = group.legs.iter().any(|leg| {
            leg.order_id != terminal.id
                && self
                    .state
                    .orders
                    .get(&leg.order_id)
                    .is_some_and(|order| order.status.is_open())
                && !step
                    .updated
                    .iter()
                    .any(|(updated, _)| updated.id == leg.order_id)
        });
        if !any_open {
            group.status = super::GroupStatus::Cancelled;
            group.revision = group
                .revision
                .checked_add(1)
                .ok_or_else(|| "order group revision overflowed".to_string())?;
            step.groups.push(group);
        }
        Ok(())
    }

    /// One bounded owner deadline scan, over the indexed open-order set only.
    pub(crate) fn next_market_timeout_unix_nanos(&self) -> Option<i64> {
        self.state
            .orders
            .open_orders()
            .filter(|order| {
                order.order_type == OrderType::Market
                    && is_simulated(&self.state.accounts, &order.account_id)
            })
            .map(|order| order.accepted_unix_nanos.saturating_add(5_000_000_000))
            .min()
    }

    /// The order book bounds this scan to the global open-order capacity; waiting broker
    /// reverses share the deadline so their expiry runs on the owner timer.
    pub(crate) fn next_order_deadline_unix_nanos(&self) -> Option<i64> {
        self.next_market_timeout_unix_nanos()
            .into_iter()
            .chain(self.next_reverse_deadline_unix_nanos())
            .chain(self.state.orders.open_orders().filter_map(|order| {
                (order.time_in_force == TimeInForce::Day
                    && is_simulated(&self.state.accounts, &order.account_id))
                .then_some(order.expires_at_unix_nanos)
                .flatten()
            }))
            .min()
    }

    /// Recover the acceptance-time close from persisted verified route and
    /// recurring hours, before the live feed attaches. Unverified legacy DAY
    /// rows stay fenced rather than receiving a guessed deadline.
    pub(crate) fn recover_day_deadlines(&mut self) -> Result<(), String> {
        let recovered = self
            .state
            .orders
            .open_orders()
            .filter_map(|order| {
                if order.time_in_force != TimeInForce::Day
                    || order.expires_at_unix_nanos.is_some()
                    || !is_simulated(&self.state.accounts, &order.account_id)
                {
                    return None;
                }
                let instrument = self.state.instruments.get(&order.instrument_id)?;
                let route = self.state.market_routes.get(&order.instrument_id)?;
                let deadline = if instrument.contract.session_hours.is_empty() {
                    (aeris_instruments::calendar::classify_venue(&route.venue_id)
                        == Some(aeris_instruments::calendar::Venue::Continuous))
                    .then(|| crate::market::feed::next_utc_midnight(order.accepted_unix_nanos))
                    .flatten()?
                } else {
                    use aeris_instruments::calendar::weekly_hours::{WeeklyPhase, weekly_status};
                    let status = weekly_status(
                        &instrument.contract.session_hours,
                        order.accepted_unix_nanos,
                    )?;
                    match status.phase {
                        WeeklyPhase::Open => status.end?,
                        WeeklyPhase::AlwaysOpen => {
                            crate::market::feed::next_utc_midnight(order.accepted_unix_nanos)?
                        }
                        WeeklyPhase::Closed => return None,
                    }
                };
                Some((order.id.clone(), deadline))
            })
            .collect::<Vec<_>>();
        if recovered.is_empty() {
            return Ok(());
        }
        self.store.backfill_day_deadlines(&recovered)?;
        for (id, deadline) in recovered {
            if let Some(mut order) = self.state.orders.get(&id).cloned() {
                order.expires_at_unix_nanos = Some(deadline);
                self.state.orders.insert(id, order);
            }
        }
        self.state.revision = self.store.revision()?;
        Ok(())
    }

    pub(crate) fn expire_due_day_orders(&mut self) -> Result<(), String> {
        let due = self
            .state
            .orders
            .open_orders()
            .filter(|order| {
                order.time_in_force == TimeInForce::Day
                    && order
                        .expires_at_unix_nanos
                        .is_some_and(|at| at <= self.command_now)
                    && is_simulated(&self.state.accounts, &order.account_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if due.is_empty() {
            return Ok(());
        }
        let mut step = OrderStep::new(self.state.next_sequence);
        for order in due {
            let deadline = order
                .expires_at_unix_nanos
                .ok_or_else(|| "DAY order has no expiry deadline".to_string())?;
            let expired = crate::book::lifecycle::expire(order, deadline)
                .map_err(|error| error.to_string())?;
            let sequence = step.next_sequence;
            let event = OrderEvent {
                id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: expired.id.clone(),
                sequence,
                kind: OrderEventKind::Expired,
                reason: None,
                event_unix_nanos: deadline,
                detail: Some("DAY session ended".to_string()),
                provenance: expired.provenance.clone(),
            };
            event.validate().map_err(|error| error.to_string())?;
            step.next_sequence += 1;
            step.updated.push((expired.clone(), event));
            self.stage_bracket_termination(&mut step, &expired)?;
        }
        self.commit_order_step(step, Vec::new())
    }

    pub(crate) fn cancel_due_markets(&mut self) -> Result<(), String> {
        let due = self
            .state
            .orders
            .open_orders()
            .filter(|order| {
                order.order_type == OrderType::Market
                    && order.accepted_unix_nanos.saturating_add(5_000_000_000) <= self.command_now
                    && is_simulated(&self.state.accounts, &order.account_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if due.is_empty() {
            return Ok(());
        }
        let mut step = OrderStep::new(self.state.next_sequence);
        for order in due {
            self.stage_cancellation(
                &mut step,
                order,
                true,
                crate::book::lifecycle::CancellationCause::NoQuote,
            )?;
        }
        self.commit_order_step(step, Vec::new())
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
            VenueRoute::BrokerDemo => return self.modify_broker_order(order, &command),
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
        crate::core::admission::validate_time_in_force(order.order_type, command.time_in_force)
            .and_then(|()| {
                crate::core::admission::validate_order_prices(
                    instrument,
                    command.limit_price,
                    command.stop_price,
                )
            })
            .map_err(|reason| reason.to_string())?;
        // A DAY order keeps the session close fixed at its acceptance; a change to DAY
        // expires at the current session close, exactly as a DAY placement would.
        let expires_at_unix_nanos = match (order.time_in_force, command.time_in_force) {
            (TimeInForce::Day, TimeInForce::Day) => order.expires_at_unix_nanos,
            (_, TimeInForce::Day) => Some(
                self.simulated_day_deadline(&order.instrument_id)
                    .map_err(|reason| reason.to_string())?,
            ),
            _ => None,
        };
        let modified = Order {
            time_in_force: command.time_in_force,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            expires_at_unix_nanos,
            provenance: command.provenance.clone(),
            ..order.clone()
        };
        let modified = crate::book::lifecycle::modify(modified, self.command_now)
            .map_err(|error| error.to_string())?;
        modified.validate().map_err(|error| error.to_string())?;
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
        // A pending group bracket's held legs keep their tick offsets and follow the entry.
        self.stage_owned_leg_reprices(&mut step, &modified)?;
        self.commit_order_step(step, Vec::new())?;
        Ok(modified)
    }

    pub(super) fn cancel_all(
        &mut self,
        account_id: Option<&TradingAccountId>,
    ) -> Result<Vec<Order>, String> {
        let (cancelled, failure) = self.cancel_open_orders(account_id, None)?;
        failure.map_or(Ok(cancelled), Err)
    }

    /// Cancels every open order in scope: simulated orders at once, broker orders by
    /// request. A broker order that cannot be requested (for example with its venue
    /// disconnected) never stops the others; it is reported in the returned summary.
    pub(super) fn cancel_open_orders(
        &mut self,
        account_id: Option<&TradingAccountId>,
        instrument_id: Option<&aeris_instruments::InstrumentId>,
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
        let open_orders = open_orders
            .into_iter()
            .filter(|order| {
                instrument_id.is_none_or(|instrument| instrument == &order.instrument_id)
            })
            .collect::<Vec<_>>();
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
                    cancelled.push(self.stage_cancellation(
                        &mut step,
                        order,
                        true,
                        crate::book::lifecycle::CancellationCause::User,
                    )?);
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
            .map(|error| format!("{failed} broker order(s) could not be cancelled: {error}"));
        Ok((cancelled, failure))
    }

    pub(super) fn execute_market_fills(
        &mut self,
        candidates: Vec<(OrderId, FixedPoint)>,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
        placement: bool,
    ) -> Result<Vec<Fill>, String> {
        let mut fills = Vec::with_capacity(candidates.len());
        for (order_id, price) in candidates {
            let mut order = self
                .state
                .orders
                .get(&order_id)
                .cloned()
                .ok_or_else(|| "working order disappeared".to_string())?;
            // A resting limit filled by the quote is the maker; a placement fill, a stop
            // trigger and a market order are the taker.
            let liquidity = if !placement
                && (matches!(order.order_type, OrderType::Limit)
                    || (matches!(order.order_type, OrderType::StopLimit)
                        && order.triggered_unix_nanos.is_some_and(|triggered| {
                            triggered < observation.provenance.observed_unix_nanos
                        }))) {
                LiquiditySide::Maker
            } else {
                LiquiditySide::Taker
            };
            if matches!(order.order_type, OrderType::Stop | OrderType::StopLimit)
                && order.triggered_unix_nanos.is_none()
            {
                order = self.trigger_simulated_stop(order, observation)?;
            }
            let sequence = self.state.next_sequence;
            let fill = Fill {
                id: FillId::try_new(format!("sim-fill-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: order.id.clone(),
                account_id: order.account_id.clone(),
                instrument_id: order.instrument_id.clone(),
                side: order.side,
                price: TouchPrice.price(price),
                quantity: TouchLiquidity.executable_quantity(order.quantity),
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
            let (position, cost_basis, policy_update) = self.apply_fill(
                &fill,
                previous_position.as_ref(),
                self.state
                    .accounts
                    .get(&order.account_id)
                    .ok_or_else(|| "fill account disappeared".to_string())?,
                instrument,
                liquidity,
            )?;
            let filled = crate::book::lifecycle::fill_simulated(order, self.command_now)
                .map_err(|error| error.to_string())?;
            self.apply_fill_to_memory(
                filled,
                event,
                &fill,
                FillMemoryUpdate {
                    position,
                    cost_basis,
                    position_key,
                    policy_update,
                    next_sequence: sequence + 1,
                },
            );
            fills.push(fill);
        }
        Ok(fills)
    }

    pub(super) fn trigger_simulated_stop(
        &mut self,
        order: Order,
        observation: &SimulatedMarketObservation,
    ) -> Result<Order, String> {
        let sequence = self.state.next_sequence;
        let (triggered, kind) =
            crate::book::lifecycle::trigger(order, observation.provenance.observed_unix_nanos)
                .map_err(|error| error.to_string())?;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: triggered.id.clone(),
            sequence,
            kind,
            reason: None,
            event_unix_nanos: observation.provenance.observed_unix_nanos,
            detail: Some("stop triggered at touch".to_string()),
            provenance: observation.provenance.clone(),
        };
        event.validate().map_err(|error| error.to_string())?;
        self.state
            .orders
            .insert(triggered.id.clone(), triggered.clone());
        self.state.order_events.push_back(event);
        self.state.next_sequence = sequence + 1;
        Ok(triggered)
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

#[cfg(test)]
mod touch_tests {
    use super::*;

    #[test]
    fn touch_hooks_keep_the_exact_price_and_full_order_quantity() {
        let touch = FixedPoint::try_new(10_125, 2).expect("touch");
        let requested = FixedPoint::try_new(3, 0).expect("quantity");
        assert_eq!(TouchPrice.price(touch), touch);
        assert_eq!(TouchLiquidity.executable_quantity(requested), requested);
    }
}
