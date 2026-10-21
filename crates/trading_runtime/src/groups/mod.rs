pub mod model;
pub(super) mod oco;
pub(super) mod owned;
pub(super) mod recovery;

use super::{
    BROKER_PNL_UNAVAILABLE, BracketStrategyTemplate, BracketTarget, BracketView, BracketViewStatus,
    ClientOrderId, Fill, FixedPoint, InstrumentId, MAXIMUM_STRATEGY_TEMPLATES, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderSide, OrderStatus, OrderStep, OrderType, PlaceBracket,
    PlaceChartBracket, PlaceInlineBracket, PlaceOco, PlaceOrder, PlacementOutcome, ProtectionRole,
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

/// When a protective order triggers relative to others of its role, and at what price. A
/// lower key triggers first: a sell protects a long position, so its highest stop and its
/// lowest target trigger first, and a buy protecting a short is the mirror image.
pub(crate) fn protection_trigger(order: &Order, role: ProtectionRole) -> Option<(i64, FixedPoint)> {
    let protects_long = order.side == OrderSide::Sell;
    let (price, highest_first) = match role {
        ProtectionRole::StopLoss => (order.stop_price.or(order.limit_price), protects_long),
        ProtectionRole::TakeProfit => (order.limit_price.or(order.stop_price), !protects_long),
    };
    let price = price?;
    let key = if highest_first {
        price.units().saturating_neg()
    } else {
        price.units()
    };
    Some((key, price))
}

/// The protective role a bracket view gives an order, if the order is its stop or a target.
pub(crate) fn managed_bracket_role(
    bracket: &BracketView,
    client_order_id: &ClientOrderId,
) -> Option<ProtectionRole> {
    if bracket.stop_client_order_id.as_ref() == Some(client_order_id) {
        Some(ProtectionRole::StopLoss)
    } else if bracket.target_client_order_ids.contains(client_order_id) {
        Some(ProtectionRole::TakeProfit)
    } else {
        None
    }
}

impl TradingCore {
    pub(super) fn place_inline_bracket_from_market(
        &mut self,
        bracket: PlaceInlineBracket,
    ) -> Result<PlacementOutcome, String> {
        let entry = self.market_resting_bracket_entry(bracket.entry)?;
        self.place_bracket_with_template(&entry, &bracket.template)
    }

    pub(super) fn place_chart_bracket(
        &mut self,
        bracket: PlaceChartBracket,
    ) -> Result<PlacementOutcome, String> {
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
    ) -> Result<PlacementOutcome, String> {
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
    ) -> Result<PlacementOutcome, String> {
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
                && !crate::core::admission::VenueCapabilities::for_route(VenueRoute::BrokerDemo)
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
                .map(PlacementOutcome::BrokerProtected);
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
        // Simulated brackets are owned only by the group owner.
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
}
