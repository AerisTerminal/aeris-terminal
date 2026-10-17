//! Price-keyed secondary membership for executable orders. The order remains
//! authoritative in `OrderBook`; every mutation rebuilds this membership.

use aeris_instruments::InstrumentId;
use aeris_trading::{Order, OrderId, OrderSide, OrderType};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
pub(super) struct TriggerIndex {
    instruments: BTreeMap<InstrumentId, Sides>,
}

#[derive(Clone, Default)]
struct Sides {
    market: BTreeSet<OrderId>,
    buy_stops: BTreeMap<i64, BTreeSet<OrderId>>,
    sell_stops: BTreeMap<i64, BTreeSet<OrderId>>,
    buy_limits: BTreeMap<i64, BTreeSet<OrderId>>,
    sell_limits: BTreeMap<i64, BTreeSet<OrderId>>,
}

#[derive(Clone, Copy)]
enum Key {
    Market,
    BuyStop(i64),
    SellStop(i64),
    BuyLimit(i64),
    SellLimit(i64),
}

impl Key {
    fn of(order: &Order) -> Option<Self> {
        if !order.status.is_executable() {
            return None;
        }
        let resting =
            order.order_type == OrderType::StopLimit && order.triggered_unix_nanos.is_some();
        match (order.side, order.order_type) {
            (_, OrderType::Market) => Some(Self::Market),
            (OrderSide::Buy, OrderType::StopLimit) if resting => {
                Some(Self::BuyLimit(order.limit_price?.units()))
            }
            (OrderSide::Sell, OrderType::StopLimit) if resting => {
                Some(Self::SellLimit(order.limit_price?.units()))
            }
            (OrderSide::Buy, OrderType::Stop | OrderType::StopLimit) => {
                Some(Self::BuyStop(order.stop_price?.units()))
            }
            (OrderSide::Sell, OrderType::Stop | OrderType::StopLimit) => {
                Some(Self::SellStop(order.stop_price?.units()))
            }
            (OrderSide::Buy, OrderType::Limit) => Some(Self::BuyLimit(order.limit_price?.units())),
            (OrderSide::Sell, OrderType::Limit) => {
                Some(Self::SellLimit(order.limit_price?.units()))
            }
            (_, OrderType::RestingAt) => None,
        }
    }
}

impl Sides {
    fn map(&mut self, key: Key) -> Option<(&mut BTreeMap<i64, BTreeSet<OrderId>>, i64)> {
        match key {
            Key::Market => None,
            Key::BuyStop(price) => Some((&mut self.buy_stops, price)),
            Key::SellStop(price) => Some((&mut self.sell_stops, price)),
            Key::BuyLimit(price) => Some((&mut self.buy_limits, price)),
            Key::SellLimit(price) => Some((&mut self.sell_limits, price)),
        }
    }

    fn is_empty(&self) -> bool {
        self.market.is_empty()
            && self.buy_stops.is_empty()
            && self.sell_stops.is_empty()
            && self.buy_limits.is_empty()
            && self.sell_limits.is_empty()
    }
}

impl TriggerIndex {
    pub(super) fn insert(&mut self, order: &Order) {
        let Some(key) = Key::of(order) else {
            return;
        };
        let sides = self
            .instruments
            .entry(order.instrument_id.clone())
            .or_default();
        if let Some((map, price)) = sides.map(key) {
            map.entry(price).or_default().insert(order.id.clone());
        } else {
            sides.market.insert(order.id.clone());
        }
    }

    pub(super) fn remove(&mut self, order: &Order) {
        let Some(key) = Key::of(order) else {
            return;
        };
        let Some(sides) = self.instruments.get_mut(&order.instrument_id) else {
            return;
        };
        if let Some((map, price)) = sides.map(key) {
            if let Some(ids) = map.get_mut(&price) {
                ids.remove(&order.id);
                if ids.is_empty() {
                    map.remove(&price);
                }
            }
        } else {
            sides.market.remove(&order.id);
        }
        if sides.is_empty() {
            self.instruments.remove(&order.instrument_id);
        }
    }

    pub(super) fn crossing(
        &self,
        instrument: &InstrumentId,
        bid: i64,
        ask: i64,
    ) -> BTreeSet<OrderId> {
        let Some(sides) = self.instruments.get(instrument) else {
            return BTreeSet::new();
        };
        let mut ids = sides.market.clone();
        for (_, members) in sides.buy_stops.range(..=ask) {
            ids.extend(members.iter().cloned());
        }
        for (_, members) in sides.sell_stops.range(bid..) {
            ids.extend(members.iter().cloned());
        }
        for (_, members) in sides.buy_limits.range(ask..) {
            ids.extend(members.iter().cloned());
        }
        for (_, members) in sides.sell_limits.range(..=bid) {
            ids.extend(members.iter().cloned());
        }
        ids
    }
}
