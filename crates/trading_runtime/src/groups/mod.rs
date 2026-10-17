pub mod model;
pub(super) mod oco;
pub(super) mod owned;
pub(super) mod planned_leg;
pub(super) mod protection;
pub(super) mod recovery;

use super::{
    ArmedBracket, BROKER_PNL_UNAVAILABLE, BracketPlacement, BracketStrategyTemplate, BracketTarget,
    ClientOrderId, Fill, FixedPoint, InstrumentId, MAXIMUM_MANAGED_BRACKETS,
    MAXIMUM_STRATEGY_TEMPLATES, ManagedBracket, ManagedBracketStatus, ModifyOrder, Order,
    OrderEvent, OrderEventId, OrderEventKind, OrderSide, OrderStatus, OrderStep, OrderType,
    PlaceBracket, PlaceChartBracket, PlaceInlineBracket, PlaceOco, PlaceOrder,
    SimulatedMarketObservation, TimeInForce, TradingAccountId, TradingCore, TradingInstrument,
    VenueRoute, allocated_target_quantities, favorable_tick_distance, instrument_tick_units,
    managed_child_client_order_id, negative_loss, nonnegative_difference, opposite_side,
    order_increment_units, remaining_limit, scale_currency_by_quantity_and_ticks, shifted_price,
    subtract_fixed, tick_value, tighter_stop,
};

pub(crate) fn chart_bracket_template(
    instrument: &TradingInstrument,
    side: OrderSide,
    entry: FixedPoint,
    stop: FixedPoint,
    target: FixedPoint,
    id: &str,
) -> Result<BracketStrategyTemplate, String> {
    let valid = match side {
        OrderSide::Buy => stop.units() < entry.units() && target.units() > entry.units(),
        OrderSide::Sell => stop.units() > entry.units() && target.units() < entry.units(),
    };
    if !valid || stop.scale() != entry.scale() || target.scale() != entry.scale() {
        return Err("bracket stop and target must be on opposite sides of the entry".to_string());
    }
    let tick = u64::try_from(instrument_tick_units(instrument, entry)?)
        .map_err(|_| "bracket price grid is invalid".to_string())?;
    let offset = |price: FixedPoint| {
        let distance = entry.units().abs_diff(price.units());
        if tick == 0 || distance == 0 || !distance.is_multiple_of(tick) {
            return Err("bracket levels must be on the instrument price grid".to_string());
        }
        u32::try_from(distance / tick).map_err(|_| "bracket leg distance overflowed".to_string())
    };
    Ok(BracketStrategyTemplate {
        template_id: format!("chart-bracket-{id}"),
        revision: 1,
        name: "Chart bracket".to_string(),
        stop_offset_ticks: offset(stop)?,
        targets: vec![BracketTarget {
            offset_ticks: offset(target)?,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    })
}

impl TradingCore {
    pub(super) fn place_inline_bracket_from_market(
        &mut self,
        bracket: PlaceInlineBracket,
    ) -> Result<BracketPlacement, String> {
        let entry = self.market_resting_bracket_entry(bracket.entry)?;
        self.place_bracket_with_template(&entry, &bracket.template)
    }

    pub(super) fn place_chart_bracket(
        &mut self,
        bracket: PlaceChartBracket,
    ) -> Result<BracketPlacement, String> {
        let entry = self.market_resting_bracket_entry(bracket.entry)?;
        let instrument = self
            .state
            .instruments
            .get(&entry.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        let entry_price = entry
            .limit_price
            .or(entry.stop_price)
            .ok_or_else(|| "bracket entry price is unavailable".to_string())?;
        let template = chart_bracket_template(
            instrument,
            entry.side,
            entry_price,
            bracket.stop_price,
            bracket.target_price,
            entry.client_order_id.as_str(),
        )?;
        self.place_bracket_with_template(&entry, &template)
    }

    pub(super) fn register_strategy_template(
        &mut self,
        template: BracketStrategyTemplate,
    ) -> Result<(), String> {
        template.validate()?;
        if !self
            .state
            .strategy_templates
            .contains_key(&template.template_id)
            && self.state.strategy_templates.len() >= MAXIMUM_STRATEGY_TEMPLATES
        {
            return Err("strategy template limit reached".to_string());
        }
        if self
            .state
            .strategy_templates
            .get(&template.template_id)
            .is_some_and(|current| template.revision <= current.revision)
        {
            return Err("strategy template revision must advance".to_string());
        }
        self.store.put_strategy_template(&template)?;
        self.state
            .strategy_templates
            .insert(template.template_id.clone(), template);
        self.bump_revision()
    }

    pub(super) fn place_bracket(
        &mut self,
        command: &PlaceBracket,
    ) -> Result<BracketPlacement, String> {
        let template = self
            .state
            .strategy_templates
            .get(&command.template_id)
            .filter(|template| template.enabled)
            .cloned()
            .ok_or_else(|| "enabled bracket strategy template is not registered".to_string())?;
        self.place_bracket_with_template(&command.entry, &template)
    }

    pub(super) fn place_bracket_with_template(
        &mut self,
        entry_command: &PlaceOrder,
        template: &BracketStrategyTemplate,
    ) -> Result<BracketPlacement, String> {
        let resolved = self
            .resolve_order_instruction(entry_command.clone())
            .map_err(|reason| reason.to_string())?;
        let entry_command = &resolved;
        template.validate()?;
        if let Some(price) = entry_command.limit_price.or(entry_command.stop_price) {
            let instrument = self
                .state
                .instruments
                .get(&entry_command.instrument_id)
                .ok_or_else(|| "trading instrument is not registered".to_string())?;
            crate::core::admission::validate_bracket_levels(
                instrument,
                entry_command.side,
                price,
                template,
            )
            .map_err(|reason| reason.to_string())?;
        }
        if self.route(&entry_command.account_id)? != VenueRoute::Simulated {
            if template.trailing_stop.is_some()
                && !crate::core::admission::VenueCapabilities::for_route(VenueRoute::CtraderDemo)
                    .managed_trailing()
            {
                return Err("managed trailing stops are unavailable for broker accounts".into());
            }
            if template.break_even.is_some() {
                return Err("managed break-even stops are unavailable for broker accounts".into());
            }
            template.validate()?;
            self.check_bracket_stop_risk(
                &entry_command.account_id,
                &entry_command.instrument_id,
                entry_command.quantity,
                template,
            )?;
            return self
                .place_broker_bracket(entry_command, template)
                .map(BracketPlacement::BrokerProtected);
        }
        template.validate()?;
        if self
            .state
            .orders
            .values()
            .any(|order| order.client_order_id == entry_command.client_order_id)
        {
            return Err(
                "entry client identifier already belongs to a non-bracket order".to_string(),
            );
        }
        self.check_bracket_stop_risk(
            &entry_command.account_id,
            &entry_command.instrument_id,
            entry_command.quantity,
            template,
        )?;
        // Simulated brackets placed after the authority switch are owned only by the group
        // owner; the legacy writer below still serves rows restored from before the switch.
        self.place_owned_bracket(entry_command, template)
    }

    pub(super) fn check_bracket_stop_risk(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        quantity: FixedPoint,
        template: &BracketStrategyTemplate,
    ) -> Result<(), String> {
        let Some(profile) = self.state.risk_profiles.get(account_id) else {
            return Ok(());
        };
        if !profile.enabled {
            return Ok(());
        }
        let instrument = self
            .state
            .instruments
            .get(instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        let account = self
            .state
            .accounts
            .get(account_id)
            .ok_or_else(|| "trading account is not registered".to_string())?;
        let per_contract_tick = tick_value(instrument, account.currency_scale)?;
        let potential_loss = scale_currency_by_quantity_and_ticks(
            per_contract_tick,
            quantity,
            template.stop_offset_ticks,
        )?
        .exact_rescale(profile.daily_loss_limit.scale())
        .map_err(|error| error.to_string())?;
        let current = self.session_pnl(profile)?.ok_or(BROKER_PNL_UNAVAILABLE)?;
        let daily_remaining = remaining_limit(profile.daily_loss_limit, negative_loss(current)?)?;
        if potential_loss.units() >= daily_remaining.units() {
            return Err("bracket loss at stop would reach the daily loss limit".to_string());
        }
        if let Some(drawdown) = profile.trailing_drawdown {
            let state = self
                .state
                .risk_rule_states
                .get(account_id)
                .ok_or_else(|| "risk rule state is unavailable".to_string())?;
            let floor = subtract_fixed(state.peak_session_pnl, drawdown)?;
            let trailing_remaining = nonnegative_difference(current, floor)?;
            if potential_loss.units() >= trailing_remaining.units() {
                return Err("bracket loss at stop would reach the trailing drawdown".to_string());
            }
        }
        Ok(())
    }

    pub(super) fn retired_managed_bracket(&self) -> Result<Option<String>, String> {
        if self.state.managed_brackets.len() < MAXIMUM_MANAGED_BRACKETS {
            return Ok(None);
        }
        let retired = self
            .state
            .managed_brackets
            .iter()
            .filter(|(_, bracket)| {
                matches!(
                    bracket.status,
                    ManagedBracketStatus::Completed | ManagedBracketStatus::Cancelled
                )
            })
            .min_by_key(|(_, bracket)| {
                self.state
                    .orders
                    .by_client(&bracket.entry_client_order_id)
                    .map_or(i64::MIN, |order| order.submitted_unix_nanos)
            })
            .map(|(id, _)| id.clone())
            .ok_or_else(|| "active managed bracket limit reached".to_string())?;
        Ok(Some(retired))
    }

    pub(super) fn recover_managed_brackets(
        &mut self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        // Only brackets on the observed instrument: arming prices the children from this quote
        // and this instrument's ticks.
        let awaiting = self
            .state
            .managed_brackets
            .iter()
            .filter(|(_, bracket)| bracket.status == ManagedBracketStatus::AwaitingEntry)
            .filter_map(|(id, bracket)| {
                let order = self
                    .order_by_client_id(&bracket.entry_client_order_id)
                    .ok()?;
                (order.status == OrderStatus::Filled
                    && order.instrument_id == observation.instrument_id)
                    .then(|| {
                        self.state
                            .fills
                            .iter()
                            .rev()
                            .find(|fill| fill.order_id == order.id)
                            .cloned()
                            .map(|fill| (id.clone(), fill))
                    })?
            })
            .collect::<Vec<_>>();
        for (bracket_id, fill) in awaiting {
            self.arm_managed_bracket(&bracket_id, &fill, observation, instrument)?;
        }

        let interrupted =
            self.state
                .managed_brackets
                .iter()
                .filter(|(_, bracket)| bracket.status == ManagedBracketStatus::Active)
                .filter_map(|(id, bracket)| {
                    let stop_id = bracket.stop_client_order_id.as_ref()?;
                    let stop = self.order_by_client_id(stop_id).ok()?;
                    if stop.status == OrderStatus::Filled {
                        return Some((id.clone(), stop_id.clone()));
                    }
                    let target_status = |target_id: &ClientOrderId| {
                        self.order_by_client_id(target_id)
                            .ok()
                            .map(|target| target.status)
                    };
                    let filled_target = bracket
                        .target_client_order_ids
                        .iter()
                        .find(|target_id| target_status(target_id) == Some(OrderStatus::Filled))?;
                    let targets_open = bracket.target_client_order_ids.iter().any(|target_id| {
                        target_status(target_id).is_some_and(OrderStatus::is_open)
                    });
                    let entry = self
                        .order_by_client_id(&bracket.entry_client_order_id)
                        .ok()?;
                    let position_open = self
                        .state
                        .positions
                        .get(&(entry.account_id.clone(), entry.instrument_id.clone()))
                        .is_some_and(|position| position.net_quantity.units() != 0);
                    (!targets_open || !position_open).then(|| (id.clone(), filled_target.clone()))
                })
                .collect::<Vec<_>>();
        for (bracket_id, child_id) in interrupted {
            self.complete_managed_bracket(&bracket_id, &child_id)?;
        }
        Ok(())
    }

    pub(super) fn reconcile_managed_brackets(
        &mut self,
        fills: &[Fill],
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        for fill in fills {
            let filled_client_order_id = self
                .state
                .orders
                .get(&fill.order_id)
                .map(|order| order.client_order_id.clone())
                .ok_or_else(|| "filled bracket order disappeared".to_string())?;
            let action = self
                .state
                .managed_brackets
                .iter()
                .find_map(|(id, bracket)| {
                    if bracket.status == ManagedBracketStatus::AwaitingEntry
                        && bracket.entry_client_order_id == filled_client_order_id
                    {
                        Some((id.clone(), true))
                    } else if bracket.status == ManagedBracketStatus::Active
                        && (bracket.stop_client_order_id.as_ref() == Some(&filled_client_order_id)
                            || bracket
                                .target_client_order_ids
                                .contains(&filled_client_order_id))
                    {
                        Some((id.clone(), false))
                    } else {
                        None
                    }
                });
            let Some((bracket_id, is_entry)) = action else {
                continue;
            };
            if is_entry {
                self.arm_managed_bracket(&bracket_id, fill, observation, instrument)?;
            } else {
                self.complete_managed_bracket(&bracket_id, &filled_client_order_id)?;
            }
        }
        Ok(())
    }

    /// Arms a bracket whose entry filled. The entry has already executed, so a bracket whose
    /// children cannot be placed is rejected on its entry order instead of being retried: a
    /// retry fails the same way on every quote and keeps every other order on the instrument
    /// from filling.
    pub(super) fn arm_managed_bracket(
        &mut self,
        bracket_id: &str,
        entry_fill: &Fill,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let bracket = self
            .state
            .managed_brackets
            .get(bracket_id)
            .cloned()
            .ok_or_else(|| "managed bracket disappeared".to_string())?;
        let entry_order = self
            .state
            .orders
            .get(&entry_fill.order_id)
            .cloned()
            .ok_or_else(|| "managed bracket entry order disappeared".to_string())?;
        match self.prepare_armed_bracket(
            bracket.clone(),
            &entry_order,
            entry_fill,
            observation,
            instrument,
        ) {
            Ok(armed) => self.commit_armed_bracket(armed),
            Err(reason) => self.reject_managed_bracket(bracket, &entry_order, observation, &reason),
        }
    }

    /// How much of an entry fill is still open in the entry's direction: all of it when it
    /// opened or added to a position, the remainder when it reversed one, and nothing when it
    /// only reduced or closed an opposite position.
    pub(super) fn opened_exposure(
        &self,
        entry_order: &Order,
        entry_fill: &Fill,
    ) -> Result<FixedPoint, String> {
        let scale = entry_fill.quantity.scale();
        let net = self
            .state
            .positions
            .get(&(
                entry_order.account_id.clone(),
                entry_order.instrument_id.clone(),
            ))
            .map_or(Ok(0), |position| {
                position
                    .net_quantity
                    .exact_rescale(scale)
                    .map(FixedPoint::units)
                    .map_err(|error| error.to_string())
            })?;
        let open_in_entry_direction = net
            .checked_mul(entry_order.side.sign())
            .ok_or_else(|| "position quantity overflowed".to_string())?
            .max(0);
        FixedPoint::try_new(
            open_in_entry_direction.min(entry_fill.quantity.units()),
            scale,
        )
        .map_err(|error| error.to_string())
    }

    /// The children and active state of a bracket whose entry filled, sized to the exposure the
    /// entry opened: protection never opens a position of its own. A bracket whose entry opened
    /// nothing has nothing to protect and completes without children.
    pub(super) fn prepare_armed_bracket(
        &mut self,
        mut bracket: ManagedBracket,
        entry_order: &Order,
        entry_fill: &Fill,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<ArmedBracket, String> {
        let opened = self.opened_exposure(entry_order, entry_fill)?;
        let mut sequence = self.state.next_sequence;
        bracket.entry_price = Some(entry_fill.price);
        if opened.units() == 0 {
            bracket.status = ManagedBracketStatus::Completed;
            return Ok(ArmedBracket {
                bracket,
                children: Vec::new(),
                next_sequence: sequence,
            });
        }
        let tick_units = instrument_tick_units(instrument, entry_fill.price)?;
        let child_side = opposite_side(entry_order.side);
        let mut prepared = Vec::with_capacity(bracket.template.targets.len() + 1);
        let stop_price = shifted_price(
            entry_fill.price,
            bracket.template.stop_offset_ticks,
            -entry_order.side.sign(),
            tick_units,
        )?;
        let (stop_order, stop_event, stop_client_order_id) = self.prepare_initial_managed_stop(
            entry_order,
            stop_price,
            opened,
            observation,
            sequence,
        )?;
        prepared.push((stop_order, stop_event));
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| "managed bracket sequence overflowed".to_string())?;
        let target_quantities = allocated_target_quantities(
            opened,
            &bracket.template.targets,
            order_increment_units(instrument),
        )?;
        let mut target_client_order_ids = Vec::with_capacity(target_quantities.len());
        for (index, quantity) in target_quantities {
            let target = bracket.template.targets[index];
            let client_order_id = managed_child_client_order_id(
                &entry_order.client_order_id,
                "target",
                u32::try_from(index).map_err(|_| "bracket target index overflowed".to_string())?,
            )?;
            let limit_price = shifted_price(
                entry_fill.price,
                target.offset_ticks,
                entry_order.side.sign(),
                tick_units,
            )?;
            let (target_order, mut target_event) = self.prepare_single_order(
                PlaceOrder {
                    client_order_id: client_order_id.clone(),
                    account_id: entry_order.account_id.clone(),
                    instrument_id: entry_order.instrument_id.clone(),
                    side: child_side,
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GoodTillCancelled,
                    quantity,
                    limit_price: Some(limit_price),
                    stop_price: None,
                    submitted_unix_nanos: observation.provenance.observed_unix_nanos,
                    provenance: observation.provenance.clone(),
                },
                false,
                sequence,
                &prepared
                    .iter()
                    .map(|(order, _): &(Order, OrderEvent)| order.instrument_id.clone())
                    .collect::<Vec<_>>(),
            )?;
            target_event.detail = Some(format!(
                "{} scale-out target {}",
                ManagedBracket::MANAGEMENT_LABEL,
                index + 1
            ));
            target_event.reason = Some(aeris_trading::OrderEventReason::LocalManaged);
            target_event.validate().map_err(|error| error.to_string())?;
            prepared.push((target_order, target_event));
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| "managed bracket sequence overflowed".to_string())?;
            target_client_order_ids.push(client_order_id);
        }
        bracket.stop_client_order_id = Some(stop_client_order_id);
        bracket.target_client_order_ids = target_client_order_ids;
        bracket.status = ManagedBracketStatus::Active;
        bracket.validate()?;
        Ok(ArmedBracket {
            bracket,
            children: prepared,
            next_sequence: sequence,
        })
    }

    pub(super) fn commit_armed_bracket(&mut self, armed: ArmedBracket) -> Result<(), String> {
        self.state.next_sequence = armed.next_sequence;
        for (order, event) in armed.children {
            self.state.order_events.push_back(event);
            self.state.orders.insert(order.id.clone(), order);
        }
        self.state
            .managed_brackets
            .insert(armed.bracket.bracket_id.clone(), armed.bracket);
        self.bump_revision()
    }

    /// Cancels a bracket that could not be armed and records why on its filled entry, so the
    /// trader sees that the position the entry opened is unprotected.
    pub(super) fn reject_managed_bracket(
        &mut self,
        mut bracket: ManagedBracket,
        entry_order: &Order,
        observation: &SimulatedMarketObservation,
        reason: &str,
    ) -> Result<(), String> {
        bracket.status = ManagedBracketStatus::Cancelled;
        let sequence = self.state.next_sequence;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: entry_order.id.clone(),
            sequence,
            kind: OrderEventKind::Rejected,
            reason: Some(aeris_trading::OrderEventReason::LocalManaged),
            event_unix_nanos: observation.provenance.observed_unix_nanos,
            detail: Some(format!(
                "{} bracket protection was not placed: {reason}",
                ManagedBracket::MANAGEMENT_LABEL
            )),
            provenance: observation.provenance.clone(),
        };
        event.validate().map_err(|error| error.to_string())?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        self.state
            .managed_brackets
            .insert(bracket.bracket_id.clone(), bracket);
        self.bump_revision()
    }

    pub(super) fn prepare_initial_managed_stop(
        &mut self,
        entry_order: &Order,
        stop_price: FixedPoint,
        quantity: FixedPoint,
        observation: &SimulatedMarketObservation,
        sequence: u64,
    ) -> Result<(Order, OrderEvent, ClientOrderId), String> {
        let client_order_id =
            managed_child_client_order_id(&entry_order.client_order_id, "stop", 0)?;
        let (order, mut event) = self.prepare_single_order(
            PlaceOrder {
                client_order_id: client_order_id.clone(),
                account_id: entry_order.account_id.clone(),
                instrument_id: entry_order.instrument_id.clone(),
                side: opposite_side(entry_order.side),
                order_type: OrderType::Stop,
                time_in_force: TimeInForce::GoodTillCancelled,
                quantity,
                limit_price: None,
                stop_price: Some(stop_price),
                submitted_unix_nanos: observation.provenance.observed_unix_nanos,
                provenance: observation.provenance.clone(),
            },
            false,
            sequence,
            &[],
        )?;
        event.detail = Some(format!(
            "{} protective stop",
            ManagedBracket::MANAGEMENT_LABEL
        ));
        event.reason = Some(aeris_trading::OrderEventReason::LocalManaged);
        event.validate().map_err(|error| error.to_string())?;
        Ok((order, event, client_order_id))
    }

    /// Completes a bracket once its stop fills, or once a target fill leaves no target or no
    /// position.
    pub(super) fn complete_managed_bracket(
        &mut self,
        bracket_id: &str,
        filled_client_order_id: &ClientOrderId,
    ) -> Result<(), String> {
        let mut bracket = self
            .state
            .managed_brackets
            .get(bracket_id)
            .cloned()
            .ok_or_else(|| "managed bracket disappeared".to_string())?;
        let entry_order = self
            .order_by_client_id(&bracket.entry_client_order_id)?
            .clone();
        if bracket.stop_client_order_id.as_ref() == Some(filled_client_order_id) {
            for target in &bracket.target_client_order_ids {
                self.cancel_order(target)?;
            }
            bracket.status = ManagedBracketStatus::Completed;
        } else {
            let remaining_targets = bracket
                .target_client_order_ids
                .iter()
                .filter(|id| {
                    self.order_by_client_id(id)
                        .is_ok_and(|order| order.status.is_open())
                })
                .count();
            let position_open = self
                .state
                .positions
                .get(&(entry_order.account_id, entry_order.instrument_id))
                .is_some_and(|position| position.net_quantity.units() != 0);
            // While targets remain, the stop keeps working and position protection resizes it
            // to the smaller position.
            if remaining_targets > 0 && position_open {
                return Ok(());
            }
            if let Some(stop) = &bracket.stop_client_order_id {
                self.cancel_order_internal(stop, false)?;
            }
            bracket.status = ManagedBracketStatus::Completed;
        }
        self.state
            .managed_brackets
            .insert(bracket_id.to_string(), bracket);
        self.bump_revision()
    }

    pub(super) fn update_managed_stops(
        &mut self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let active = self
            .state
            .managed_brackets
            .values()
            .filter(|bracket| bracket.status == ManagedBracketStatus::Active)
            .cloned()
            .collect::<Vec<_>>();
        for bracket in active {
            let entry = self
                .order_by_client_id(&bracket.entry_client_order_id)?
                .clone();
            if entry.instrument_id != observation.instrument_id {
                continue;
            }
            let stop_id = bracket
                .stop_client_order_id
                .as_ref()
                .ok_or_else(|| "active managed bracket stop is missing".to_string())?;
            let stop = self.order_by_client_id(stop_id)?.clone();
            if !stop.status.is_open() {
                continue;
            }
            let entry_price = bracket
                .entry_price
                .ok_or_else(|| "active managed bracket entry price is missing".to_string())?;
            let tick_units = instrument_tick_units(instrument, entry_price)?;
            let favorable_price = match entry.side {
                OrderSide::Buy => observation.bid,
                OrderSide::Sell => observation.ask,
            };
            let favorable_ticks =
                favorable_tick_distance(entry_price, favorable_price, entry.side, tick_units)?;
            let mut next_stop = stop
                .stop_price
                .ok_or_else(|| "managed bracket stop price is missing".to_string())?;
            if let Some(rule) = bracket.template.break_even
                && favorable_ticks >= i64::from(rule.activation_ticks)
            {
                let price = shifted_price(
                    entry_price,
                    rule.offset_ticks.unsigned_abs(),
                    entry.side.sign() * i64::from(rule.offset_ticks.signum()),
                    tick_units,
                )?;
                next_stop = tighter_stop(next_stop, price, entry.side);
            }
            if let Some(rule) = bracket.template.trailing_stop
                && favorable_ticks >= i64::from(rule.activation_ticks)
            {
                let price = shifted_price(
                    favorable_price,
                    rule.distance_ticks,
                    -entry.side.sign(),
                    tick_units,
                )?;
                next_stop = tighter_stop(next_stop, price, entry.side);
            }
            if Some(next_stop) != stop.stop_price {
                self.modify_order(ModifyOrder {
                    client_order_id: stop.client_order_id,
                    time_in_force: stop.time_in_force,
                    limit_price: None,
                    stop_price: Some(next_stop),
                    modified_unix_nanos: observation.provenance.observed_unix_nanos,
                    provenance: observation.provenance.clone(),
                })?;
            }
        }
        Ok(())
    }
}
