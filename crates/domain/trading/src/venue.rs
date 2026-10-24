//! Provider-neutral contract between the trading owner and a futures broker venue relay.
//!
//! The trading owner sends [`VenueRequest`]s and applies [`VenueEvent`]s. The relay owns
//! the broker connection, translates both to and from one broker's protocol, and holds
//! no order, fill, position or account state. Accounts are netted: one position per
//! account and instrument. Requests carry prices and quantities at the instrument's
//! scales. Reports carry them fixed-point at any scale that holds the broker's value
//! exactly, and money at the broker's precision; the owner converts both to its own scales
//! only when the value is preserved. Broker identifiers are opaque strings whose ordering
//! carries no meaning.

use crate::{
    ClientOrderId, FixedPoint, OrderSide, OrderType, TimeInForce, TradingValidationError,
    validate_field,
};
use aeris_instruments::InstrumentId;

/// The venue id of accounts registered by the attached broker venue relay.
pub const BROKER_VENUE_ID: &str = "broker";

/// Most target levels and most stop levels one server-held bracket carries.
pub const MAXIMUM_BRACKET_LEVELS: usize = 4;

/// What the attached venue supports beyond placing, modifying and cancelling orders,
/// cancelling all orders and exiting a position, which every venue supports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VenueCapabilities {
    /// Most target levels and most stop levels the venue holds per bracket; zero when the
    /// venue holds no brackets.
    pub bracket_levels: usize,
    /// Longest client order id the venue echoes back unchanged, in bytes.
    pub client_order_id_limit: usize,
}

impl VenueCapabilities {
    /// # Errors
    /// Returns a validation error for more bracket levels than the contract carries or a
    /// zero client order id limit.
    pub const fn validate(self) -> Result<(), TradingValidationError> {
        if self.bracket_levels > MAXIMUM_BRACKET_LEVELS {
            return Err(TradingValidationError::InvalidBracket);
        }
        if self.client_order_id_limit == 0 {
            return Err(TradingValidationError::EmptyField("client_order_id_limit"));
        }
        Ok(())
    }
}

/// The exit side of a bracket level.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BracketSide {
    Target,
    Stop,
}

impl BracketSide {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Target => "target",
            Self::Stop => "stop",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "target" => Some(Self::Target),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }
}

/// One exit level of a server-held bracket: `quantity` of the entry, placed `ticks`
/// minimum price increments from the entry's fill price.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BracketLevel {
    pub quantity: FixedPoint,
    pub ticks: i32,
}

/// Exit levels the venue holds with an entry order and releases as the entry fills, so
/// they keep working while Aeris is disconnected. Each side with levels covers the entry
/// quantity exactly with distinct distances, because a level is addressed by its distance.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServerBracket {
    pub targets: Vec<BracketLevel>,
    pub stops: Vec<BracketLevel>,
}

impl ServerBracket {
    /// The levels on one side.
    #[must_use]
    pub fn side(&self, side: BracketSide) -> &[BracketLevel] {
        match side {
            BracketSide::Target => &self.targets,
            BracketSide::Stop => &self.stops,
        }
    }

    /// Mutable levels on one side.
    pub fn side_mut(&mut self, side: BracketSide) -> &mut Vec<BracketLevel> {
        match side {
            BracketSide::Target => &mut self.targets,
            BracketSide::Stop => &mut self.stops,
        }
    }

    /// Validates the levels against the entry quantity they protect.
    ///
    /// # Errors
    /// Returns [`TradingValidationError::InvalidBracket`] for a bracket without levels, too
    /// many levels, non-positive distances or quantities, repeated distances, or a side
    /// that does not cover the entry quantity exactly.
    pub fn validate(&self, entry_quantity: FixedPoint) -> Result<(), TradingValidationError> {
        if self.targets.is_empty() && self.stops.is_empty() {
            return Err(TradingValidationError::InvalidBracket);
        }
        for levels in [&self.targets, &self.stops] {
            if levels.is_empty() {
                continue;
            }
            if levels.len() > MAXIMUM_BRACKET_LEVELS {
                return Err(TradingValidationError::InvalidBracket);
            }
            let mut covered = 0_i64;
            for (index, level) in levels.iter().enumerate() {
                if level.ticks <= 0
                    || level.quantity.units() <= 0
                    || level.quantity.scale() != entry_quantity.scale()
                    || levels[..index]
                        .iter()
                        .any(|earlier| earlier.ticks == level.ticks)
                {
                    return Err(TradingValidationError::InvalidBracket);
                }
                covered = covered
                    .checked_add(level.quantity.units())
                    .ok_or(TradingValidationError::ArithmeticOverflow)?;
            }
            if covered != entry_quantity.units() {
                return Err(TradingValidationError::InvalidBracket);
            }
        }
        Ok(())
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
    pub bracket: Option<ServerBracket>,
}

/// The complete replacement terms of a working broker order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VenueAmendment {
    pub client_order_id: ClientOrderId,
    pub broker_account: String,
    pub broker_order_id: String,
    pub instrument_id: InstrumentId,
    pub order_type: OrderType,
    pub quantity: FixedPoint,
    pub limit_price: Option<FixedPoint>,
    pub stop_price: Option<FixedPoint>,
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
    /// Moves one level of a bracket the venue still holds, addressed by its current
    /// distance from the entry.
    MoveBracketLevel {
        broker_account: String,
        entry_broker_order_id: String,
        side: BracketSide,
        current_ticks: i32,
        new_ticks: i32,
    },
    /// Cancels every working order of the account, including bracket exits.
    CancelAll {
        broker_account: String,
    },
    /// Flattens the account's position in one instrument at the venue.
    ExitPosition {
        broker_account: String,
        instrument_id: InstrumentId,
    },
    /// Report every open order and position of the account as one snapshot, preceded by
    /// the deals executed since `deals_since_unix_nanos` so fills missed while disconnected
    /// are applied before the snapshot settles open orders and positions.
    Reconcile {
        broker_account: String,
        deals_since_unix_nanos: Option<i64>,
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
            | Self::MoveBracketLevel { broker_account, .. }
            | Self::CancelAll { broker_account }
            | Self::ExitPosition { broker_account, .. }
            | Self::Reconcile { broker_account, .. } => broker_account,
        }
    }

    /// Validates identities, positive amounts and brackets before a request leaves the
    /// owner.
    ///
    /// # Errors
    /// Returns a validation error for blank identities, non-positive values or an invalid
    /// bracket.
    pub fn validate(&self) -> Result<(), TradingValidationError> {
        validate_field("broker_account", self.broker_account())?;
        match self {
            Self::Place(order) => {
                positive(order.quantity)?;
                prices(order.limit_price, order.stop_price)?;
                order
                    .bracket
                    .as_ref()
                    .map_or(Ok(()), |bracket| bracket.validate(order.quantity))
            }
            Self::Amend(amendment) => {
                validate_field("broker_order_id", &amendment.broker_order_id)?;
                positive(amendment.quantity)?;
                prices(amendment.limit_price, amendment.stop_price)
            }
            Self::Cancel {
                broker_order_id, ..
            } => validate_field("broker_order_id", broker_order_id),
            Self::MoveBracketLevel {
                entry_broker_order_id,
                current_ticks,
                new_ticks,
                ..
            } => {
                validate_field("entry_broker_order_id", entry_broker_order_id)?;
                if *current_ticks <= 0 || *new_ticks <= 0 || current_ticks == new_ticks {
                    return Err(TradingValidationError::InvalidBracket);
                }
                Ok(())
            }
            Self::CancelAll { .. } | Self::ExitPosition { .. } => Ok(()),
            Self::Reconcile {
                deals_since_unix_nanos,
                ..
            } => {
                if deals_since_unix_nanos.is_some_and(|since| since <= 0) {
                    return Err(TradingValidationError::InvalidTimestamp);
                }
                Ok(())
            }
        }
    }
}

/// A broker account the relay's connection can trade, as the broker describes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedAccount {
    pub environment: crate::AccountEnvironment,
    pub display_name: String,
    /// The account currency, in which the broker reports balances and open P&L.
    pub currency: String,
    pub currency_scale: u8,
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

/// What kind of order the broker reports, including orders it creates itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrokerOrderKind {
    Market,
    Limit,
    Stop,
    StopLimit,
    /// A broker order type Aeris does not place.
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
}

/// One broker execution (deal). The deal id makes it idempotent across replays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrokerFill {
    pub broker_deal_id: String,
    pub broker_order_id: String,
    /// The client order id the broker echoes on the deal's order, as on [`BrokerOrder`]. A
    /// deal can arrive before the order's first report, so it binds the owner order itself.
    pub client_order_id: Option<String>,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub executed_unix_nanos: i64,
}

/// The account's net position in one instrument as the venue reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VenuePosition {
    pub instrument_id: InstrumentId,
    /// Positive long, negative short, zero flat.
    pub net_quantity: FixedPoint,
    /// The average open price; `None` while flat.
    pub average_price: Option<FixedPoint>,
    /// Open P&L in the account currency; `None` when the venue did not report it.
    pub open_pnl: Option<FixedPoint>,
}

/// Every open order and position of one account at one moment. An instrument without a
/// position entry is flat.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VenueSnapshot {
    pub orders: Vec<BrokerOrder>,
    pub positions: Vec<VenuePosition>,
}

/// A venue request whose outcome the owner must learn even though it names no order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VenueOperation {
    /// A place, amend or cancel of this client order.
    OrderCommand {
        client_order_id: ClientOrderId,
    },
    CancelAll,
    ExitPosition {
        instrument_id: InstrumentId,
    },
    MoveBracketLevel {
        entry_broker_order_id: String,
        side: BracketSide,
        current_ticks: i32,
        new_ticks: i32,
    },
    Reconcile,
}

/// Why a venue operation did not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationFailure {
    /// The broker or the relay refused it; nothing changed at the venue.
    Refused,
    /// No answer arrived within the relay's deadline; the venue state is uncertain until the
    /// account is reconciled. The relay never resubmits such a request.
    Unanswered,
}

/// One change reported by a broker venue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VenueUpdate {
    /// The connection can reach this account; the owner registers it.
    AccountObserved(ObservedAccount),
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
    /// The venue moved a bracket level it still holds.
    BracketLevelMoved {
        entry_broker_order_id: String,
        side: BracketSide,
        from_ticks: i32,
        to_ticks: i32,
    },
    Balance {
        balance: FixedPoint,
    },
    Snapshot(VenueSnapshot),
    /// A request that did not complete. A refused order command is reported as
    /// [`VenueUpdate::Refused`]; every other failure is reported here.
    OperationFailed {
        operation: VenueOperation,
        failure: OperationFailure,
        reason: String,
    },
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

    fn point(units: i64, scale: u8) -> FixedPoint {
        FixedPoint::try_new(units, scale).expect("fixed point")
    }

    fn level(quantity: i64, ticks: i32) -> BracketLevel {
        BracketLevel {
            quantity: point(quantity, 0),
            ticks,
        }
    }

    fn order() -> VenueOrder {
        VenueOrder {
            client_order_id: ClientOrderId::try_new("client-1").expect("id"),
            broker_account: "1001".into(),
            instrument_id: InstrumentId::try_new("broker:demo:1001:1").expect("instrument"),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            quantity: point(3, 0),
            limit_price: None,
            stop_price: None,
            bracket: Some(ServerBracket {
                targets: vec![level(2, 8), level(1, 16)],
                stops: vec![level(3, 12)],
            }),
        }
    }

    #[test]
    fn requests_name_their_account_and_reject_non_positive_values() {
        let place = VenueRequest::Place(order());
        assert_eq!(place.broker_account(), "1001");
        assert!(place.validate().is_ok());
        for bad in [
            VenueRequest::Place(VenueOrder {
                quantity: point(0, 0),
                bracket: None,
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
            VenueRequest::MoveBracketLevel {
                broker_account: "1001".into(),
                entry_broker_order_id: "entry".into(),
                side: BracketSide::Stop,
                current_ticks: 12,
                new_ticks: 12,
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} must be rejected");
        }
        let reconcile = VenueRequest::Reconcile {
            broker_account: "1001".into(),
            deals_since_unix_nanos: Some(1),
        };
        assert_eq!(reconcile.broker_account(), "1001");
        assert!(reconcile.validate().is_ok());
        assert!(
            VenueRequest::Reconcile {
                broker_account: "1001".into(),
                deals_since_unix_nanos: Some(0),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn bracket_sides_cover_the_entry_with_distinct_positive_distances() {
        let entry = point(3, 0);
        let valid = order().bracket.expect("bracket");
        assert!(valid.validate(entry).is_ok());
        assert!(
            ServerBracket {
                targets: Vec::new(),
                stops: vec![level(3, 10)],
            }
            .validate(entry)
            .is_ok()
        );
        for bad in [
            ServerBracket::default(),
            ServerBracket {
                targets: vec![level(2, 8)],
                stops: Vec::new(),
            },
            ServerBracket {
                targets: vec![level(2, 8), level(1, 8)],
                stops: Vec::new(),
            },
            ServerBracket {
                targets: Vec::new(),
                stops: vec![level(3, 0)],
            },
            ServerBracket {
                targets: vec![level(1, 1), level(1, 2), level(0, 3), level(1, 4)],
                stops: Vec::new(),
            },
            ServerBracket {
                targets: (1..=5).map(|ticks| level(1, ticks)).collect(),
                stops: Vec::new(),
            },
        ] {
            assert_eq!(
                bad.validate(entry),
                Err(TradingValidationError::InvalidBracket),
                "{bad:?} must be rejected"
            );
        }
        assert_eq!(
            VenueCapabilities {
                bracket_levels: MAXIMUM_BRACKET_LEVELS + 1,
                client_order_id_limit: 32,
            }
            .validate(),
            Err(TradingValidationError::InvalidBracket)
        );
    }
}
