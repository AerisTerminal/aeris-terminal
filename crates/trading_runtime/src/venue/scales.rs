//! Brings venue reports to the owner's scales. A relay reports each price and quantity at
//! the scale the broker's wire carried it and money at the broker's precision; the owner
//! accounts them at the registered instrument's scales and the account's currency scale.
//! A value converts only when it is preserved exactly, so nothing is rounded.

use crate::TradingCore;
use aeris_trading::{
    FixedPoint, TradingAccountId,
    venue::{BrokerFill, BrokerOrder, VenueSnapshot, VenueUpdate},
};

impl TradingCore {
    /// The update with order and fill prices and quantities at their instrument's scales and
    /// the balance at the account's currency scale. Reports in instruments the owner has not
    /// registered stay as reported; the step that needs the instrument refuses them. Position
    /// reports are converted where they are applied, because their average price may carry
    /// finer decimals than the instrument.
    ///
    /// # Errors
    /// Returns an error when a value cannot be expressed exactly at the owner's scale.
    pub(super) fn owner_scaled(
        &self,
        account_id: &TradingAccountId,
        update: &VenueUpdate,
    ) -> Result<VenueUpdate, String> {
        Ok(match update {
            VenueUpdate::Order {
                order,
                state,
                reason,
            } => VenueUpdate::Order {
                order: self.scaled_order(order)?,
                state: *state,
                reason: reason.clone(),
            },
            VenueUpdate::Fill(fill) => VenueUpdate::Fill(self.scaled_fill(fill)?),
            VenueUpdate::Balance { balance } => {
                let currency_scale = self
                    .state
                    .accounts
                    .get(account_id)
                    .ok_or("trading account is not registered")?
                    .currency_scale;
                VenueUpdate::Balance {
                    balance: exact(*balance, currency_scale, "balance")?,
                }
            }
            VenueUpdate::Snapshot(snapshot) => VenueUpdate::Snapshot(VenueSnapshot {
                orders: snapshot
                    .orders
                    .iter()
                    .map(|order| self.scaled_order(order))
                    .collect::<Result<_, _>>()?,
                positions: snapshot.positions.clone(),
            }),
            VenueUpdate::AccountObserved(_)
            | VenueUpdate::Refused { .. }
            | VenueUpdate::Position(_)
            | VenueUpdate::BracketLevelMoved { .. }
            | VenueUpdate::OperationFailed { .. } => update.clone(),
        })
    }

    fn scaled_order(&self, order: &BrokerOrder) -> Result<BrokerOrder, String> {
        let Some(instrument) = self.state.instruments.get(&order.instrument_id) else {
            return Ok(order.clone());
        };
        let (price, quantity) = (instrument.price_scale, instrument.quantity_scale);
        Ok(BrokerOrder {
            quantity: exact(order.quantity, quantity, "order quantity")?,
            filled_quantity: exact(order.filled_quantity, quantity, "filled quantity")?,
            limit_price: order
                .limit_price
                .map(|value| exact(value, price, "limit price"))
                .transpose()?,
            stop_price: order
                .stop_price
                .map(|value| exact(value, price, "stop price"))
                .transpose()?,
            ..order.clone()
        })
    }

    fn scaled_fill(&self, fill: &BrokerFill) -> Result<BrokerFill, String> {
        let Some(instrument) = self.state.instruments.get(&fill.instrument_id) else {
            return Ok(fill.clone());
        };
        Ok(BrokerFill {
            price: exact(fill.price, instrument.price_scale, "fill price")?,
            quantity: exact(fill.quantity, instrument.quantity_scale, "fill quantity")?,
            ..fill.clone()
        })
    }
}

fn exact(value: FixedPoint, scale: u8, field: &str) -> Result<FixedPoint, String> {
    value
        .exact_rescale(scale)
        .map_err(|error| format!("the broker's {field} does not fit the owner's scale: {error}"))
}
