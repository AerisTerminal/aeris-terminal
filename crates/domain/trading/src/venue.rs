//! Provider-neutral contract between the trading owner and a broker venue relay.
//!
//! The trading owner sends [`VenueRequest`]s and applies [`VenueEvent`]s. The relay owns
//! the broker connection, translates both to and from one broker's protocol, and holds
//! no order, fill, position or account state. Prices and quantities are fixed-point at
//! the instrument's scales and money at the broker's reported scale; broker identifiers
//! are opaque strings whose ordering carries no meaning.

use crate::{
    ClientOrderId, FixedPoint, OrderSide, OrderType, TimeInForce, TradingValidationError,
    validate_field,
};
use aeris_instruments::InstrumentId;

/// A stop-loss or take-profit level: an absolute price, or a distance from the entry
/// price, both at the instrument's price scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protection {
    Price(FixedPoint),
    Distance(FixedPoint),
}

impl Protection {
    fn validate(self) -> Result<(), TradingValidationError> {
        match self {
            Self::Price(value) | Self::Distance(value) if value.units() <= 0 => {
                Err(TradingValidationError::NonPositivePrice)
            }
            _ => Ok(()),
        }
    }
}

/// A new order for one broker account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VenueOrder {
    pub client_order_id: ClientOrderId,
    pub broker_account: String,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub quantity: FixedPoint,
    pub limit_price: Option<FixedPoint>,
    pub stop_price: Option<FixedPoint>,
    pub stop_loss: Option<Protection>,
    pub take_profit: Option<Protection>,
    /// The broker position this order adds to or reduces, on a hedged account.
    pub broker_position_id: Option<String>,
}

/// A change to a working broker order; only the fields that are `Some` change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VenueAmendment {
    pub client_order_id: ClientOrderId,
    pub broker_account: String,
    pub broker_order_id: String,
    pub instrument_id: InstrumentId,
    pub quantity: Option<FixedPoint>,
    pub limit_price: Option<FixedPoint>,
    pub stop_price: Option<FixedPoint>,
    pub stop_loss: Option<Protection>,
    pub take_profit: Option<Protection>,
}

/// One request from the trading owner to a broker venue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VenueRequest {
    Place(VenueOrder),
    Amend(VenueAmendment),
    Cancel {
        client_order_id: ClientOrderId,
        broker_account: String,
        broker_order_id: String,
    },
    /// Close `quantity` of one broker position.
    ClosePosition {
        broker_account: String,
        instrument_id: InstrumentId,
        broker_position_id: String,
        quantity: FixedPoint,
    },
    /// Set a position's complete protection; a level left `None` is removed.
    AmendPositionProtection {
        broker_account: String,
        instrument_id: InstrumentId,
        broker_position_id: String,
        stop_loss: Option<FixedPoint>,
        take_profit: Option<FixedPoint>,
    },
    /// Report every open order and position of the account as one snapshot.
    Reconcile {
        broker_account: String,
    },
}

impl VenueRequest {
    /// The broker account the request acts on.
    #[must_use]
    pub fn broker_account(&self) -> &str {
        match self {
            Self::Place(order) => &order.broker_account,
            Self::Amend(amendment) => &amendment.broker_account,
            Self::Cancel { broker_account, .. }
            | Self::ClosePosition { broker_account, .. }
            | Self::AmendPositionProtection { broker_account, .. }
            | Self::Reconcile { broker_account } => broker_account,
        }
    }

    /// Validates identities, positive amounts and protection before a request leaves the
    /// owner.
    ///
    /// # Errors
    /// Returns a validation error for blank identities or non-positive values.
    pub fn validate(&self) -> Result<(), TradingValidationError> {
        validate_field("broker_account", self.broker_account())?;
        match self {
            Self::Place(order) => {
                positive(order.quantity)?;
                prices(order.limit_price, order.stop_price)?;
                protections(order.stop_loss, order.take_profit)?;
                if let Some(position) = &order.broker_position_id {
                    validate_field("broker_position_id", position)?;
                }
                Ok(())
            }
            Self::Amend(amendment) => {
                validate_field("broker_order_id", &amendment.broker_order_id)?;
                if let Some(quantity) = amendment.quantity {
                    positive(quantity)?;
                }
                prices(amendment.limit_price, amendment.stop_price)?;
                protections(amendment.stop_loss, amendment.take_profit)
            }
            Self::Cancel {
                broker_order_id, ..
            } => validate_field("broker_order_id", broker_order_id),
            Self::ClosePosition {
                broker_position_id,
                quantity,
                ..
            } => {
                validate_field("broker_position_id", broker_position_id)?;
                positive(*quantity)
            }
            Self::AmendPositionProtection {
                broker_position_id,
                stop_loss,
                take_profit,
                ..
            } => {
                validate_field("broker_position_id", broker_position_id)?;
                prices(*stop_loss, *take_profit)
            }
            Self::Reconcile { .. } => Ok(()),
        }
    }
}

const fn positive(quantity: FixedPoint) -> Result<(), TradingValidationError> {
    if quantity.units() <= 0 {
        return Err(TradingValidationError::NonPositiveQuantity);
    }
    Ok(())
}

fn prices(
    first: Option<FixedPoint>,
    second: Option<FixedPoint>,
) -> Result<(), TradingValidationError> {
    if first
        .into_iter()
        .chain(second)
        .any(|price| price.units() <= 0)
    {
        return Err(TradingValidationError::NonPositivePrice);
    }
    Ok(())
}

fn protections(
    stop_loss: Option<Protection>,
    take_profit: Option<Protection>,
) -> Result<(), TradingValidationError> {
    stop_loss
        .into_iter()
        .chain(take_profit)
        .try_for_each(Protection::validate)
}

/// What kind of order the broker reports, including orders it creates itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrokerOrderKind {
    Market,
    Limit,
    Stop,
    StopLimit,
    /// A server-created stop-loss/take-profit order that closes its position.
    Protection,
    /// A broker order type Aeris does not place (for example a market-range order).
    Other,
}

/// The order state an execution report moves to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrokerOrderState {
    Accepted,
    Replaced,
    PartiallyFilled,
    Filled,
    Cancelled,
    CancelRejected,
    Expired,
    Rejected,
}

/// One broker order as reported, identified by the broker's order id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrokerOrder {
    pub broker_order_id: String,
    /// The client order id the broker echoes; brokers also assign one to orders they
    /// create, so it identifies an owner order only together with the account.
    pub client_order_id: Option<String>,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub kind: BrokerOrderKind,
    pub quantity: FixedPoint,
    pub filled_quantity: FixedPoint,
    pub limit_price: Option<FixedPoint>,
    pub stop_price: Option<FixedPoint>,
    pub broker_position_id: Option<String>,
    /// The order reduces or closes its position.
    pub closing: bool,
}

/// What a closing fill realized, in the account's money scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealizedClose {
    pub gross_profit: FixedPoint,
    pub swap: FixedPoint,
    pub commission: FixedPoint,
    pub balance: FixedPoint,
}

/// One broker execution (deal). The deal id makes it idempotent across replays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrokerFill {
    pub broker_deal_id: String,
    pub broker_order_id: String,
    pub broker_position_id: String,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub executed_unix_nanos: i64,
    pub commission: Option<FixedPoint>,
    pub realized: Option<RealizedClose>,
}

/// One open broker position as reported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VenuePosition {
    pub broker_position_id: String,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub quantity: FixedPoint,
    /// `None` while the position has no fills.
    pub entry_price: Option<FixedPoint>,
    pub stop_loss: Option<FixedPoint>,
    pub take_profit: Option<FixedPoint>,
    pub swap: FixedPoint,
    pub commission: FixedPoint,
    pub opened_unix_nanos: i64,
}

/// Every open order and position of one account at one moment.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VenueSnapshot {
    pub orders: Vec<BrokerOrder>,
    pub positions: Vec<VenuePosition>,
}

/// One change reported by a broker venue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VenueUpdate {
    Order {
        order: BrokerOrder,
        state: BrokerOrderState,
        reason: Option<String>,
    },
    /// A request the broker refused before creating an order.
    Refused {
        client_order_id: ClientOrderId,
        reason: String,
    },
    Fill(BrokerFill),
    Position(VenuePosition),
    PositionClosed {
        broker_position_id: String,
    },
    Balance {
        balance: FixedPoint,
    },
    Snapshot(VenueSnapshot),
}

/// One update for one broker account, stamped with the relay session that saw it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VenueEvent {
    pub session_generation: u64,
    pub broker_account: String,
    pub update: VenueUpdate,
    pub observed_unix_nanos: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(units: i64) -> FixedPoint {
        FixedPoint::try_new(units, 2).expect("fixed point")
    }

    fn order() -> VenueOrder {
        VenueOrder {
            client_order_id: ClientOrderId::try_new("client-1").expect("id"),
            broker_account: "1001".into(),
            instrument_id: InstrumentId::try_new("ctrader:demo:1001:1").expect("instrument"),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            quantity: point(100_000),
            limit_price: None,
            stop_price: None,
            stop_loss: Some(Protection::Distance(point(50))),
            take_profit: None,
            broker_position_id: None,
        }
    }

    #[test]
    fn requests_name_their_account_and_reject_non_positive_values() {
        let place = VenueRequest::Place(order());
        assert_eq!(place.broker_account(), "1001");
        assert!(place.validate().is_ok());
        for bad in [
            VenueRequest::Place(VenueOrder {
                quantity: point(0),
                ..order()
            }),
            VenueRequest::Place(VenueOrder {
                stop_loss: Some(Protection::Distance(point(0))),
                ..order()
            }),
            VenueRequest::Place(VenueOrder {
                broker_account: " ".into(),
                ..order()
            }),
            VenueRequest::Cancel {
                client_order_id: order().client_order_id,
                broker_account: "1001".into(),
                broker_order_id: String::new(),
            },
            VenueRequest::ClosePosition {
                broker_account: "1001".into(),
                instrument_id: order().instrument_id,
                broker_position_id: "77".into(),
                quantity: point(0),
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} must be rejected");
        }
        let reconcile = VenueRequest::Reconcile {
            broker_account: "1001".into(),
        };
        assert_eq!(reconcile.broker_account(), "1001");
        assert!(reconcile.validate().is_ok());
    }
}
