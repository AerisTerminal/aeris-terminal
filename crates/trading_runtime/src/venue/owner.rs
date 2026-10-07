//! Broker order state is persisted and advanced only by the trading owner.

use super::{VenueEvent, VenueRequest, VenueUpdate};
use crate::{
    Coordinator, DEMO_VENUE_UNAVAILABLE, MAXIMUM_OPEN_ORDERS, ModifyOrder, PlaceOrder,
    TradingProvenance, current_unix_nanos,
};
use aeris_trading::{
    FixedPoint, Order, OrderEvent, OrderEventId, OrderEventKind, OrderId, OrderStatus,
};
use std::sync::mpsc::TrySendError;

impl Coordinator {
    pub(crate) fn place_broker_order(&mut self, command: &PlaceOrder) -> Result<Order, String> {
        let outbound = self.venue_outbound.as_ref().ok_or(DEMO_VENUE_UNAVAILABLE)?;
        if command.client_order_id.as_str().len() > 50 {
            return Err("cTrader ClientOrderId must be at most 50 characters".into());
        }
        if !self.state.instruments.contains_key(&command.instrument_id) {
            return Err("trading instrument is not registered".into());
        }
        let instrument = &self.state.instruments[&command.instrument_id];
        if command.quantity.scale() != instrument.quantity_scale
            || command
                .limit_price
                .into_iter()
                .chain(command.stop_price)
                .any(|price| price.scale() != instrument.price_scale)
        {
            return Err("order scales do not match the instrument".into());
        }
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
        // Validate and persist before making a request visible to the writer.
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
            time_in_force: command.time_in_force,
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
        self.store.insert_order(&order, &event, sequence + 1)?;
        match outbound.try_send(VenueRequest::Place(command.clone())) {
            Ok(()) => {
                self.state.next_sequence = sequence + 1;
                self.state.orders.insert(order.id.clone(), order.clone());
                self.state.order_events.push_back(event);
                self.bump_revision()?;
                Ok(order)
            }
            Err(error) => {
                self.store.reject_unsent_broker_order(&order, sequence)?;
                Err(match error {
                    TrySendError::Full(_) => "cTrader venue outbound queue is full",
                    TrySendError::Disconnected(_) => DEMO_VENUE_UNAVAILABLE,
                }
                .into())
            }
        }
    }

    pub(crate) fn modify_broker_order(
        &mut self,
        order: Order,
        command: ModifyOrder,
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
            time_in_force: command.time_in_force,
            limit_price: command.limit_price,
            stop_price: command.stop_price,
            ..order.clone()
        };
        candidate.validate().map_err(|error| error.to_string())?;
        let pending = Order {
            status: OrderStatus::PendingModify,
            ..order
        };
        self.queue_broker_transition(
            &pending,
            OrderEventKind::Modified,
            "modify requested",
            VenueRequest::Modify(command.clone()),
        )?;
        self.pending_modifications
            .insert(command.client_order_id.clone(), command);
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
        let request = VenueRequest::Cancel(order.client_order_id.clone());
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

    pub(crate) fn apply_venue_event(&mut self, event: &VenueEvent) -> Result<(), String> {
        if event.session_generation != self.venue_generation || self.venue_generation == 0 {
            return Ok(());
        }
        self.venue_ingestion = self
            .venue_ingestion
            .checked_add(1)
            .ok_or("venue ingestion sequence exhausted")?;
        let Some(order) = self
            .state
            .orders
            .values()
            .find(|order| order.client_order_id == event.client_order_id)
            .cloned()
        else {
            return Ok(());
        };
        if !order.status.is_open() {
            return Ok(());
        }
        let (status, kind, detail) = match &event.update {
            VenueUpdate::Accepted => (
                OrderStatus::Working,
                OrderEventKind::Accepted,
                "accepted".to_string(),
            ),
            VenueUpdate::Replaced if order.status == OrderStatus::PendingModify => (
                OrderStatus::Working,
                OrderEventKind::Modified,
                "replaced".to_string(),
            ),
            VenueUpdate::Cancelled => (
                OrderStatus::Cancelled,
                OrderEventKind::Cancelled,
                "cancelled".to_string(),
            ),
            VenueUpdate::CancelRejected(reason) if order.status == OrderStatus::PendingCancel => (
                if order.filled_quantity.units() == 0 {
                    OrderStatus::Working
                } else {
                    OrderStatus::PartiallyFilled
                },
                OrderEventKind::Rejected,
                reason.clone(),
            ),
            VenueUpdate::Expired => (
                OrderStatus::Cancelled,
                OrderEventKind::Cancelled,
                "expired".to_string(),
            ),
            VenueUpdate::Rejected(reason) => (
                OrderStatus::Rejected,
                OrderEventKind::Rejected,
                reason.clone(),
            ),
            _ => return Ok(()),
        };
        let mut updated = Order { status, ..order };
        if matches!(event.update, VenueUpdate::Replaced)
            && let Some(modification) = self.pending_modifications.get(&event.client_order_id)
        {
            updated.time_in_force = modification.time_in_force;
            updated.limit_price = modification.limit_price;
            updated.stop_price = modification.stop_price;
        }
        let provenance = TradingProvenance {
            venue_id: "ctrader".into(),
            provider_id: "ctrader".into(),
            session_generation: self.venue_generation,
            source_sequence: self.venue_ingestion,
            observed_unix_nanos: current_unix_nanos()?,
        };
        updated.provenance = provenance.clone();
        self.record_broker_transition(updated, kind, &detail, provenance)?;
        if matches!(
            event.update,
            VenueUpdate::Replaced | VenueUpdate::Rejected(_)
        ) || matches!(status, OrderStatus::Cancelled | OrderStatus::Rejected)
        {
            self.pending_modifications.remove(&event.client_order_id);
        }
        Ok(())
    }

    fn queue_broker_transition(
        &mut self,
        updated: &Order,
        kind: OrderEventKind,
        detail: &str,
        request: VenueRequest,
    ) -> Result<(), String> {
        updated.validate().map_err(|error| error.to_string())?;
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
            .transition_broker_order(updated, &event, sequence + 1)?;
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
            session_generation: self.venue_generation,
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
    ) -> Result<(), String> {
        updated.validate().map_err(|error| error.to_string())?;
        let sequence = self.state.next_sequence;
        let event = Self::broker_event(&updated, sequence, kind, detail, provenance)?;
        self.store
            .transition_broker_order(&updated, &event, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.orders.insert(updated.id.clone(), updated);
        self.state.order_events.push_back(event);
        self.bump_revision()
    }
}

fn outbound_error<T>(error: &TrySendError<T>) -> String {
    match error {
        TrySendError::Full(_) => "cTrader venue outbound queue is full".into(),
        TrySendError::Disconnected(_) => DEMO_VENUE_UNAVAILABLE.into(),
    }
}
