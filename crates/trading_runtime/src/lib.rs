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
    AccountEnvironment, Fill, OrderEventKind, OrderSide, OrderStatus, OrderType, TimeInForce,
};
pub(crate) use aeris_trading::{
    ClientOrderId, FixedPoint, Order, TradingAccount, TradingAccountId, TradingProvenance,
};
#[cfg(test)]
pub(crate) use core::REPLY_CAPACITY;
pub use core::market::feed::MarketFeedInbox;
pub use core::*;
pub(crate) use core::{
    DEMO_VENUE_UNAVAILABLE, MAXIMUM_OPEN_ORDERS, Reply, TradingCore, UNREALIZED_PNL_INTERVAL,
    allocated_target_quantities, check_quantity_increment, instrument_tick_units,
    managed_child_client_order_id, opposite_side, order_increment_units, shifted_price,
};
pub(crate) use service::Command;
pub(crate) use std::path::PathBuf;
#[cfg(test)]
pub(crate) use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak, atomic::AtomicBool, mpsc},
    time::Duration,
};

#[cfg(test)]
mod tests;
