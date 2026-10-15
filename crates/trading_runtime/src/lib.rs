mod book;
pub mod clock;
mod copier;
mod core;
mod discipline;
mod plan;
mod risk;
mod round_trip;
mod service;
mod store;
mod strategy;
pub mod venue;

#[cfg(test)]
pub(crate) use aeris_instruments::{ContractMetadata, InstrumentId};
#[cfg(test)]
pub(crate) use aeris_trading::{
    AccountEnvironment, OrderEventKind, OrderSide, OrderStatus, OrderType, TimeInForce,
};
pub(crate) use aeris_trading::{
    ClientOrderId, Fill, FixedPoint, Order, TradingAccount, TradingAccountId, TradingProvenance,
};
pub use core::market::feed::MarketFeedInbox;
pub use core::*;
pub(crate) use core::{
    DEMO_VENUE_UNAVAILABLE, MAXIMUM_OPEN_ORDERS, Reply, TradingCore, UNREALIZED_PNL_INTERVAL,
    allocated_target_quantities, check_quantity_increment, instrument_tick_units,
    managed_child_client_order_id, opposite_side, order_increment_units, shifted_price,
};
#[cfg(test)]
pub(crate) use core::{PendingObservations, REPLY_CAPACITY};
pub(crate) use risk::RiskTradeCycleState;
pub(crate) use service::Command;
#[cfg(test)]
pub(crate) use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
pub(crate) use std::{path::PathBuf, sync::mpsc::Receiver};
pub(crate) use venue::VenueRequest;

#[cfg(test)]
mod tests;
