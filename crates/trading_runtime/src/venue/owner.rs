//! Broker order state is persisted and advanced only by the trading owner.

use super::{VenueEvent, VenueRequest, VenueUpdate};
use crate::BracketStrategyTemplate;
use crate::{
    Coordinator, DEMO_VENUE_UNAVAILABLE, FlattenOutcome, MAXIMUM_OPEN_ORDERS, ModifyOrder,
    PlaceOrder, TradingProvenance, check_quantity_increment, current_unix_nanos,
    store::BrokerFillRecord,
};
use aeris_trading::{
    AccountEnvironment, BrokerPosition, ClientOrderId, Fill, FillId, FixedPoint, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, TimeInForce,
    TradingAccount, TradingAccountId,
    venue::{
        BrokerFill, BrokerOrder, BrokerOrderState, ObservedAccount, Protection, RealizedClose,
        VenueAmendment, VenueOrder, VenuePosition, VenueSnapshot,
    },
};
use std::{collections::BTreeSet, sync::mpsc::TrySendError};

const NOT_ACKNOWLEDGED: &str = "cTrader has not acknowledged this order yet; try again shortly";

impl Coordinator {
    /// The broker account behind a registered broker-routed trading account.
    fn broker_account(&self, account_id: &TradingAccountId) -> Result<String, String> {
        self.state
            .accounts
            .get(account_id)
            .and_then(|account| account.broker_ref.clone())
            .ok_or_else(|| "trading account has no broker account".into())
    }

    /// The trading account a venue event's broker account belongs to.
    fn venue_account(&self, broker_account: &str) -> Option<TradingAccountId> {
        self.state
            .accounts
            .values()
            .find(|account| {
                account.venue_id == "ctrader"
                    && account.broker_ref.as_deref() == Some(broker_account)
            })
            .map(|account| account.id.clone())
    }

    /// A reconcile for one broker account. Deals are replayed from the earlier of its
    /// oldest open order and its latest recorded deal, so anything that executed while the
    /// owner was not listening is applied before the snapshot settles open orders.
    pub(crate) fn reconcile_request(&self, account: &TradingAccount) -> Option<VenueRequest> {
        let broker_account = account.broker_ref.clone()?;
        let oldest_open = self
            .state
            .orders
            .values()
            .filter(|order| order.account_id == account.id && order.status.is_open())
            .map(|order| order.submitted_unix_nanos)
            .min();
        let latest_deal = self
            .state
            .fills
            .iter()
            .filter(|fill| fill.account_id == account.id)
            .map(|fill| fill.execution_unix_nanos)
            .max();
        let deals_since_unix_nanos = match (oldest_open, latest_deal) {
            (Some(open), Some(deal)) => Some(open.min(deal)),
            (open, deal) => open.or(deal),
        };
        Some(VenueRequest::Reconcile {
            broker_account,
            deals_since_unix_nanos,
        })
    }

    /// Registers a broker account the relay can reach, under a stable id derived from the
    /// broker account. A known account keeps its id and money scale; only its name follows
    /// the broker. A newly registered demo account is reconciled at once.
    fn register_observed_account(
        &mut self,
        broker_account: &str,
        observed: &ObservedAccount,
    ) -> Result<(), String> {
        if let Some(account_id) = self.venue_account(broker_account) {
            let mut known = self
                .state
                .accounts
                .get(&account_id)
                .cloned()
                .ok_or("trading account is not registered")?;
            if known.environment != observed.environment {
                return Err("a broker account changed environment".into());
            }
            if known.display_name == observed.display_name {
                return Ok(());
            }
            known.display_name.clone_from(&observed.display_name);
            return self.register_account(known);
        }
        let account = TradingAccount {
            id: TradingAccountId::try_new(format!(
                "ctrader-{}-{broker_account}",
                observed.environment.as_str()
            ))
            .map_err(|error| error.to_string())?,
            display_name: observed.display_name.clone(),
            environment: observed.environment,
            venue_id: "ctrader".into(),
            broker_ref: Some(broker_account.to_string()),
            currency: observed.currency.clone(),
            currency_scale: observed.currency_scale,
            starting_equity: None,
        };
        let reconcile = (account.environment == AccountEnvironment::Demo)
            .then(|| self.reconcile_request(&account))
            .flatten();
        self.register_account(account)?;
        match reconcile {
            Some(request) if self.venue_outbound.is_some() => self.send_venue_request(request),
            _ => Ok(()),
        }
    }

    fn broker_order_id(&self, order_id: &OrderId) -> Option<String> {
        self.state
            .broker_order_ids
            .iter()
            .find(|(_, owner)| *owner == order_id)
            .map(|(broker, _)| broker.clone())
    }

    pub(crate) fn place_broker_order(&mut self, command: &PlaceOrder) -> Result<Order, String> {
        self.place_protected_broker_order(command, None, None)
    }

    /// A bracket on a broker account is one entry order carrying server-side protection:
    /// the stop at the template's stop distance and the take-profit at its single target.
    /// cTrader holds one take-profit per order, so scale-out targets are refused.
    pub(crate) fn place_broker_bracket(
        &mut self,
        command: &PlaceOrder,
        template: &BracketStrategyTemplate,
    ) -> Result<Order, String> {
        let take_profit_ticks = match template.targets.as_slice() {
            [] => None,
            [target] if target.quantity_percent == 100 => Some(target.offset_ticks),
            _ => {
                return Err(
                    "cTrader holds one take-profit per order; scale-out targets are unavailable \
                     for broker accounts"
                        .into(),
                );
            }
        };
        let instrument = self
            .state
            .instruments
            .get(&command.instrument_id)
            .ok_or("trading instrument is not registered")?;
        let tick = instrument
            .contract
            .tick_size
            .ok_or("the instrument has no tick size")
            .and_then(|tick| {
                FixedPoint::try_new(tick.units(), tick.scale())
                    .and_then(|tick| tick.exact_rescale(instrument.price_scale))
                    .map_err(|_| "the instrument tick size does not fit its price scale")
            })?;
        let distance = |ticks: u32| {
            tick.units()
                .checked_mul(i64::from(ticks))
                .ok_or_else(|| "bracket distance overflowed".to_string())
                .and_then(|units| {
                    FixedPoint::try_new(units, tick.scale()).map_err(|error| error.to_string())
                })
                .map(Protection::Distance)
        };
        let stop_loss = distance(template.stop_offset_ticks)?;
        let take_profit = take_profit_ticks.map(distance).transpose()?;
        self.place_protected_broker_order(command, Some(stop_loss), take_profit)
    }

    fn place_protected_broker_order(
        &mut self,
        command: &PlaceOrder,
        stop_loss: Option<Protection>,
        take_profit: Option<Protection>,
    ) -> Result<Order, String> {
        if self.venue_outbound.is_none() {
            return Err(DEMO_VENUE_UNAVAILABLE.into());
        }
        if command.client_order_id.as_str().len() > 50 {
            return Err("cTrader ClientOrderId must be at most 50 characters".into());
        }
        let instrument = self
            .state
            .instruments
            .get(&command.instrument_id)
            .ok_or("trading instrument is not registered")?;
        if command.quantity.scale() != instrument.quantity_scale
            || command
                .limit_price
                .into_iter()
                .chain(command.stop_price)
                .any(|price| price.scale() != instrument.price_scale)
        {
            return Err("order scales do not match the instrument".into());
        }
        check_quantity_increment(instrument, command.quantity)?;
        if self
            .state
            .orders
            .values()
            .filter(|order| order.status.is_open())
            .count()
            >= MAXIMUM_OPEN_ORDERS
        {
            return Err("trading open-order limit reached".into());
        }
        // Every M3.2 check applies to broker orders. The practice rule that the contract
        // currency equal the account currency does not: the broker converts profit and loss
        // into the deposit currency.
        self.evaluate_order_risk(command)?;
        let time_in_force = broker_time_in_force(command.order_type, command.time_in_force);
        let request = VenueRequest::Place(VenueOrder {
            client_order_id: command.client_order_id.clone(),
            broker_account: self.broker_account(&command.account_id)?,
            instrument_id: command.instrument_id.clone(),
            side: command.side,
            order_type: command.order_type,
            time_in_force,
            quantity: command.quantity,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            stop_loss,
            take_profit,
            broker_position_id: None,
        });
        request.validate().map_err(|error| error.to_string())?;
        // Validate and persist before making a request visible to the relay.
        let sequence = self.state.next_sequence;
        let provenance = self.broker_provenance(current_unix_nanos()?, sequence);
        let order = Order {
            id: OrderId::try_new(format!("ct-order-{sequence}"))
                .map_err(|error| error.to_string())?,
            client_order_id: command.client_order_id.clone(),
            account_id: command.account_id.clone(),
            instrument_id: command.instrument_id.clone(),
            side: command.side,
            order_type: command.order_type,
            time_in_force,
            quantity: command.quantity,
            filled_quantity: FixedPoint::try_new(0, command.quantity.scale())
                .map_err(|error| error.to_string())?,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            status: OrderStatus::Pending,
            submitted_unix_nanos: command.submitted_unix_nanos,
            provenance: provenance.clone(),
        };
        order.validate().map_err(|error| error.to_string())?;
        let event = Self::broker_event(
            &order,
            sequence,
            OrderEventKind::Accepted,
            "submitted to cTrader",
            provenance,
        )?;
        self.store
            .insert_broker_order(&order, &event, sequence + 1)?;
        let outbound = self.venue_outbound.as_ref().ok_or(DEMO_VENUE_UNAVAILABLE)?;
        match outbound.try_send(request) {
            Ok(()) => {
                self.state.next_sequence = sequence + 1;
                self.state.orders.insert(order.id.clone(), order.clone());
                self.state.order_events.push_back(event);
                self.bump_revision()?;
                Ok(order)
            }
            Err(error) => {
                self.store.reject_unsent_broker_order(&order, sequence)?;
                Err(outbound_error(&error))
            }
        }
    }

    /// Requests a price change. The order keeps its current prices until the broker
    /// reports the replacement, so a reconnect never applies an unconfirmed change.
    pub(crate) fn modify_broker_order(
        &mut self,
        order: Order,
        command: &ModifyOrder,
    ) -> Result<Order, String> {
        if !matches!(
            order.status,
            OrderStatus::Working | OrderStatus::PartiallyFilled
        ) {
            return Err("only working orders can be modified".into());
        }
        if command.modified_unix_nanos <= 0 {
            return Err("order modification timestamp must be positive".into());
        }
        if broker_time_in_force(order.order_type, command.time_in_force) != order.time_in_force {
            return Err("cTrader cannot change an order's time in force".into());
        }
        let instrument = self
            .state
            .instruments
            .get(&order.instrument_id)
            .ok_or("trading instrument is not registered")?;
        if command
            .limit_price
            .into_iter()
            .chain(command.stop_price)
            .any(|price| price.scale() != instrument.price_scale)
        {
            return Err("modified order scales do not match the instrument".into());
        }
        let candidate = Order {
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            ..order.clone()
        };
        candidate.validate().map_err(|error| error.to_string())?;
        let broker_order_id = self.broker_order_id(&order.id).ok_or(NOT_ACKNOWLEDGED)?;
        let request = VenueRequest::Amend(VenueAmendment {
            client_order_id: order.client_order_id.clone(),
            broker_account: self.broker_account(&order.account_id)?,
            broker_order_id,
            instrument_id: order.instrument_id.clone(),
            quantity: None,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            stop_loss: None,
            take_profit: None,
        });
        let pending = Order {
            status: OrderStatus::PendingModify,
            ..order
        };
        self.queue_broker_transition(
            &pending,
            OrderEventKind::Modified,
            "modify requested",
            request,
        )?;
        Ok(pending)
    }

    pub(crate) fn cancel_broker_order(&mut self, order: Order) -> Result<Order, String> {
        if !order.status.is_open() {
            return Ok(order);
        }
        if matches!(
            order.status,
            OrderStatus::PendingModify | OrderStatus::PendingCancel
        ) {
            return Err("broker order already has a pending request".into());
        }
        let broker_order_id = self.broker_order_id(&order.id).ok_or(NOT_ACKNOWLEDGED)?;
        let request = VenueRequest::Cancel {
            client_order_id: order.client_order_id.clone(),
            broker_account: self.broker_account(&order.account_id)?,
            broker_order_id,
        };
        let pending = Order {
            status: OrderStatus::PendingCancel,
            ..order
        };
        self.queue_broker_transition(
            &pending,
            OrderEventKind::Cancelled,
            "cancel requested",
            request,
        )?;
        Ok(pending)
    }

    fn broker_position_state(
        &self,
        account_id: &TradingAccountId,
        broker_position_id: &str,
    ) -> Result<BrokerPosition, String> {
        if broker_position_id.trim().is_empty() {
            return Err("broker position identifier must not be empty".into());
        }
        self.state
            .broker_positions
            .get(&(account_id.clone(), broker_position_id.to_string()))
            .cloned()
            .ok_or_else(|| "broker position is not open".into())
    }

    fn send_venue_request(&self, request: VenueRequest) -> Result<(), String> {
        request.validate().map_err(|error| error.to_string())?;
        self.venue_outbound
            .as_ref()
            .ok_or(DEMO_VENUE_UNAVAILABLE)?
            .try_send(request)
            .map_err(|error| outbound_error(&error))
    }

    /// Requests closing one whole broker position. The close is asynchronous: the
    /// position stays until the broker reports it closed.
    pub(crate) fn close_broker_position(
        &self,
        account_id: &TradingAccountId,
        broker_position_id: &str,
    ) -> Result<(), String> {
        let position = self.broker_position_state(account_id, broker_position_id)?;
        self.send_venue_request(VenueRequest::ClosePosition {
            broker_account: self.broker_account(account_id)?,
            instrument_id: position.instrument_id,
            broker_position_id: position.broker_position_id,
            quantity: position.quantity,
        })
    }

    /// Requests a position's complete server-side protection; a level left `None` is
    /// removed, as the broker does.
    pub(crate) fn amend_broker_position_protection(
        &self,
        account_id: &TradingAccountId,
        broker_position_id: &str,
        stop_loss: Option<FixedPoint>,
        take_profit: Option<FixedPoint>,
    ) -> Result<(), String> {
        if stop_loss.is_none() && take_profit.is_none() {
            return Err("stop-loss or take-profit is required".into());
        }
        let position = self.broker_position_state(account_id, broker_position_id)?;
        let price_scale = self
            .state
            .instruments
            .get(&position.instrument_id)
            .ok_or("trading instrument is not registered")?
            .price_scale;
        if stop_loss
            .into_iter()
            .chain(take_profit)
            .any(|price| price.scale() != price_scale)
        {
            return Err("protection prices do not match the instrument scale".into());
        }
        self.send_venue_request(VenueRequest::AmendPositionProtection {
            broker_account: self.broker_account(account_id)?,
            instrument_id: position.instrument_id,
            broker_position_id: position.broker_position_id,
            stop_loss,
            take_profit,
        })
    }

    /// Requests closing every broker position of `account_id` on one instrument and
    /// returns the positions whose closes were submitted.
    pub(crate) fn close_broker_positions_on(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &aeris_instruments::InstrumentId,
    ) -> Result<Vec<String>, String> {
        let positions: Vec<String> = self
            .state
            .broker_positions
            .values()
            .filter(|position| {
                position.account_id == *account_id && position.instrument_id == *instrument_id
            })
            .map(|position| position.broker_position_id.clone())
            .collect();
        for broker_position_id in &positions {
            self.close_broker_position(account_id, broker_position_id)?;
        }
        Ok(positions)
    }

    /// Flattens one broker account on one instrument: cancels its open orders and
    /// requests closing its positions there. Closes are attempted even when a cancel
    /// cannot be requested; what failed is reported in the outcome.
    pub(crate) fn flatten_broker_account(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &aeris_instruments::InstrumentId,
    ) -> Result<FlattenOutcome, String> {
        let (_, incomplete) = self.cancel_open_orders(Some(account_id))?;
        let pending_close_requests = self.close_broker_positions_on(account_id, instrument_id)?;
        Ok(FlattenOutcome {
            fills: Vec::new(),
            pending_close_requests,
            incomplete,
        })
    }

    /// Reverses the account's net broker exposure on one instrument: closes its positions
    /// and places one risk-checked market order for the opposite net quantity.
    pub(crate) fn reverse_broker_position(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &aeris_instruments::InstrumentId,
        provenance: &TradingProvenance,
    ) -> Result<FlattenOutcome, String> {
        let mut net = 0_i64;
        let mut scale = None;
        for position in self.state.broker_positions.values().filter(|position| {
            position.account_id == *account_id && position.instrument_id == *instrument_id
        }) {
            net = net
                .checked_add(position.side.sign() * position.quantity.units())
                .ok_or("reverse quantity overflowed")?;
            scale = Some(position.quantity.scale());
        }
        let (Some(scale), false) = (scale, net == 0) else {
            return Err("reverse requires an open position".into());
        };
        let side = if net > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let quantity = FixedPoint::try_new(
            i64::try_from(net.unsigned_abs()).map_err(|_| "reverse quantity overflowed")?,
            scale,
        )
        .map_err(|error| error.to_string())?;
        let outcome = self.flatten_broker_account(account_id, instrument_id)?;
        let order = PlaceOrder {
            client_order_id: ClientOrderId::try_new(format!(
                "ct-reverse-{}",
                self.state.next_sequence
            ))
            .map_err(|error| error.to_string())?,
            account_id: account_id.clone(),
            instrument_id: instrument_id.clone(),
            side,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            quantity,
            limit_price: None,
            stop_price: None,
            submitted_unix_nanos: provenance.observed_unix_nanos,
            provenance: provenance.clone(),
        };
        self.place_broker_order(&order)?;
        Ok(outcome)
    }

    pub(crate) fn apply_venue_event(&mut self, event: &VenueEvent) -> Result<(), String> {
        let generation = self.state.venue_generation;
        if event.session_generation != generation || generation == 0 {
            return Ok(());
        }
        self.venue_ingestion = self
            .venue_ingestion
            .checked_add(1)
            .ok_or("venue ingestion sequence exhausted")?;
        if let VenueUpdate::AccountObserved(observed) = &event.update {
            return self.register_observed_account(&event.broker_account, observed);
        }
        let Some(account_id) = self.venue_account(&event.broker_account) else {
            return Ok(());
        };
        match &event.update {
            VenueUpdate::AccountObserved(_) => Ok(()),
            VenueUpdate::Order {
                order,
                state,
                reason,
            } => self.apply_broker_order(&account_id, order, *state, reason.as_deref(), false),
            VenueUpdate::Refused {
                client_order_id,
                reason,
            } => self.apply_refusal(&account_id, client_order_id, reason),
            VenueUpdate::Fill(fill) => self.apply_broker_fill(&account_id, fill),
            VenueUpdate::Position(position) => {
                let position = self.broker_position(&account_id, position)?;
                self.write_positions(&account_id, position.into_iter().collect(), &[], false)
            }
            VenueUpdate::PositionClosed { broker_position_id } => self.write_positions(
                &account_id,
                Vec::new(),
                std::slice::from_ref(broker_position_id),
                false,
            ),
            VenueUpdate::Balance { balance } => {
                self.store
                    .set_broker_balance(&account_id, *balance, generation)?;
                self.state.broker_balances.insert(account_id, *balance);
                self.bump_revision()
            }
            VenueUpdate::Snapshot(snapshot) => self.apply_snapshot(&account_id, snapshot),
        }
    }

    /// Records one broker deal once. Our order's filled quantity is the larger of what
    /// the broker reported and what its deals add up to, so replays never double-count.
    fn apply_broker_fill(
        &mut self,
        account_id: &TradingAccountId,
        report: &BrokerFill,
    ) -> Result<(), String> {
        if self.store.broker_deal_recorded(&report.broker_deal_id)? {
            return Ok(());
        }
        if !self.state.instruments.contains_key(&report.instrument_id) {
            return Err("broker fill names an unregistered instrument".into());
        }
        let provenance = self.venue_provenance()?;
        let sequence = self.state.next_sequence;
        let bound = self
            .state
            .broker_order_ids
            .get(&report.broker_order_id)
            .and_then(|order_id| self.state.orders.get(order_id))
            .filter(|order| order.account_id == *account_id)
            .cloned();
        let (current, mirror) = match bound {
            Some(order) => (order, None),
            None => (
                Self::mirror_order(account_id, report, sequence, &provenance)?,
                Some(()),
            ),
        };
        if report.quantity.scale() != current.quantity.scale() {
            return Err("broker fill quantity scale does not match the order".into());
        }
        let fill = Fill {
            id: FillId::try_new(format!("ct-fill-{}", report.broker_deal_id))
                .map_err(|error| error.to_string())?,
            order_id: current.id.clone(),
            account_id: account_id.clone(),
            instrument_id: report.instrument_id.clone(),
            side: report.side,
            price: report.price,
            quantity: report.quantity,
            execution_unix_nanos: report.executed_unix_nanos,
            provenance: provenance.clone(),
        };
        fill.validate().map_err(|error| error.to_string())?;
        let filled_by_deals = self
            .state
            .fills
            .iter()
            .filter(|known| known.order_id == current.id)
            .try_fold(report.quantity, |total, known| {
                total.checked_add(known.quantity)
            })
            .map_err(|error| error.to_string())?;
        let updated = filled_order(&current, filled_by_deals, mirror.is_some(), provenance)?;
        let event = Self::broker_event(
            &updated,
            sequence,
            OrderEventKind::Filled,
            "filled at cTrader",
            updated.provenance.clone(),
        )?;
        let realized_pnl = report
            .realized
            .map(|realized| self.realized_in_account_currency(account_id, &realized))
            .transpose()?;
        self.store.insert_broker_fill(
            &BrokerFillRecord {
                fill: &fill,
                order: &updated,
                event: &event,
                mirror: mirror.map(|()| (&current, report.broker_order_id.as_str())),
                broker_deal_id: &report.broker_deal_id,
                broker_position_id: &report.broker_position_id,
                realized_pnl,
            },
            sequence + 1,
        )?;
        if mirror.is_some() {
            self.state
                .broker_order_ids
                .insert(report.broker_order_id.clone(), current.id.clone());
        }
        if let Some(pnl) = realized_pnl {
            self.state.fill_realized_pnl.insert(fill.id.clone(), pnl);
        }
        self.state.next_sequence = sequence + 1;
        self.state.orders.insert(updated.id.clone(), updated);
        self.state.order_events.push_back(event);
        self.state.fills.push_back(fill);
        self.bump_revision()
    }

    /// A broker-originated order (a server stop-loss or take-profit, a close request, or
    /// an order placed outside Aeris) first seen through one of its deals. Its client id is
    /// namespaced by the broker order id, because brokers reuse request ids across sessions.
    fn mirror_order(
        account_id: &TradingAccountId,
        report: &BrokerFill,
        sequence: u64,
        provenance: &TradingProvenance,
    ) -> Result<Order, String> {
        let order = Order {
            id: OrderId::try_new(format!("ct-order-{sequence}"))
                .map_err(|error| error.to_string())?,
            client_order_id: ClientOrderId::try_new(format!("ct-{}", report.broker_order_id))
                .map_err(|error| error.to_string())?,
            account_id: account_id.clone(),
            instrument_id: report.instrument_id.clone(),
            side: report.side,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            quantity: report.quantity,
            filled_quantity: FixedPoint::try_new(0, report.quantity.scale())
                .map_err(|error| error.to_string())?,
            limit_price: None,
            stop_price: None,
            status: OrderStatus::Working,
            submitted_unix_nanos: report.executed_unix_nanos,
            provenance: provenance.clone(),
        };
        order.validate().map_err(|error| error.to_string())?;
        Ok(order)
    }

    /// Net realized amount of a closing deal, exactly in the account currency scale.
    fn realized_in_account_currency(
        &self,
        account_id: &TradingAccountId,
        realized: &RealizedClose,
    ) -> Result<FixedPoint, String> {
        let scale = self
            .state
            .accounts
            .get(account_id)
            .ok_or("trading account is not registered")?
            .currency_scale;
        [realized.gross_profit, realized.swap, realized.commission]
            .into_iter()
            .try_fold(
                FixedPoint::try_new(0, scale).map_err(|error| error.to_string())?,
                |total, amount| {
                    amount
                        .exact_rescale(scale)
                        .and_then(|amount| total.checked_add(amount))
                        .map_err(|error| error.to_string())
                },
            )
    }

    fn broker_position(
        &self,
        account_id: &TradingAccountId,
        report: &VenuePosition,
    ) -> Result<Option<BrokerPosition>, String> {
        // A position without fills is a pending order's placeholder, not exposure.
        let Some(entry_price) = report.entry_price else {
            return Ok(None);
        };
        let scale = self
            .state
            .accounts
            .get(account_id)
            .ok_or("trading account is not registered")?
            .currency_scale;
        let zero = FixedPoint::try_new(0, scale).map_err(|error| error.to_string())?;
        let position = BrokerPosition {
            account_id: account_id.clone(),
            instrument_id: report.instrument_id.clone(),
            broker_position_id: report.broker_position_id.clone(),
            side: report.side,
            quantity: report.quantity,
            entry_price,
            stop_loss: report.stop_loss,
            take_profit: report.take_profit,
            swap: report
                .swap
                .exact_rescale(scale)
                .map_err(|error| error.to_string())?,
            commission: report
                .commission
                .exact_rescale(scale)
                .map_err(|error| error.to_string())?,
            // The broker reports no mark; unrealized P&L is projected from quotes.
            gross_unrealized: zero,
            net_unrealized: zero,
            opened_unix_nanos: report.opened_unix_nanos,
        };
        position.validate().map_err(|error| error.to_string())?;
        Ok(Some(position))
    }

    fn write_positions(
        &mut self,
        account_id: &TradingAccountId,
        upserts: Vec<BrokerPosition>,
        removals: &[String],
        replace_all: bool,
    ) -> Result<(), String> {
        self.store
            .write_broker_positions(account_id, &upserts, removals, replace_all)?;
        if replace_all {
            self.state
                .broker_positions
                .retain(|(owner, _), _| owner != account_id);
        }
        for broker_position_id in removals {
            self.state
                .broker_positions
                .remove(&(account_id.clone(), broker_position_id.clone()));
        }
        for position in upserts {
            self.state.broker_positions.insert(
                (account_id.clone(), position.broker_position_id.clone()),
                position,
            );
        }
        self.bump_revision()
    }

    /// Makes the owner agree with the broker after a (re)attach. Positions are replaced.
    /// Open orders still at the broker are re-confirmed, which also resolves requests the
    /// broker never answered; open orders it no longer holds are settled from their deals.
    fn apply_snapshot(
        &mut self,
        account_id: &TradingAccountId,
        snapshot: &VenueSnapshot,
    ) -> Result<(), String> {
        let positions = snapshot
            .positions
            .iter()
            .map(|position| self.broker_position(account_id, position))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        self.write_positions(account_id, positions, &[], true)?;
        let mut reconfirmed = BTreeSet::new();
        for report in &snapshot.orders {
            if let Some(order) = self.reported_order(account_id, report) {
                reconfirmed.insert(order.id);
                self.apply_broker_order(
                    account_id,
                    report,
                    BrokerOrderState::Accepted,
                    Some("confirmed by reconcile"),
                    true,
                )?;
            }
        }
        let missing: Vec<Order> = self
            .state
            .orders
            .values()
            .filter(|order| {
                order.account_id == *account_id
                    && order.status.is_open()
                    && !reconfirmed.contains(&order.id)
            })
            .cloned()
            .collect();
        for order in missing {
            let bound = self.broker_order_id(&order.id).is_some();
            let (status, kind, detail) = if order.filled_quantity == order.quantity {
                (
                    OrderStatus::Filled,
                    OrderEventKind::Filled,
                    "filled at cTrader",
                )
            } else if bound || order.filled_quantity.units() > 0 {
                (
                    OrderStatus::Cancelled,
                    OrderEventKind::Cancelled,
                    "no longer open at cTrader",
                )
            } else {
                (
                    OrderStatus::Rejected,
                    OrderEventKind::Rejected,
                    "cTrader did not receive the order",
                )
            };
            let updated = Order {
                status,
                provenance: self.venue_provenance()?,
                ..order
            };
            let provenance = updated.provenance.clone();
            self.record_broker_transition(updated, kind, detail, provenance, None)?;
        }
        Ok(())
    }

    /// The owner order a broker report describes: by its bound broker id, or, before the
    /// first report binds it, by client order id within the same account only.
    fn reported_order(&self, account_id: &TradingAccountId, report: &BrokerOrder) -> Option<Order> {
        if let Some(order_id) = self.state.broker_order_ids.get(&report.broker_order_id) {
            return self
                .state
                .orders
                .get(order_id)
                .filter(|order| order.account_id == *account_id)
                .cloned();
        }
        let client = report.client_order_id.as_deref()?;
        self.state
            .orders
            .values()
            .find(|order| {
                order.account_id == *account_id
                    && order.client_order_id.as_str() == client
                    && self.broker_order_id(&order.id).is_none()
            })
            .cloned()
    }

    /// Applies one broker order report. `resolve_pending` (a reconcile) treats an
    /// unanswered modify or cancel as settled by the broker's current state.
    fn apply_broker_order(
        &mut self,
        account_id: &TradingAccountId,
        report: &BrokerOrder,
        state: BrokerOrderState,
        reason: Option<&str>,
        resolve_pending: bool,
    ) -> Result<(), String> {
        let Some(current) = self.reported_order(account_id, report) else {
            return Ok(());
        };
        let binding = !self
            .state
            .broker_order_ids
            .contains_key(&report.broker_order_id);
        if !current.status.is_open() {
            return Ok(());
        }
        if report.quantity.scale() != current.quantity.scale()
            || report.filled_quantity.scale() != current.quantity.scale()
        {
            return Err("broker order quantity scale does not match the order".into());
        }
        let (status, kind, detail) =
            reported_transition(&current, report, state, reason, resolve_pending);
        let mut updated = Order {
            status,
            quantity: report.quantity,
            filled_quantity: report.filled_quantity,
            ..current.clone()
        };
        // The broker's report is authoritative for prices, so a replacement applies the
        // prices the broker confirmed rather than the request.
        if matches!(current.order_type, OrderType::Limit | OrderType::StopLimit) {
            updated.limit_price = report.limit_price.or(current.limit_price);
        }
        if matches!(current.order_type, OrderType::Stop | OrderType::StopLimit) {
            updated.stop_price = report.stop_price.or(current.stop_price);
        }
        if updated.status == OrderStatus::Filled {
            updated.filled_quantity = updated.quantity;
        }
        let changed = binding
            || updated.status != current.status
            || updated.filled_quantity != current.filled_quantity
            || updated.limit_price != current.limit_price
            || updated.stop_price != current.stop_price;
        if !changed {
            return Ok(());
        }
        updated.provenance = self.venue_provenance()?;
        let provenance = updated.provenance.clone();
        self.record_broker_transition(
            updated,
            kind,
            &detail,
            provenance,
            binding.then_some(report.broker_order_id.as_str()),
        )
    }

    /// A refused place rejects the order; a refused amend or cancel returns the order to
    /// its last confirmed state.
    fn apply_refusal(
        &mut self,
        account_id: &TradingAccountId,
        client_order_id: &ClientOrderId,
        reason: &str,
    ) -> Result<(), String> {
        let Some(current) = self
            .state
            .orders
            .values()
            .find(|order| {
                order.account_id == *account_id && order.client_order_id == *client_order_id
            })
            .cloned()
        else {
            return Ok(());
        };
        let status = match current.status {
            OrderStatus::Pending => OrderStatus::Rejected,
            OrderStatus::PendingModify | OrderStatus::PendingCancel => confirmed_status(&current),
            _ => return Ok(()),
        };
        let updated = Order {
            status,
            provenance: self.venue_provenance()?,
            ..current
        };
        let provenance = updated.provenance.clone();
        self.record_broker_transition(updated, OrderEventKind::Rejected, reason, provenance, None)
    }

    fn venue_provenance(&self) -> Result<TradingProvenance, String> {
        Ok(TradingProvenance {
            venue_id: "ctrader".into(),
            provider_id: "ctrader".into(),
            session_generation: self.state.venue_generation,
            source_sequence: self.venue_ingestion,
            observed_unix_nanos: current_unix_nanos()?,
        })
    }

    fn queue_broker_transition(
        &mut self,
        updated: &Order,
        kind: OrderEventKind,
        detail: &str,
        request: VenueRequest,
    ) -> Result<(), String> {
        updated.validate().map_err(|error| error.to_string())?;
        request.validate().map_err(|error| error.to_string())?;
        let outbound = self.venue_outbound.clone().ok_or(DEMO_VENUE_UNAVAILABLE)?;
        let previous = self
            .state
            .orders
            .get(&updated.id)
            .ok_or("broker order is not registered")?
            .clone();
        let sequence = self.state.next_sequence;
        let provenance = self.broker_provenance(current_unix_nanos()?, sequence);
        let event = Self::broker_event(updated, sequence, kind, detail, provenance)?;
        self.store
            .transition_broker_order(updated, &event, sequence + 1, None)?;
        if let Err(error) = outbound.try_send(request) {
            self.store
                .reject_unsent_broker_transition(&previous, &event)?;
            return Err(outbound_error(&error));
        }
        self.state.next_sequence = sequence + 1;
        self.state
            .orders
            .insert(updated.id.clone(), updated.clone());
        self.state.order_events.push_back(event);
        self.bump_revision()
    }

    fn broker_provenance(&self, observed_unix_nanos: i64, sequence: u64) -> TradingProvenance {
        TradingProvenance {
            venue_id: "ctrader".into(),
            provider_id: "ctrader".into(),
            session_generation: self.state.venue_generation,
            source_sequence: sequence,
            observed_unix_nanos,
        }
    }

    fn broker_event(
        order: &Order,
        sequence: u64,
        kind: OrderEventKind,
        detail: &str,
        provenance: TradingProvenance,
    ) -> Result<OrderEvent, String> {
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("ct-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind,
            event_unix_nanos: provenance.observed_unix_nanos,
            detail: Some(detail.to_string()),
            provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        Ok(event)
    }

    fn record_broker_transition(
        &mut self,
        updated: Order,
        kind: OrderEventKind,
        detail: &str,
        provenance: TradingProvenance,
        binding: Option<&str>,
    ) -> Result<(), String> {
        updated.validate().map_err(|error| error.to_string())?;
        let sequence = self.state.next_sequence;
        let event = Self::broker_event(&updated, sequence, kind, detail, provenance)?;
        self.store
            .transition_broker_order(&updated, &event, sequence + 1, binding)?;
        if let Some(broker_order_id) = binding {
            self.state
                .broker_order_ids
                .insert(broker_order_id.to_string(), updated.id.clone());
        }
        self.state.next_sequence = sequence + 1;
        self.state.orders.insert(updated.id.clone(), updated);
        self.state.order_events.push_back(event);
        self.bump_revision()
    }
}

/// The order after one more deal. Its filled quantity is the larger of what it already
/// shows and what its deals add up to; a broker-originated order's quantity grows with its
/// deals, since the broker reports it only through them.
fn filled_order(
    current: &Order,
    filled_by_deals: FixedPoint,
    mirror: bool,
    provenance: TradingProvenance,
) -> Result<Order, String> {
    let quantity = if mirror || filled_by_deals.units() > current.quantity.units() {
        filled_by_deals
    } else {
        current.quantity
    };
    let filled = if filled_by_deals.units() > current.filled_quantity.units() {
        filled_by_deals
    } else {
        current.filled_quantity
    };
    let status = if filled == quantity {
        OrderStatus::Filled
    } else if !current.status.is_open()
        || matches!(
            current.status,
            OrderStatus::PendingModify | OrderStatus::PendingCancel
        )
    {
        current.status
    } else {
        OrderStatus::PartiallyFilled
    };
    let updated = Order {
        quantity,
        filled_quantity: filled,
        status,
        provenance,
        ..current.clone()
    };
    updated.validate().map_err(|error| error.to_string())?;
    Ok(updated)
}

/// The time in force a broker order actually carries. Market orders fill or cancel at once.
/// cTrader offers no day orders, so a day pending order rests good-till-cancelled
/// (maintainer decision, 2026-10-08); the stored order says so.
const fn broker_time_in_force(order_type: OrderType, requested: TimeInForce) -> TimeInForce {
    match (order_type, requested) {
        (OrderType::Market, _) => TimeInForce::ImmediateOrCancel,
        (_, TimeInForce::Day) => TimeInForce::GoodTillCancelled,
        (_, requested) => requested,
    }
}

/// The last confirmed working status of an order with a pending request.
fn confirmed_status(order: &Order) -> OrderStatus {
    if order.filled_quantity.units() == 0 {
        OrderStatus::Working
    } else {
        OrderStatus::PartiallyFilled
    }
}

/// The owner status a broker report moves an order to. A pending modify or cancel stays
/// pending until the broker answers that request, so an acceptance or a partial fill never
/// hides an unanswered request.
fn reported_transition(
    current: &Order,
    report: &BrokerOrder,
    state: BrokerOrderState,
    reason: Option<&str>,
    resolve_pending: bool,
) -> (OrderStatus, OrderEventKind, String) {
    let pending_request = !resolve_pending
        && matches!(
            current.status,
            OrderStatus::PendingModify | OrderStatus::PendingCancel
        );
    let working = if report.filled_quantity.units() == 0 {
        OrderStatus::Working
    } else {
        OrderStatus::PartiallyFilled
    };
    let detail = |text: &str| reason.unwrap_or(text).to_string();
    match state {
        BrokerOrderState::Accepted if pending_request => {
            (current.status, OrderEventKind::Accepted, detail("accepted"))
        }
        BrokerOrderState::Accepted => (working, OrderEventKind::Accepted, detail("accepted")),
        BrokerOrderState::Replaced if current.status == OrderStatus::PendingCancel => {
            (current.status, OrderEventKind::Modified, detail("replaced"))
        }
        BrokerOrderState::Replaced => (working, OrderEventKind::Modified, detail("replaced")),
        BrokerOrderState::PartiallyFilled if pending_request => (
            current.status,
            OrderEventKind::Filled,
            detail("partially filled"),
        ),
        BrokerOrderState::PartiallyFilled => {
            (working, OrderEventKind::Filled, detail("partially filled"))
        }
        BrokerOrderState::Filled => (
            OrderStatus::Filled,
            OrderEventKind::Filled,
            detail("filled"),
        ),
        BrokerOrderState::Cancelled => (
            OrderStatus::Cancelled,
            OrderEventKind::Cancelled,
            detail("cancelled"),
        ),
        BrokerOrderState::Expired => (
            OrderStatus::Cancelled,
            OrderEventKind::Cancelled,
            detail("expired"),
        ),
        BrokerOrderState::CancelRejected if current.status == OrderStatus::PendingCancel => {
            (working, OrderEventKind::Rejected, detail("cancel rejected"))
        }
        BrokerOrderState::CancelRejected => (
            current.status,
            OrderEventKind::Rejected,
            detail("cancel rejected"),
        ),
        // A rejection after a partial fill ends the order without undoing its fills.
        BrokerOrderState::Rejected if report.filled_quantity.units() > 0 => (
            OrderStatus::Cancelled,
            OrderEventKind::Rejected,
            detail("rejected"),
        ),
        BrokerOrderState::Rejected => (
            OrderStatus::Rejected,
            OrderEventKind::Rejected,
            detail("rejected"),
        ),
    }
}

fn outbound_error<T>(error: &TrySendError<T>) -> String {
    match error {
        TrySendError::Full(_) => "cTrader venue outbound queue is full".into(),
        TrySendError::Disconnected(_) => DEMO_VENUE_UNAVAILABLE.into(),
    }
}
