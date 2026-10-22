//! Broker order state is persisted and advanced only by the trading owner. Broker fills run
//! through the same net-position ledger as practice fills; the venue's position reports supply
//! the open P&L and settle any difference at reconcile.

use super::FillMemoryUpdate;
use crate::BracketStrategyTemplate;
use crate::core::ledger::{CostBasis, TradeCycle};
use crate::venue::{VenueEvent, VenueRequest, VenueUpdate};
use crate::{
    DEMO_VENUE_UNAVAILABLE, FlattenOutcome, ModifyOrder, MoveBracketLevel, PendingExit, PlaceOrder,
    TradingCore, TradingInstrument, TradingProvenance, store::BrokerFillRecord,
};
use aeris_instruments::InstrumentId;
use aeris_trading::{
    AccountEnvironment, ClientOrderId, Fill, FillId, FixedPoint, Liquidity, Order, OrderEvent,
    OrderEventId, OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, Position,
    TimeInForce, TradingAccount, TradingAccountId,
    venue::{
        BROKER_VENUE_ID, BracketLevel, BracketSide, BrokerFill, BrokerOrder, BrokerOrderKind,
        BrokerOrderState, ObservedAccount, ServerBracket, VenueAmendment, VenueOrder,
        VenuePosition, VenueSnapshot,
    },
};
use std::{collections::BTreeSet, sync::mpsc::TrySendError};

const NOT_ACKNOWLEDGED: &str = "the broker has not acknowledged this order yet; try again shortly";

impl TradingCore {
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
                account.venue_id == BROKER_VENUE_ID
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
            .open_orders()
            .filter(|order| order.account_id == account.id)
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
            let newly_connected = self.connected_broker_accounts.insert(account_id);
            if known.display_name == observed.display_name {
                return if newly_connected {
                    self.bump_revision()
                } else {
                    Ok(())
                };
            }
            known.display_name.clone_from(&observed.display_name);
            return self.register_account(known);
        }
        let account = TradingAccount {
            id: TradingAccountId::try_new(format!(
                "broker-{}-{broker_account}",
                observed.environment.as_str()
            ))
            .map_err(|error| error.to_string())?,
            display_name: observed.display_name.clone(),
            environment: observed.environment,
            venue_id: BROKER_VENUE_ID.into(),
            broker_ref: Some(broker_account.to_string()),
            currency: observed.currency.clone(),
            currency_scale: observed.currency_scale,
            starting_equity: None,
        };
        let reconcile = (account.environment == AccountEnvironment::Demo)
            .then(|| self.reconcile_request(&account))
            .flatten();
        self.connected_broker_accounts.insert(account.id.clone());
        self.register_account(account)?;
        match reconcile {
            Some(request) if self.venue_outbound.is_some() => self.send_venue_request(request),
            _ => Ok(()),
        }
    }

    fn broker_order_id(&self, order_id: &OrderId) -> Option<String> {
        self.state
            .orders
            .broker_for_order(order_id)
            .map(str::to_owned)
    }

    pub(crate) fn place_broker_order(&mut self, command: &PlaceOrder) -> Result<Order, String> {
        self.place_broker_entry(command, None)
    }

    /// A bracket on a broker account is one entry order carrying a server-held bracket: the
    /// template's targets split the entry quantity exactly as on a practice account and one
    /// stop covers all of it. The venue releases the levels as the entry fills, so they keep
    /// working while Aeris is disconnected.
    pub(crate) fn place_broker_bracket(
        &mut self,
        command: &PlaceOrder,
        template: &BracketStrategyTemplate,
    ) -> Result<Order, String> {
        let held_levels = self
            .venue_capabilities
            .map_or(0, |capabilities| capabilities.bracket_levels);
        if held_levels == 0 {
            return Err("the broker venue holds no server-side brackets".into());
        }
        let instrument = self
            .state
            .instruments
            .get(&command.instrument_id)
            .ok_or("trading instrument is not registered")?;
        let ticks = |offset: u32| {
            i32::try_from(offset).map_err(|_| "bracket distance is out of range".to_string())
        };
        let mut targets = if template.targets.is_empty() {
            Vec::new()
        } else {
            super::allocated_target_quantities(
                command.quantity,
                &template.targets,
                super::order_increment_units(instrument),
            )?
            .into_iter()
            .map(|(index, quantity)| {
                Ok(BracketLevel {
                    quantity,
                    ticks: ticks(template.targets[index].offset_ticks)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?
        };
        if targets.len() > held_levels {
            return Err(format!(
                "the broker venue holds at most {held_levels} target levels per bracket"
            ));
        }
        targets.sort_by_key(|level| level.ticks);
        let bracket = ServerBracket {
            targets,
            stops: vec![BracketLevel {
                quantity: command.quantity,
                ticks: ticks(template.stop_offset_ticks)?,
            }],
        };
        bracket
            .validate(command.quantity)
            .map_err(|error| error.to_string())?;
        self.place_broker_entry(command, Some(bracket))
    }

    fn place_broker_entry(
        &mut self,
        command: &PlaceOrder,
        bracket: Option<ServerBracket>,
    ) -> Result<Order, String> {
        if self.venue_outbound.is_none() {
            return Err(DEMO_VENUE_UNAVAILABLE.into());
        }
        let instrument = self
            .state
            .instruments
            .get(&command.instrument_id)
            .ok_or("trading instrument is not registered")?
            .clone();
        let warnings = crate::core::admission::admit(
            self,
            &instrument,
            command,
            crate::VenueRoute::BrokerDemo,
            0,
            &[],
            crate::core::admission::AdmissionRisk::Enforced {
                attached_stop: command.stop_price.is_some() || bracket.is_some(),
            },
        )?;
        // Every M3.2 check applies to broker orders. The practice rule that the contract
        // currency equal the account currency does not: the broker converts profit and loss
        // into the deposit currency.
        let time_in_force =
            crate::core::admission::RouteCapabilities::for_route(crate::VenueRoute::BrokerDemo)
                .effective_tif(command.order_type, command.time_in_force);
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
            bracket: bracket.clone(),
        });
        request.validate().map_err(|error| error.to_string())?;
        // Validate and persist before making a request visible to the relay.
        let sequence = self.state.next_sequence;
        let provenance = self.broker_provenance(self.command_now, sequence);
        let order = Order {
            id: OrderId::try_new(format!("broker-order-{sequence}"))
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
            revision: 0,
            accepted_unix_nanos: 0,
            eligible_after_unix_nanos: 0,
            triggered_unix_nanos: None,
            expires_at_unix_nanos: None,
            updated_unix_nanos: 0,
            terminal_unix_nanos: None,
            provenance: provenance.clone(),
        };
        let order = crate::book::lifecycle::accept(order, self.command_now)
            .map_err(|error| error.to_string())?;
        order.validate().map_err(|error| error.to_string())?;
        let mut event = Self::broker_event(
            &order,
            sequence,
            OrderEventKind::Accepted,
            "submitted to the broker",
            provenance,
        )?;
        if !warnings.is_empty() {
            event.reason = Some(aeris_trading::OrderEventReason::RiskWarning);
            event.detail = Some(format!(
                "submitted to the broker · {}",
                warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        let outbound = self.venue_outbound.clone().ok_or(DEMO_VENUE_UNAVAILABLE)?;
        let reservation = outbound
            .try_reserve(request)
            .map_err(|error| outbound_error(&error))?;
        self.store
            .insert_broker_order(&order, &event, None, bracket.as_ref(), sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.orders.insert(order.id.clone(), order.clone());
        if let Some(bracket) = bracket {
            self.state.broker_brackets.insert(order.id.clone(), bracket);
        }
        self.state.order_events.push_back(event);
        self.state.revision = self.state.revision.saturating_add(1).min(i64::MAX as u64);
        reservation.commit();
        Ok(order)
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
        if crate::core::admission::RouteCapabilities::for_route(crate::VenueRoute::BrokerDemo)
            .effective_tif(order.order_type, command.time_in_force)
            != order.time_in_force
        {
            return Err("the broker cannot change an order's time in force".into());
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
            order_type: order.order_type,
            quantity: order.quantity,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
        });
        let pending =
            crate::book::lifecycle::request(order, OrderStatus::PendingModify, self.command_now)
                .map_err(|error| error.to_string())?;
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
        let pending =
            crate::book::lifecycle::request(order, OrderStatus::PendingCancel, self.command_now)
                .map_err(|error| error.to_string())?;
        self.queue_broker_transition(
            &pending,
            OrderEventKind::Cancelled,
            "cancel requested",
            request,
        )?;
        Ok(pending)
    }

    /// Asks the venue to move one level of a bracket it holds. The level keeps its distance
    /// until the venue reports the move, so a lost request never changes the record.
    pub(crate) fn move_bracket_level(&self, command: &MoveBracketLevel) -> Result<(), String> {
        let entry = self
            .state
            .orders
            .by_client(&command.client_order_id)
            .ok_or("bracket entry is not registered")?;
        self.require_broker_route(&entry.account_id)?;
        let bracket = self
            .state
            .broker_brackets
            .get(&entry.id)
            .ok_or("the order holds no server-side bracket")?;
        let levels = bracket.side(command.side);
        let level = levels
            .iter()
            .find(|level| level.ticks == command.current_ticks)
            .ok_or("the bracket holds no level at that distance")?;
        if levels.iter().any(|level| level.ticks == command.new_ticks) {
            return Err("the bracket already holds a level at the new distance".into());
        }
        if command.side == BracketSide::Stop && command.new_ticks > command.current_ticks {
            self.check_bracket_stop_risk(
                &entry.account_id,
                &entry.instrument_id,
                level.quantity,
                u32::try_from(command.new_ticks)
                    .map_err(|_| "bracket distance is out of range".to_string())?,
            )?;
        }
        let entry_broker_order_id = self.broker_order_id(&entry.id).ok_or(NOT_ACKNOWLEDGED)?;
        self.send_venue_request(VenueRequest::MoveBracketLevel {
            broker_account: self.broker_account(&entry.account_id)?,
            entry_broker_order_id,
            side: command.side,
            current_ticks: command.current_ticks,
            new_ticks: command.new_ticks,
        })
    }

    fn send_venue_request(&self, request: VenueRequest) -> Result<(), String> {
        request.validate().map_err(|error| error.to_string())?;
        self.venue_outbound
            .as_ref()
            .ok_or(DEMO_VENUE_UNAVAILABLE)?
            .try_send(request)
            .map_err(|error| outbound_error(&error))
    }

    /// Flattens one broker account across the scoped instruments. A whole-account flatten
    /// cancels every working order at the venue, including bracket exits Aeris has not seen;
    /// an instrument flatten cancels that instrument's open orders. Each open position in
    /// scope is then exited at the venue and stays until the venue reports the fills. Exits
    /// are requested even when a cancel cannot be; what failed is reported in the outcome.
    pub(crate) fn flatten_broker_account(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: Option<&InstrumentId>,
    ) -> Result<FlattenOutcome, String> {
        let broker_account = self.broker_account(account_id)?;
        let mut incomplete = match instrument_id {
            // Without an attached venue nothing can be asked, and only orders the owner
            // knows of are worth reporting as left working.
            None if self.venue_outbound.is_none()
                && self.state.orders.open_for_account(account_id).is_empty() =>
            {
                None
            }
            None => self
                .send_venue_request(VenueRequest::CancelAll {
                    broker_account: broker_account.clone(),
                })
                .err()
                .map(|error| format!("open broker orders were not cancelled: {error}")),
            Some(_) => self.cancel_open_orders(Some(account_id), instrument_id)?.1,
        };
        let positions = self
            .state
            .positions
            .values()
            .filter(|position| {
                position.account_id == *account_id
                    && position.net_quantity.units() != 0
                    && instrument_id.is_none_or(|scope| scope == &position.instrument_id)
            })
            .map(|position| PendingExit {
                account_id: position.account_id.clone(),
                instrument_id: position.instrument_id.clone(),
                net_quantity: position.net_quantity,
            })
            .collect::<Vec<_>>();
        let mut pending_exits = Vec::with_capacity(positions.len());
        for exit in positions {
            match self.send_venue_request(VenueRequest::ExitPosition {
                broker_account: broker_account.clone(),
                instrument_id: exit.instrument_id.clone(),
            }) {
                Ok(()) => pending_exits.push(exit),
                Err(error) => {
                    let failure = format!(
                        "the {} position was not exited: {error}",
                        exit.instrument_id.as_str()
                    );
                    incomplete = Some(match incomplete {
                        Some(earlier) => format!("{earlier}; {failure}"),
                        None => failure,
                    });
                }
            }
        }
        Ok(FlattenOutcome {
            fills: Vec::new(),
            pending_exits,
            incomplete,
            unpriced: Vec::new(),
        })
    }

    /// Reverses the account's broker position in one instrument: exits it and places one
    /// risk-checked market order for the opposite quantity. Nothing is placed unless the exit
    /// was requested, so a failed exit can never double the position.
    pub(crate) fn reverse_broker_position(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        provenance: &TradingProvenance,
    ) -> Result<FlattenOutcome, String> {
        let net = self
            .state
            .positions
            .get(&(account_id.clone(), instrument_id.clone()))
            .map(|position| position.net_quantity)
            .filter(|net| net.units() != 0)
            .ok_or("reverse requires an open position")?;
        let side = if net.units() > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let quantity = FixedPoint::try_new(
            net.units()
                .checked_abs()
                .ok_or("reverse quantity overflowed")?,
            net.scale(),
        )
        .map_err(|error| error.to_string())?;
        let outcome = self.flatten_broker_account(account_id, Some(instrument_id))?;
        if outcome.pending_exits.is_empty() {
            return Err(outcome
                .incomplete
                .unwrap_or_else(|| "the position exit could not be requested".into()));
        }
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
        match &self.owner_scaled(&account_id, &event.update)? {
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
            VenueUpdate::Fill(fill) => {
                if self.apply_broker_fill(&account_id, fill)? {
                    self.evaluate_live_risk(event.observed_unix_nanos)?;
                }
                Ok(())
            }
            VenueUpdate::Position(position) => {
                if self.apply_open_pnl(&account_id, position)? {
                    self.evaluate_live_risk(event.observed_unix_nanos)?;
                }
                Ok(())
            }
            VenueUpdate::BracketLevelMoved {
                entry_broker_order_id,
                side,
                from_ticks,
                to_ticks,
            } => self.apply_bracket_move(
                &account_id,
                entry_broker_order_id,
                *side,
                *from_ticks,
                *to_ticks,
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

    /// Applies the venue's open P&L to the position it describes and reports whether it
    /// changed. It is a live mark held in owner memory like a quote mark. A report whose
    /// quantity differs from the ledger waits for the fills, or the next reconcile, that
    /// bring the two together.
    fn apply_open_pnl(
        &mut self,
        account_id: &TradingAccountId,
        report: &VenuePosition,
    ) -> Result<bool, String> {
        let Some(open_pnl) = report.open_pnl else {
            return Ok(false);
        };
        let Some(position) = self
            .state
            .positions
            .get_mut(&(account_id.clone(), report.instrument_id.clone()))
        else {
            return Ok(false);
        };
        let reported = report
            .net_quantity
            .exact_rescale(position.net_quantity.scale())
            .map_err(|error| error.to_string())?;
        if reported != position.net_quantity || reported.units() == 0 {
            return Ok(false);
        }
        let open_pnl = open_pnl
            .exact_rescale(position.unrealized_pnl.scale())
            .map_err(|error| error.to_string())?;
        if position.unrealized_pnl == open_pnl {
            return Ok(false);
        }
        position.unrealized_pnl = open_pnl;
        self.mark_revision = self.mark_revision.saturating_add(1);
        Ok(true)
    }

    /// Records a bracket level the venue moved. A move of a level the owner no longer holds
    /// changes nothing.
    fn apply_bracket_move(
        &mut self,
        account_id: &TradingAccountId,
        entry_broker_order_id: &str,
        side: BracketSide,
        from_ticks: i32,
        to_ticks: i32,
    ) -> Result<(), String> {
        let Some(order_id) = self
            .state
            .orders
            .broker_owner(entry_broker_order_id)
            .filter(|order_id| {
                self.state
                    .orders
                    .get(order_id)
                    .is_some_and(|order| order.account_id == *account_id)
            })
            .cloned()
        else {
            return Ok(());
        };
        let Some(levels) = self
            .state
            .broker_brackets
            .get(&order_id)
            .map(|bracket| bracket.side(side))
        else {
            return Ok(());
        };
        if !levels.iter().any(|level| level.ticks == from_ticks) {
            return Ok(());
        }
        if to_ticks <= 0 || levels.iter().any(|level| level.ticks == to_ticks) {
            return Err("the broker moved a bracket level onto an invalid distance".into());
        }
        self.store
            .move_broker_bracket_level(&order_id, side, from_ticks, to_ticks)?;
        if let Some(levels) = self
            .state
            .broker_brackets
            .get_mut(&order_id)
            .map(|bracket| bracket.side_mut(side))
        {
            for level in levels.iter_mut().filter(|level| level.ticks == from_ticks) {
                level.ticks = to_ticks;
            }
            levels.sort_by_key(|level| level.ticks);
        }
        self.bump_revision()
    }

    /// Records one broker deal once and runs it through the account's net position exactly
    /// like a practice fill: position, cost basis, trade cycle, per-fill accounting and the
    /// rule and discipline state commit with the order transition. The broker holds the
    /// account's cash, so a broker fill writes no cash-ledger entries. Our order's filled
    /// quantity is the larger of what the broker reported and what its deals add up to, so
    /// replays never double-count. Returns whether the deal was new.
    fn apply_broker_fill(
        &mut self,
        account_id: &TradingAccountId,
        report: &BrokerFill,
    ) -> Result<bool, String> {
        if self.store.broker_deal_recorded(&report.broker_deal_id)? {
            return Ok(false);
        }
        let (account, instrument) = self.deal_terms(account_id, report)?;
        let provenance = self.venue_provenance();
        let sequence = self.state.next_sequence;
        let (current, mirror) = self.deal_order(account_id, report, &provenance)?;
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
        let filled_by_deals = self.deal_quantity(&current.id, report.quantity)?;
        let updated = crate::book::lifecycle::broker_deal(
            &current,
            filled_by_deals,
            mirror,
            provenance,
            self.command_now,
        )
        .map_err(|error| error.to_string())?;
        updated.validate().map_err(|error| error.to_string())?;
        let event = Self::broker_event(
            &updated,
            sequence,
            OrderEventKind::Filled,
            "filled at the broker",
            updated.provenance.clone(),
        )?;
        let position_key = (account_id.clone(), report.instrument_id.clone());
        let previous = self.state.positions.get(&position_key).cloned();
        let (position, cost_basis, mut policy_update) = self.apply_fill(
            &fill,
            previous.as_ref(),
            &account,
            &instrument,
            Liquidity::Taker,
        )?;
        policy_update.ledger_entries.clear();
        let released = self.ended_brackets(
            account_id,
            &report.instrument_id,
            position.net_quantity.units() == 0,
            Some(&updated),
        );
        self.store.insert_broker_fill(
            &BrokerFillRecord {
                fill: &fill,
                order: &updated,
                event: &event,
                mirror: mirror.then_some((&current, report.broker_order_id.as_str())),
                broker_deal_id: &report.broker_deal_id,
                position: &position,
                cost_basis,
                accounting: policy_update.accounting,
                trade_cycle: &policy_update.trade_cycle,
                rule_state: policy_update.rule_state.as_ref(),
                discipline_state: &policy_update.discipline_state,
                released_brackets: &released,
            },
            sequence + 1,
        )?;
        for order_id in &released {
            self.state.broker_brackets.remove(order_id);
        }
        self.apply_fill_to_memory(
            updated,
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
        if mirror {
            self.state
                .orders
                .bind_broker(report.broker_order_id.clone(), current.id);
        }
        self.bump_revision()?;
        Ok(true)
    }

    /// The account and instrument a deal is accounted in, at scales the deal matches.
    fn deal_terms(
        &self,
        account_id: &TradingAccountId,
        report: &BrokerFill,
    ) -> Result<(TradingAccount, TradingInstrument), String> {
        let instrument = self
            .state
            .instruments
            .get(&report.instrument_id)
            .cloned()
            .ok_or("broker fill names an unregistered instrument")?;
        if report.quantity.scale() != instrument.quantity_scale
            || report.price.scale() != instrument.price_scale
        {
            return Err("broker fill scales do not match the instrument".into());
        }
        let account = self
            .state
            .accounts
            .get(account_id)
            .cloned()
            .ok_or("trading account is not registered")?;
        Ok((account, instrument))
    }

    /// The order a deal belongs to: its bound order, or a mirror of a broker-originated order
    /// first seen through this deal. Reports whether the order is a new mirror.
    fn deal_order(
        &self,
        account_id: &TradingAccountId,
        report: &BrokerFill,
        provenance: &TradingProvenance,
    ) -> Result<(Order, bool), String> {
        if let Some(order) = self
            .state
            .orders
            .broker_owner(&report.broker_order_id)
            .and_then(|order_id| self.state.orders.get(order_id))
            .filter(|order| order.account_id == *account_id)
        {
            if report.quantity.scale() != order.quantity.scale() {
                return Err("broker fill quantity scale does not match the order".into());
            }
            return Ok((order.clone(), false));
        }
        let order = Self::mirror_order(
            account_id,
            &report.broker_order_id,
            &report.instrument_id,
            report.side,
            report.quantity,
            provenance,
        )?;
        order.validate().map_err(|error| error.to_string())?;
        Ok((order, true))
    }

    fn deal_quantity(
        &self,
        order_id: &OrderId,
        incoming: FixedPoint,
    ) -> Result<FixedPoint, String> {
        self.state
            .fills
            .iter()
            .filter(|known| &known.order_id == order_id)
            .try_fold(incoming, |total, known| total.checked_add(known.quantity))
            .map_err(|error| error.to_string())
    }

    /// A broker-originated order (a released bracket exit, a venue exit, or an order placed
    /// outside Aeris), accepted as a working market order. Its client id is namespaced by the
    /// broker order id, because brokers reuse request ids across sessions.
    fn mirror_order(
        account_id: &TradingAccountId,
        broker_order_id: &str,
        instrument_id: &InstrumentId,
        side: OrderSide,
        quantity: FixedPoint,
        provenance: &TradingProvenance,
    ) -> Result<Order, String> {
        let sequence = provenance.source_sequence;
        let order = Order {
            id: OrderId::try_new(format!("broker-order-{sequence}"))
                .map_err(|error| error.to_string())?,
            client_order_id: ClientOrderId::try_new(format!("ct-{broker_order_id}"))
                .map_err(|error| error.to_string())?,
            account_id: account_id.clone(),
            instrument_id: instrument_id.clone(),
            side,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            quantity,
            filled_quantity: FixedPoint::try_new(0, quantity.scale())
                .map_err(|error| error.to_string())?,
            limit_price: None,
            stop_price: None,
            status: OrderStatus::Working,
            submitted_unix_nanos: provenance.observed_unix_nanos,
            revision: 0,
            accepted_unix_nanos: 0,
            eligible_after_unix_nanos: 0,
            triggered_unix_nanos: None,
            expires_at_unix_nanos: None,
            updated_unix_nanos: 0,
            terminal_unix_nanos: None,
            provenance: provenance.clone(),
        };
        crate::book::lifecycle::accept(order, provenance.observed_unix_nanos)
            .map_err(|error| error.to_string())
    }

    /// Mirrors a working broker order Aeris did not place when the venue first reports it,
    /// so it can be seen, cancelled and matched to its deals. An order of a kind Aeris does
    /// not place stays unmirrored until a deal arrives. Returns the mirrored order's id.
    fn mirror_open_order(
        &mut self,
        account_id: &TradingAccountId,
        report: &BrokerOrder,
    ) -> Result<Option<OrderId>, String> {
        let order_type = match report.kind {
            BrokerOrderKind::Market => OrderType::Market,
            BrokerOrderKind::Limit => OrderType::Limit,
            BrokerOrderKind::Stop => OrderType::Stop,
            BrokerOrderKind::StopLimit => OrderType::StopLimit,
            BrokerOrderKind::Other => return Ok(None),
        };
        let instrument = self
            .state
            .instruments
            .get(&report.instrument_id)
            .ok_or("broker order names an unregistered instrument")?;
        if report.quantity.scale() != instrument.quantity_scale
            || report.filled_quantity.scale() != instrument.quantity_scale
        {
            return Err("broker order quantity scale does not match the instrument".into());
        }
        // The venue sequence keys the mirror's id; the owner sequence keys its event.
        let sequence = self.state.next_sequence;
        let provenance = self.broker_provenance(self.command_now, sequence);
        let mut order = Self::mirror_order(
            account_id,
            &report.broker_order_id,
            &report.instrument_id,
            report.side,
            report.quantity,
            &provenance,
        )?;
        order.order_type = order_type;
        if order_type != OrderType::Market {
            order.time_in_force = TimeInForce::GoodTillCancelled;
        }
        if matches!(order_type, OrderType::Limit | OrderType::StopLimit) {
            order.limit_price = report.limit_price;
        }
        if matches!(order_type, OrderType::Stop | OrderType::StopLimit) {
            order.stop_price = report.stop_price;
        }
        order.filled_quantity = report.filled_quantity;
        if report.filled_quantity.units() > 0 {
            order.status = OrderStatus::PartiallyFilled;
        }
        order.validate().map_err(|error| error.to_string())?;
        let event = Self::broker_event(
            &order,
            sequence,
            OrderEventKind::Accepted,
            "reported by the broker",
            provenance,
        )?;
        self.store.insert_broker_order(
            &order,
            &event,
            Some(&report.broker_order_id),
            None,
            sequence + 1,
        )?;
        let order_id = order.id.clone();
        self.state.next_sequence = sequence + 1;
        self.state.orders.insert(order_id.clone(), order);
        self.state
            .orders
            .bind_broker(report.broker_order_id.clone(), order_id.clone());
        self.state.order_events.push_back(event);
        self.bump_revision()?;
        Ok(Some(order_id))
    }

    /// Brackets that end with this step. An entry's levels are released once the entry is
    /// terminal and either never filled or left the account flat in the instrument, because
    /// the venue then holds no exit for it.
    fn ended_brackets(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        flat: bool,
        changed: Option<&Order>,
    ) -> Vec<OrderId> {
        self.state
            .broker_brackets
            .keys()
            .filter(|order_id| {
                changed
                    .filter(|order| &order.id == *order_id)
                    .or_else(|| self.state.orders.get(order_id))
                    .is_some_and(|entry| {
                        entry.account_id == *account_id
                            && entry.instrument_id == *instrument_id
                            && !entry.status.is_open()
                            && (flat || entry.filled_quantity.units() == 0)
                    })
            })
            .cloned()
            .collect()
    }

    fn is_flat(&self, account_id: &TradingAccountId, instrument_id: &InstrumentId) -> bool {
        self.state
            .positions
            .get(&(account_id.clone(), instrument_id.clone()))
            .is_none_or(|position| position.net_quantity.units() == 0)
    }

    /// Makes the owner agree with the broker after a (re)attach. Open orders still at the
    /// broker are re-confirmed, which also resolves requests the broker never answered, and
    /// working orders Aeris has not seen are mirrored; open orders it no longer holds are
    /// settled from their deals. Positions the venue disagrees with are then rebased.
    fn apply_snapshot(
        &mut self,
        account_id: &TradingAccountId,
        snapshot: &VenueSnapshot,
    ) -> Result<(), String> {
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
            } else if let Some(mirrored) = self.mirror_open_order(account_id, report)? {
                reconfirmed.insert(mirrored);
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
            let (mut updated, kind, detail) =
                crate::book::lifecycle::broker_missing(order, bound, self.command_now)
                    .map_err(|error| error.to_string())?;
            updated.provenance = self.venue_provenance();
            let provenance = updated.provenance.clone();
            self.record_broker_transition(updated, kind, detail, provenance, None)?;
        }
        self.rebase_positions(account_id, &snapshot.positions)
    }

    /// Rebases every position of the account the venue disagrees with. Positions in
    /// instruments the owner has not registered cannot be accounted and are reported after
    /// the rest are settled.
    fn rebase_positions(
        &mut self,
        account_id: &TradingAccountId,
        reports: &[VenuePosition],
    ) -> Result<(), String> {
        let mut instruments = self
            .state
            .positions
            .values()
            .filter(|position| {
                position.account_id == *account_id && position.net_quantity.units() != 0
            })
            .map(|position| position.instrument_id.clone())
            .collect::<BTreeSet<_>>();
        instruments.extend(reports.iter().map(|report| report.instrument_id.clone()));
        let mut unregistered = Vec::new();
        for instrument_id in instruments {
            if !self.state.instruments.contains_key(&instrument_id) {
                unregistered.push(instrument_id.as_str().to_string());
                continue;
            }
            let report = reports
                .iter()
                .find(|report| report.instrument_id == instrument_id);
            self.rebase_position(account_id, &instrument_id, report)?;
        }
        if unregistered.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the broker reports positions in unregistered instruments: {}",
                unregistered.join(", ")
            ))
        }
    }

    /// Replaces a ledger position the venue disagrees with by the venue's net quantity and
    /// average price. The venue averages fills at instrument prices, so the average times the
    /// quantity is a whole number of price units up to the average's own rounding, and the
    /// nearest unit recovers the exact basis. Realized P&L is kept; the trade cycle's P&L
    /// becomes unknown, because fills Aeris never saw moved it. A position the venue agrees
    /// with only takes its open P&L.
    fn rebase_position(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        report: Option<&VenuePosition>,
    ) -> Result<(), String> {
        let currency_scale = self
            .state
            .accounts
            .get(account_id)
            .ok_or("trading account is not registered")?
            .currency_scale;
        let instrument = self
            .state
            .instruments
            .get(instrument_id)
            .ok_or("trading instrument is not registered")?;
        let (price_scale, quantity_scale) = (instrument.price_scale, instrument.quantity_scale);
        let key = (account_id.clone(), instrument_id.clone());
        let current = self.state.positions.get(&key).cloned();
        let zero_money =
            FixedPoint::try_new(0, currency_scale).map_err(|error| error.to_string())?;
        let net = match report {
            Some(report) => report.net_quantity.exact_rescale(quantity_scale),
            None => FixedPoint::try_new(0, quantity_scale),
        }
        .map_err(|error| error.to_string())?;
        let open_pnl = match report.and_then(|report| report.open_pnl) {
            Some(open_pnl) if net.units() != 0 => open_pnl
                .exact_rescale(currency_scale)
                .map_err(|error| error.to_string())?,
            _ => zero_money,
        };
        let ledger_net = current
            .as_ref()
            .map(|position| position.net_quantity.exact_rescale(quantity_scale))
            .transpose()
            .map_err(|error| error.to_string())?
            .map_or(0, FixedPoint::units);
        if ledger_net == net.units() {
            if let Some(position) = self.state.positions.get_mut(&key)
                && net.units() != 0
                && position.unrealized_pnl != open_pnl
            {
                position.unrealized_pnl = open_pnl;
                self.mark_revision = self.mark_revision.saturating_add(1);
            }
            return Ok(());
        }
        let (basis, average_entry_price) = venue_entry(report, net, price_scale)?;
        let position = Position {
            account_id: account_id.clone(),
            instrument_id: instrument_id.clone(),
            net_quantity: net,
            average_entry_price,
            realized_pnl: current
                .as_ref()
                .map_or(zero_money, |position| position.realized_pnl),
            unrealized_pnl: open_pnl,
            last_fill_unix_nanos: current
                .as_ref()
                .map_or(self.command_now, |position| position.last_fill_unix_nanos),
        };
        // The ledger's open cycle ends here, as a flatten or reversal would end it; the
        // rebased cycle's P&L is unknown because fills Aeris never saw moved it.
        let cycle_id = match self.state.trade_cycles.get(&key) {
            Some(cycle) if ledger_net != 0 => cycle
                .cycle_id
                .checked_add(1)
                .ok_or("trade cycle id overflowed")?,
            Some(cycle) => cycle.cycle_id,
            None => 0,
        };
        let cycle = TradeCycle {
            account_id: account_id.clone(),
            instrument_id: instrument_id.clone(),
            cycle_id,
            peak_quantity: FixedPoint::try_new(
                net.units()
                    .checked_abs()
                    .ok_or("position quantity overflowed")?,
                quantity_scale,
            )
            .map_err(|error| error.to_string())?,
            realized_pnl: zero_money,
            risk_realized_pnl: zero_money,
            pnl_known: false,
        };
        let released = if net.units() == 0 {
            self.ended_brackets(account_id, instrument_id, true, None)
        } else {
            Vec::new()
        };
        self.store
            .rebase_position(&position, basis, &cycle, &released)?;
        for order_id in &released {
            self.state.broker_brackets.remove(order_id);
        }
        match basis {
            Some(basis) => self.state.cost_basis.insert(key.clone(), basis),
            None => self.state.cost_basis.remove(&key),
        };
        self.state.trade_cycles.insert(key.clone(), cycle);
        self.state.positions.insert(key, position);
        self.bump_revision()
    }

    /// The owner order a broker report describes: by its bound broker id, or, before the
    /// first report binds it, by client order id within the same account only.
    fn reported_order(&self, account_id: &TradingAccountId, report: &BrokerOrder) -> Option<Order> {
        if let Some(order_id) = self.state.orders.broker_owner(&report.broker_order_id) {
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
            .by_client_str(client)
            .filter(|order| {
                order.account_id == *account_id && self.broker_order_id(&order.id).is_none()
            })
            .cloned()
    }

    /// Applies one broker order report; a working order Aeris has not seen is mirrored.
    /// `resolve_pending` (a reconcile) treats an unanswered modify or cancel as settled by
    /// the broker's current state.
    fn apply_broker_order(
        &mut self,
        account_id: &TradingAccountId,
        report: &BrokerOrder,
        state: BrokerOrderState,
        reason: Option<&str>,
        resolve_pending: bool,
    ) -> Result<(), String> {
        let Some(current) = self.reported_order(account_id, report) else {
            if matches!(
                state,
                BrokerOrderState::Accepted
                    | BrokerOrderState::Replaced
                    | BrokerOrderState::PartiallyFilled
            ) {
                self.mirror_open_order(account_id, report)?;
            }
            return Ok(());
        };
        let binding = self
            .state
            .orders
            .broker_owner(&report.broker_order_id)
            .is_none();
        if !current.status.is_open() {
            return Ok(());
        }
        if report.quantity.scale() != current.quantity.scale()
            || report.filled_quantity.scale() != current.quantity.scale()
        {
            return Err("broker order quantity scale does not match the order".into());
        }
        let Some((mut updated, kind)) = crate::book::lifecycle::broker_report(
            &current,
            report,
            state,
            resolve_pending,
            binding,
            self.command_now,
        )
        .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        updated.provenance = self.venue_provenance();
        let provenance = updated.provenance.clone();
        let detail = reason.unwrap_or(broker_report_detail(state));
        self.record_broker_transition(
            updated,
            kind,
            detail,
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
            .by_client(client_order_id)
            .filter(|order| order.account_id == *account_id)
            .cloned()
        else {
            return Ok(());
        };
        let Some(mut updated) = crate::book::lifecycle::broker_refusal(current, self.command_now)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        updated.provenance = self.venue_provenance();
        let provenance = updated.provenance.clone();
        self.record_broker_transition(updated, OrderEventKind::Rejected, reason, provenance, None)
    }

    fn venue_provenance(&self) -> TradingProvenance {
        TradingProvenance {
            venue_id: BROKER_VENUE_ID.into(),
            provider_id: BROKER_VENUE_ID.into(),
            session_generation: self.state.venue_generation,
            source_sequence: self.venue_ingestion,
            observed_unix_nanos: self.command_now,
        }
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
        if self.state.orders.get(&updated.id).is_none() {
            return Err("broker order is not registered".into());
        }
        let sequence = self.state.next_sequence;
        let provenance = self.broker_provenance(self.command_now, sequence);
        let event = Self::broker_event(updated, sequence, kind, detail, provenance)?;
        let reservation = outbound
            .try_reserve(request)
            .map_err(|error| outbound_error(&error))?;
        self.store
            .transition_broker_order(updated, &event, sequence + 1, None, &[])?;
        self.state.next_sequence = sequence + 1;
        self.state
            .orders
            .insert(updated.id.clone(), updated.clone());
        self.state.order_events.push_back(event);
        self.state.revision = self.state.revision.saturating_add(1).min(i64::MAX as u64);
        reservation.commit();
        Ok(())
    }

    fn broker_provenance(&self, observed_unix_nanos: i64, sequence: u64) -> TradingProvenance {
        TradingProvenance {
            venue_id: BROKER_VENUE_ID.into(),
            provider_id: BROKER_VENUE_ID.into(),
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
            reason: None,
            event_unix_nanos: provenance.observed_unix_nanos,
            detail: Some(detail.to_string()),
            provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        Ok(event)
    }

    /// Persists one venue-driven order transition. An entry that ends releases its bracket
    /// levels in the same transaction when the venue no longer holds exits for it.
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
        let released = if updated.status.is_open() {
            Vec::new()
        } else {
            self.ended_brackets(
                &updated.account_id,
                &updated.instrument_id,
                self.is_flat(&updated.account_id, &updated.instrument_id),
                Some(&updated),
            )
        };
        self.store
            .transition_broker_order(&updated, &event, sequence + 1, binding, &released)?;
        if let Some(broker_order_id) = binding {
            self.state
                .orders
                .bind_broker(broker_order_id.to_string(), updated.id.clone());
        }
        for order_id in &released {
            self.state.broker_brackets.remove(order_id);
        }
        self.state.next_sequence = sequence + 1;
        self.state.orders.insert(updated.id.clone(), updated);
        self.state.order_events.push_back(event);
        self.bump_revision()
    }
}

/// The exact cost basis and the average entry price of a venue-reported position of `net`.
fn venue_entry(
    report: Option<&VenuePosition>,
    net: FixedPoint,
    price_scale: u8,
) -> Result<(Option<CostBasis>, Option<FixedPoint>), String> {
    if net.units() == 0 {
        return Ok((None, None));
    }
    let reported = report
        .and_then(|report| report.average_price)
        .ok_or("the broker reported an open position without its average price")?;
    let basis = venue_basis(
        reported,
        net,
        price_scale
            .checked_add(net.scale())
            .ok_or("position cost basis scale overflowed")?,
    )?;
    let average =
        aeris_trading::round_divide_i128(basis.units, i128::from(net.units().unsigned_abs()))
            .map_err(|error| error.to_string())?;
    let average = FixedPoint::try_new(
        i64::try_from(average).map_err(|_| "position average overflowed".to_string())?,
        price_scale,
    )
    .map_err(|error| error.to_string())?;
    Ok((Some(basis), Some(average)))
}

/// The exact cost basis of `net` at `average`, at the instrument's combined basis scale,
/// rounded once to the nearest basis unit when the average carries finer decimals.
fn venue_basis(average: FixedPoint, net: FixedPoint, scale: u8) -> Result<CostBasis, String> {
    if average.units() <= 0 {
        return Err("the broker reported a non-positive average price".into());
    }
    let units = i128::from(average.units())
        .checked_mul(i128::from(net.units().unsigned_abs()))
        .ok_or("position cost basis overflowed")?;
    let reported = average
        .scale()
        .checked_add(net.scale())
        .ok_or("position cost basis scale overflowed")?;
    if reported <= scale {
        return CostBasis {
            units,
            scale: reported,
        }
        .rescaled(scale);
    }
    let divisor = 10_i128
        .checked_pow(u32::from(reported - scale))
        .ok_or("position cost basis scale overflowed")?;
    Ok(CostBasis {
        units: aeris_trading::round_divide_i128(units, divisor)
            .map_err(|error| error.to_string())?,
        scale,
    })
}

fn broker_report_detail(state: BrokerOrderState) -> &'static str {
    match state {
        BrokerOrderState::Accepted => "accepted",
        BrokerOrderState::Replaced => "replaced",
        BrokerOrderState::PartiallyFilled => "partially filled",
        BrokerOrderState::Filled => "filled",
        BrokerOrderState::Cancelled => "cancelled",
        BrokerOrderState::Expired => "expired",
        BrokerOrderState::CancelRejected => "cancel rejected",
        BrokerOrderState::Rejected => "rejected",
    }
}

fn outbound_error<T>(error: &TrySendError<T>) -> String {
    match error {
        TrySendError::Full(_) => "broker venue outbound queue is full".into(),
        TrySendError::Disconnected(_) => DEMO_VENUE_UNAVAILABLE.into(),
    }
}
