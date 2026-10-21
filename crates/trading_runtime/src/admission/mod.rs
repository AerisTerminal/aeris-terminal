use super::{
    AccountEnvironment, BracketStrategyTemplate, BracketView, CreatePracticeAccount,
    DEMO_VENUE_UNAVAILABLE, FixedPoint, InstrumentId, LedgerEntry, LedgerEntryKind,
    MAXIMUM_OPEN_ORDERS, MAXIMUM_SNAPSHOT_ITEMS, Order, OrderEvent, OrderEventId, OrderEventKind,
    OrderId, OrderStatus, OrderStep, OrderType, PlaceOrder, PlaceProtection, ProtectionRole,
    TradeCopyDispatch, TradingAccount, TradingAccountId, TradingCore, TradingInstrument,
    VenueRequestReceiver, VenueRoute, instrument_tick_units, mirrored_client_order_id,
    multiply_quantity, order_acceptance_detail, shifted_price, venue,
};
use aeris_instruments::PriceGrid;
use aeris_trading::{OrderSide, RejectReason, RiskWarning, TimeInForce};

mod capabilities;
pub(crate) use capabilities::VenueCapabilities;

/// Explicit no-op buying-power step; margin is outside the solid-core venue.
pub(crate) struct BuyingPower;

impl BuyingPower {
    fn check(_command: &PlaceOrder) {}
}

pub(crate) fn validate_time_in_force(
    kind: OrderType,
    tif: TimeInForce,
) -> Result<(), RejectReason> {
    if matches!(kind, OrderType::Stop | OrderType::StopLimit)
        && matches!(
            tif,
            TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill
        )
    {
        return Err(RejectReason::TifNotSupported);
    }
    Ok(())
}

/// Validates exact decimal ticks without rounding provider prices or manufacturing a tick.
pub(crate) fn validate_price_grid(grid: PriceGrid, price: FixedPoint) -> Result<(), RejectReason> {
    if price.units() <= 0 {
        return Err(RejectReason::OffGrid);
    }
    match grid {
        PriceGrid::Unknown => Ok(()),
        PriceGrid::Fixed(tick) => {
            let scale = price.scale().max(tick.scale());
            let price_units =
                i128::from(price.units()) * 10_i128.pow(u32::from(scale - price.scale()));
            let tick_units =
                i128::from(tick.units()) * 10_i128.pow(u32::from(scale - tick.scale()));
            if price_units % tick_units == 0 {
                Ok(())
            } else {
                Err(RejectReason::OffGrid)
            }
        }
        PriceGrid::SignificantFigures {
            figures,
            max_decimals,
        } => {
            let mut units = price.units();
            let mut decimals = price.scale();
            while decimals > 0 && units % 10 == 0 {
                units /= 10;
                decimals -= 1;
            }
            if decimals == 0 {
                return Ok(());
            }
            let significant_digits = units.to_string().trim_start_matches('0').len();
            if decimals <= max_decimals && significant_digits <= usize::from(figures) {
                Ok(())
            } else {
                Err(RejectReason::OffGrid)
            }
        }
    }
}

pub(crate) fn validate_order_prices(
    instrument: &TradingInstrument,
    command: &PlaceOrder,
) -> Result<(), RejectReason> {
    for price in [command.limit_price, command.stop_price]
        .into_iter()
        .flatten()
    {
        validate_price_grid(instrument.contract.price_grid, price)?;
    }
    Ok(())
}

/// Price the protection relative to its entry before committing the entry. A
/// distance measured in entry ticks need not land on the instrument's price
/// grid at every resulting price (notably around significant-figure boundaries).
pub(crate) fn validate_bracket_levels(
    instrument: &TradingInstrument,
    side: OrderSide,
    entry: FixedPoint,
    template: &BracketStrategyTemplate,
) -> Result<(), RejectReason> {
    validate_price_grid(instrument.contract.price_grid, entry)?;
    let tick = instrument_tick_units(instrument, entry).map_err(|_| RejectReason::OffGrid)?;
    let stop = shifted_price(entry, template.stop_offset_ticks, -side.sign(), tick)
        .map_err(|_| RejectReason::WrongSideTrigger)?;
    let check_side = |price: FixedPoint, winning: bool| {
        let distance =
            (i128::from(price.units()) - i128::from(entry.units())) * i128::from(side.sign());
        if (winning && distance <= 0) || (!winning && distance >= 0) {
            return Err(RejectReason::WrongSideTrigger);
        }
        validate_price_grid(instrument.contract.price_grid, price)
    };
    check_side(stop, false)?;
    for target in &template.targets {
        let price = shifted_price(entry, target.offset_ticks, side.sign(), tick)
            .map_err(|_| RejectReason::WrongSideTrigger)?;
        check_side(price, true)?;
    }
    Ok(())
}

pub(crate) fn price_grid_warning(instrument: &TradingInstrument) -> Option<RiskWarning> {
    matches!(instrument.contract.price_grid, PriceGrid::Unknown)
        .then_some(RiskWarning::UnverifiedPriceGrid)
}

pub(crate) fn validate_currency(
    instrument: &TradingInstrument,
    account_currency: &str,
    route: VenueRoute,
) -> Result<(), RejectReason> {
    if route == VenueRoute::Simulated && instrument.contract.currency != account_currency {
        Err(RejectReason::CurrencyMismatch)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_trigger_side(
    command: &PlaceOrder,
    bid: FixedPoint,
    ask: FixedPoint,
) -> Result<(), RejectReason> {
    if !matches!(command.order_type, OrderType::Stop | OrderType::StopLimit) {
        return Ok(());
    }
    let Some(trigger) = command.stop_price else {
        return Ok(());
    };
    if trigger.scale() != bid.scale() || trigger.scale() != ask.scale() {
        return Err(RejectReason::WrongSideTrigger);
    }
    let wrong_side = match command.side {
        aeris_trading::OrderSide::Buy => trigger.units() <= ask.units(),
        aeris_trading::OrderSide::Sell => trigger.units() >= bid.units(),
    };
    if wrong_side {
        Err(RejectReason::WrongSideTrigger)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_owner_trigger_side(
    owner: &TradingCore,
    command: &PlaceOrder,
) -> Result<(), RejectReason> {
    if let Some(observation) = owner.quote_book.observation(&command.instrument_id) {
        validate_trigger_side(command, observation.bid, observation.ask)?;
    }
    Ok(())
}

/// Turns a chart's price-relative instruction into one durable, executable order kind.
/// An unresolved `RestingAt` never reaches the book, venue request or SQLite.
pub(crate) fn resolve_resting_at(
    mut command: PlaceOrder,
    bid: FixedPoint,
    ask: FixedPoint,
) -> Result<PlaceOrder, RejectReason> {
    if command.order_type != OrderType::RestingAt {
        return Ok(command);
    }
    let price = command.limit_price.ok_or(RejectReason::OffGrid)?;
    if price.units() <= 0 || command.stop_price.is_some() {
        return Err(RejectReason::OffGrid);
    }
    let touch = match command.side {
        OrderSide::Buy => ask,
        OrderSide::Sell => bid,
    }
    .exact_rescale(price.scale())
    .map_err(|_| RejectReason::ScaleMismatch)?;
    let beyond = match command.side {
        OrderSide::Buy => price.units() > touch.units(),
        OrderSide::Sell => price.units() < touch.units(),
    };
    command.order_type = if beyond {
        OrderType::Stop
    } else {
        OrderType::Limit
    };
    command.limit_price = (!beyond).then_some(price);
    command.stop_price = beyond.then_some(price);
    Ok(command)
}

/// The owner touch used for placement-time taker execution. Resting limit
/// candidates use their stated limit instead, not this price.
pub(crate) fn placement_touch_price(
    order: &Order,
    bid: FixedPoint,
    ask: FixedPoint,
) -> Option<FixedPoint> {
    match (order.side, order.order_type) {
        (OrderSide::Buy, OrderType::Market) => Some(ask),
        (OrderSide::Sell, OrderType::Market) => Some(bid),
        (OrderSide::Buy, OrderType::Limit) if ask.units() <= order.limit_price?.units() => {
            Some(ask)
        }
        (OrderSide::Sell, OrderType::Limit) if bid.units() >= order.limit_price?.units() => {
            Some(bid)
        }
        _ => None,
    }
}

/// The shared, side-effect-free checks run before either route persists an order.
pub(crate) fn validate_common_order(
    instrument: &TradingInstrument,
    command: &PlaceOrder,
    route: VenueRoute,
    open_orders: usize,
    staged_orders: usize,
) -> Result<(), RejectReason> {
    validate_client_id(command, route)?;
    if command.quantity.scale() != instrument.quantity_scale
        || command
            .limit_price
            .into_iter()
            .chain(command.stop_price)
            .any(|price| price.scale() != instrument.price_scale)
    {
        return Err(RejectReason::ScaleMismatch);
    }
    if command.quantity.units() <= 0
        || instrument
            .contract
            .order_quantity_increment
            .is_some_and(|increment| command.quantity.units() % increment.units() != 0)
    {
        return Err(RejectReason::InvalidQuantity);
    }
    validate_time_in_force(command.order_type, command.time_in_force)?;
    if command.order_type == OrderType::RestingAt {
        return Err(RejectReason::QuoteUnavailable);
    }
    validate_order_prices(instrument, command)?;
    if open_orders.saturating_add(staged_orders) >= MAXIMUM_OPEN_ORDERS {
        return Err(RejectReason::CapacityExceeded);
    }
    Ok(())
}

fn validate_client_id(command: &PlaceOrder, route: VenueRoute) -> Result<(), RejectReason> {
    if VenueCapabilities::for_route(route)
        .client_id_limit
        .is_some_and(|limit| command.client_order_id.as_str().len() > limit)
    {
        Err(RejectReason::ClientIdTooLong)
    } else {
        Ok(())
    }
}

/// The only order-admission boundary for simulated, broker and managed-child
/// placement. Dynamic rules may persist an account lock or advance discipline
/// state, exactly as they did before order admission was consolidated.
/// How admission evaluates account risk for one order.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum AdmissionRisk {
    /// Protective and managed exits skip rule evaluation.
    Disabled,
    /// Full rule evaluation. `attached_stop` counts a managed bracket stop or an explicit
    /// stop order as the attached protection the no-attached-stop warning looks for.
    Enforced { attached_stop: bool },
}

/// The only order-admission boundary for simulated, broker and managed-child
/// placement. Dynamic rules may persist an account lock or advance discipline
/// state, exactly as they did before order admission was consolidated.
pub(crate) fn admit(
    owner: &mut TradingCore,
    instrument: &TradingInstrument,
    command: &PlaceOrder,
    route: VenueRoute,
    additional_open_orders: usize,
    staged_instruments: &[InstrumentId],
    risk: AdmissionRisk,
) -> Result<Vec<RiskWarning>, String> {
    if risk != AdmissionRisk::Disabled && owner.state.risk_locks.contains_key(&command.account_id) {
        return Err(owner.admission_detail(command, RejectReason::AccountLocked));
    }
    validate_common_order(
        instrument,
        command,
        route,
        owner.state.orders.open_count(),
        additional_open_orders,
    )
    .map_err(|reason| owner.admission_detail(command, reason))?;
    BuyingPower::check(command);
    validate_owner_trigger_side(owner, command)
        .map_err(|reason| owner.admission_detail(command, reason))?;
    let account = owner
        .state
        .accounts
        .get(&command.account_id)
        .ok_or_else(|| owner.admission_detail(command, RejectReason::AccountNotRegistered))?;
    if route == VenueRoute::Simulated {
        owner
            .check_new_order_interest(&command.instrument_id, staged_instruments)
            .map_err(|reason| owner.admission_detail(command, reason))?;
        match owner.check_practice_market(instrument) {
            Ok(()) => {}
            Err(RejectReason::QuoteUnavailable)
                if command.order_type == OrderType::Market
                    && owner
                        .quote_book
                        .live_without_quote(&instrument.instrument_id) => {}
            Err(reason) => return Err(owner.admission_detail(command, reason)),
        }
    }
    validate_currency(instrument, &account.currency, route)
        .map_err(|reason| owner.admission_detail(command, reason))?;
    let mut warnings = match risk {
        AdmissionRisk::Enforced { attached_stop } => {
            owner.evaluate_order_risk(command, attached_stop)?.warnings
        }
        AdmissionRisk::Disabled => {
            owner.validate_managed_exit(command)?;
            Vec::new()
        }
    };
    warnings.extend(price_grid_warning(instrument));
    Ok(warnings)
}

impl TradingCore {
    pub(crate) fn resolve_order_instruction(
        &self,
        command: PlaceOrder,
    ) -> Result<PlaceOrder, RejectReason> {
        if command.order_type != OrderType::RestingAt {
            return Ok(command);
        }
        let quote = self
            .quote_book
            .observation(&command.instrument_id)
            .ok_or(RejectReason::QuoteUnavailable)?;
        let mut command = resolve_resting_at(command, quote.bid, quote.ask)?;
        command.provenance = quote.provenance;
        Ok(command)
    }

    pub(crate) fn admission_detail(&self, command: &PlaceOrder, reason: RejectReason) -> String {
        if reason == RejectReason::AccountLocked
            && let Some(lock) = self.state.risk_locks.get(&command.account_id)
        {
            return format!("{reason}: {}", lock.reason);
        }
        reason.to_string()
    }
}

#[cfg(test)]
mod grid_tests;
#[cfg(test)]
mod parity_tests;

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
        // A newly funded simulated account records its opening equity as the first ledger
        // deposit in the same commit; re-registration of a known account never redeposits.
        let deposit = if account.environment == AccountEnvironment::Simulated
            && !self.state.accounts.contains_key(&account.id)
        {
            account.starting_equity.map(|equity| LedgerEntry {
                entry_id: format!("deposit-{}", account.id.as_str()),
                account_id: account.id.clone(),
                kind: LedgerEntryKind::Deposit,
                amount: equity,
                fill_id: None,
                at_unix_nanos: self.command_now,
            })
        } else {
            None
        };
        let deposited = self
            .store
            .put_account_with_deposit(&account, deposit.as_ref())?;
        if let (true, Some(entry)) = (deposited, deposit) {
            self.state.ledger_entries.push(entry);
        }
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
            .ok_or("broker venue generation is exhausted")?;
        let (outbound, receiver) =
            aeris_trading::venue_outbox::bounded_venue_requests(venue::OUTBOUND_CAPACITY);
        // Every attach starts with a reconcile of each demo broker account, so state left
        // by a restart or a dropped session is settled against the broker first.
        let reconciles = self
            .state
            .accounts
            .values()
            .filter(|account| VenueRoute::from_account(account) == Ok(VenueRoute::BrokerDemo))
            .filter_map(|account| self.reconcile_request(account))
            .collect::<Vec<_>>();
        for request in reconciles {
            outbound
                .try_send(request)
                .map_err(|_| "broker venue reconcile requests exceed the outbound queue")?;
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
            VenueRoute::Simulated => Err("broker position requires a broker account".into()),
            VenueRoute::BrokerDemo => Ok(()),
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
        let (account_order_ids, account_client_order_ids) =
            self.practice_account_order_identity(account_id);
        self.store.delete_practice_account(account_id)?;
        self.purge_practice_account_state(
            account_id,
            &account_order_ids,
            &account_client_order_ids,
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
        (account_order_ids, account_client_order_ids)
    }

    pub(super) fn purge_practice_account_state(
        &mut self,
        account_id: &TradingAccountId,
        account_order_ids: &std::collections::BTreeSet<String>,
        account_client_order_ids: &std::collections::BTreeSet<String>,
    ) {
        self.state.accounts.remove(account_id);
        self.state.orders.remove_account(account_id);
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
            .ledger_entries
            .retain(|entry| &entry.account_id != account_id);
        self.state.fee_schedules.remove(account_id);
        self.state
            .positions
            .retain(|(stored_account, _), _| stored_account != account_id);
        self.state
            .cost_basis
            .retain(|(stored_account, _), _| stored_account != account_id);
        self.state.risk_profiles.remove(account_id);
        self.state.risk_locks.remove(account_id);
        self.state.risk_rule_states.remove(account_id);
        self.state
            .trade_cycles
            .retain(|(stored_account, _), _| stored_account != account_id);
        self.state.session_plans.remove(account_id);
        self.state.discipline_states.remove(account_id);
        self.state
            .order_groups
            .retain(|_, group| &group.account_id != account_id);
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
        let resolved = self
            .resolve_order_instruction(command.clone())
            .map_err(|reason| reason.to_string())?;
        let command = &resolved;
        match self.route(&command.account_id)? {
            VenueRoute::Simulated => {}
            VenueRoute::BrokerDemo => {
                validate_client_id(command, VenueRoute::BrokerDemo)
                    .map_err(|error| error.to_string())?;
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
        self.commit_placement_step(step, dispatches, source)
    }

    /// Compose acceptance, copied orders and any taker fills on a private projection.
    /// The same fact writer used for quote execution commits the entire step once.
    fn commit_placement_step(
        &mut self,
        step: OrderStep,
        dispatches: Vec<TradeCopyDispatch>,
        source: Order,
    ) -> Result<Order, String> {
        self.pending_equity_samples.clear();
        let candidates = step
            .inserted
            .iter()
            .filter_map(|(order, _)| {
                let quote = self.quote_book.observation(&order.instrument_id)?;
                let price = placement_touch_price(order, quote.bid, quote.ask)?;
                Some((order.id.clone(), quote, price))
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            self.commit_order_step(step, dispatches)?;
            return Ok(source);
        }
        let projection = self.state.clone();
        let original = std::mem::replace(&mut self.state, projection);
        let previous_dispatches = self.copy_dispatches.clone();
        self.quote_planning = true;
        let result = (|| {
            self.commit_order_step(step, dispatches)?;
            for (id, observation, price) in candidates {
                let instrument = self
                    .state
                    .instruments
                    .get(&observation.instrument_id)
                    .ok_or("placement instrument disappeared")?
                    .clone();
                self.validate_mark_to_market(&observation, &instrument)?;
                let fills =
                    self.execute_market_fills(vec![(id, price)], &observation, &instrument, true)?;
                self.bump_revision()?;
                self.reconcile_order_groups(&fills, &observation, &instrument)?;
                self.align_position_protection(&fills, &observation)?;
                self.update_mark_to_market(&observation, &instrument)?;
                self.evaluate_live_risk(observation.provenance.observed_unix_nanos)?;
            }
            Ok::<_, String>(())
        })();
        self.quote_planning = false;
        let planned = std::mem::replace(&mut self.state, original);
        if let Err(error) = result {
            self.pending_equity_samples.clear();
            self.copy_dispatches = previous_dispatches;
            return Err(error);
        }
        if let Err(error) =
            self.store
                .commit_quote_step(&self.state, &planned, &self.pending_equity_samples)
        {
            self.pending_equity_samples.clear();
            self.copy_dispatches = previous_dispatches;
            return Err(error);
        }
        self.pending_equity_samples.clear();
        self.state = planned;
        self.enforce_memory_retention();
        self.state
            .orders
            .get(&source.id)
            .cloned()
            .ok_or_else(|| "placed order disappeared".to_string())
    }

    pub(super) fn stage_single_order(
        &mut self,
        step: &mut OrderStep,
        command: PlaceOrder,
        enforce_risk: bool,
    ) -> Result<Order, String> {
        let attached_stop = command.stop_price.is_some();
        let (order, event) = self.prepare_single_order(
            command,
            enforce_risk,
            attached_stop,
            step.next_sequence,
            &step
                .inserted
                .iter()
                .map(|(order, _)| order.instrument_id.clone())
                .collect::<Vec<_>>(),
        )?;
        step.next_sequence += 1;
        step.inserted.push((order.clone(), event));
        if matches!(
            order.time_in_force,
            TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill
        ) && order.order_type == OrderType::Limit
            && let Some(quote) = self.quote_book.observation(&order.instrument_id)
            && placement_touch_price(&order, quote.bid, quote.ask).is_none()
        {
            let (cancelled, reason) = crate::book::lifecycle::cancel_with_cause(
                order,
                crate::book::lifecycle::CancellationCause::NotMarketable,
                self.command_now,
            )
            .map_err(|error| error.to_string())?;
            let sequence = step.next_sequence;
            let event = OrderEvent {
                id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: cancelled.id.clone(),
                sequence,
                kind: OrderEventKind::Cancelled,
                reason: Some(reason),
                event_unix_nanos: self.command_now,
                detail: Some("not marketable at placement".to_string()),
                provenance: quote.provenance,
            };
            event.validate().map_err(|error| error.to_string())?;
            step.next_sequence += 1;
            step.updated.push((cancelled.clone(), event));
            return Ok(cancelled);
        }
        Ok(order)
    }

    pub(super) fn place_protective_order(
        &mut self,
        command: PlaceProtection,
    ) -> Result<Order, String> {
        if self.route(&command.order.account_id)? != VenueRoute::Simulated {
            return Err("managed protective orders are unavailable for broker accounts".into());
        }
        if let Some(existing) = self.state.orders.by_client(&command.order.client_order_id) {
            let existing = existing_client_order(existing, &command.order)
                .map_err(|error| error.to_string())?;
            let role_matches = self
                .protection_leg_role(&command.order.client_order_id)
                .is_some_and(|role| role == command.role);
            return role_matches.then_some(existing).ok_or_else(|| {
                "protective client identifier has conflicting semantics".to_string()
            });
        }
        let shape_matches = matches!(
            (command.role, command.order.order_type),
            (ProtectionRole::StopLoss, OrderType::Stop)
                | (ProtectionRole::TakeProfit, OrderType::Limit)
        );
        if !shape_matches {
            return Err("protective order role does not match its order type".to_string());
        }
        let sequence = self.state.next_sequence;
        let (order, mut event) =
            self.prepare_single_order(command.order, false, false, sequence, &[])?;
        event.detail = Some(format!(
            "{} standalone {}",
            BracketView::MANAGEMENT_LABEL,
            command.role.as_str()
        ));
        event.reason = Some(aeris_trading::OrderEventReason::LocalManaged);
        event.validate().map_err(|error| error.to_string())?;
        // Standalone protection is a one-leg group, the same shape the V23 migration gave
        // historical protective orders.
        let group = crate::core::OrderGroup {
            group_id: format!("protection/{}", order.id.as_str()),
            account_id: order.account_id.clone(),
            instrument_id: order.instrument_id.clone(),
            kind: crate::core::GroupKind::Protection,
            status: crate::core::GroupStatus::Active,
            entry: None,
            legs: vec![crate::core::GroupLeg {
                order_id: order.id.clone(),
                role: match command.role {
                    ProtectionRole::StopLoss => crate::core::LegRole::StopLoss,
                    ProtectionRole::TakeProfit => crate::core::LegRole::TakeProfit { index: 0 },
                },
                offset_ticks: None,
                allocation_percent: None,
            }],
            management: crate::core::StopManagement::default(),
            template: None,
            revision: 1,
        };
        group
            .validate()
            .map_err(|error| format!("invalid order group: {error:?}"))?;
        let mut step = OrderStep::new(sequence + 1);
        step.inserted.push((order.clone(), event));
        step.groups.push(group);
        self.commit_order_step(step, Vec::new())?;
        Ok(order)
    }

    pub(super) fn prepare_single_order(
        &mut self,
        mut command: PlaceOrder,
        enforce_risk: bool,
        attached_stop: bool,
        sequence: u64,
        staged_instruments: &[InstrumentId],
    ) -> Result<(Order, OrderEvent), String> {
        let account = self
            .state
            .accounts
            .get(&command.account_id)
            .ok_or_else(|| "trading account is not registered".to_string())?;
        match VenueRoute::from_account(account)? {
            VenueRoute::Simulated => {}
            VenueRoute::BrokerDemo => return Err(DEMO_VENUE_UNAVAILABLE.into()),
        }
        command.provenance.venue_id.clone_from(&account.venue_id);
        let instrument = self
            .state
            .instruments
            .get(&command.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?
            .clone();
        let risk = if enforce_risk {
            AdmissionRisk::Enforced { attached_stop }
        } else {
            AdmissionRisk::Disabled
        };
        let risk_warnings = admit(
            self,
            &instrument,
            &command,
            VenueRoute::Simulated,
            staged_instruments.len(),
            staged_instruments,
            risk,
        )?;
        let expires_at_unix_nanos = if command.time_in_force == TimeInForce::Day {
            // Legacy replay fixtures do not install a routed market calendar. Real
            // admission always requires the owner session supplied by the feed.
            #[cfg(test)]
            let legacy_deadline = if self
                .state
                .market_routes
                .contains_key(&command.instrument_id)
            {
                None
            } else {
                crate::market::feed::next_utc_midnight(self.command_now)
            };
            #[cfg(not(test))]
            let legacy_deadline: Option<i64> = None;
            Some(
                legacy_deadline
                    .map_or_else(
                        || {
                            self.quote_book
                                .day_deadline(&command.instrument_id, self.command_now)
                        },
                        Ok,
                    )
                    .map_err(|reason| reason.to_string())?,
            )
        } else {
            None
        };
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
            revision: 0,
            accepted_unix_nanos: 0,
            updated_unix_nanos: 0,
            eligible_after_unix_nanos: 0,
            triggered_unix_nanos: None,
            expires_at_unix_nanos,
            terminal_unix_nanos: None,
            provenance: command.provenance.clone(),
        };
        let order = crate::book::lifecycle::accept(order, self.command_now)
            .map_err(|error| error.to_string())?;
        order.validate().map_err(|error| error.to_string())?;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Accepted,
            reason: (!risk_warnings.is_empty())
                .then_some(aeris_trading::OrderEventReason::RiskWarning),
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
