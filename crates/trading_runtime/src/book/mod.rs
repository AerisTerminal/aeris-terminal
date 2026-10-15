//! Owner-maintained order indexes; mutations go through this type so all views stay in sync.

use aeris_instruments::InstrumentId;
use aeris_trading::{ClientOrderId, Order, OrderId, TradingAccountId};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
};

#[derive(Default)]
pub(crate) struct OrderBook {
    orders: BTreeMap<OrderId, Order>,
    by_client: BTreeMap<ClientOrderId, OrderId>,
    by_broker: BTreeMap<String, OrderId>,
    broker_by_order: BTreeMap<OrderId, String>,
    open: BTreeSet<OrderId>,
    open_by_account_instrument: BTreeMap<(TradingAccountId, InstrumentId), BTreeSet<OrderId>>,
    executable_by_instrument: BTreeMap<InstrumentId, BTreeSet<OrderId>>,
}

impl OrderBook {
    pub(crate) fn from_stored(
        orders: BTreeMap<OrderId, Order>,
        broker_ids: BTreeMap<String, OrderId>,
    ) -> Self {
        let mut book = Self::default();
        for (id, order) in orders {
            book.insert(id, order);
        }
        for (broker, id) in broker_ids {
            book.bind_broker(broker, id);
        }
        book
    }

    pub(crate) fn insert(&mut self, id: OrderId, order: Order) -> Option<Order> {
        // Replacements may change status or instrument. Retire all old membership first.
        let broker = self.broker_by_order.get(&id).cloned();
        let previous = self.remove(&id);
        self.by_client
            .insert(order.client_order_id.clone(), id.clone());
        if order.status.is_open() {
            self.open.insert(id.clone());
            self.open_by_account_instrument
                .entry((order.account_id.clone(), order.instrument_id.clone()))
                .or_default()
                .insert(id.clone());
        }
        if order.status.is_executable() {
            self.executable_by_instrument
                .entry(order.instrument_id.clone())
                .or_default()
                .insert(id.clone());
        }
        self.orders.insert(id.clone(), order);
        if let Some(broker) = broker {
            self.bind_broker(broker, id);
        }
        previous
    }

    pub(crate) fn remove(&mut self, id: &OrderId) -> Option<Order> {
        let order = self.orders.remove(id)?;
        self.by_client.remove(&order.client_order_id);
        self.open.remove(id);
        let key = (order.account_id.clone(), order.instrument_id.clone());
        if let Some(ids) = self.open_by_account_instrument.get_mut(&key) {
            ids.remove(id);
            if ids.is_empty() {
                self.open_by_account_instrument.remove(&key);
            }
        }
        if let Some(ids) = self.executable_by_instrument.get_mut(&order.instrument_id) {
            ids.remove(id);
            if ids.is_empty() {
                self.executable_by_instrument.remove(&order.instrument_id);
            }
        }
        if let Some(broker) = self.broker_by_order.remove(id) {
            self.by_broker.remove(&broker);
        }
        Some(order)
    }

    pub(crate) fn remove_account(&mut self, account: &TradingAccountId) {
        let ids: Vec<_> = self
            .orders
            .iter()
            .filter(|(_, order)| &order.account_id == account)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.remove(&id);
        }
    }

    pub(crate) fn by_client(&self, id: &ClientOrderId) -> Option<&Order> {
        self.by_client.get(id).and_then(|key| self.orders.get(key))
    }

    pub(crate) fn by_client_str(&self, id: &str) -> Option<&Order> {
        self.by_client(&ClientOrderId::try_new(id.to_owned()).ok()?)
    }

    pub(crate) fn broker_owner(&self, broker: &str) -> Option<&OrderId> {
        self.by_broker.get(broker)
    }

    pub(crate) fn broker_for_order(&self, id: &OrderId) -> Option<&str> {
        self.broker_by_order.get(id).map(String::as_str)
    }

    pub(crate) fn bind_broker(&mut self, broker: String, id: OrderId) {
        debug_assert!(self.orders.contains_key(&id));
        if let Some(old) = self.by_broker.remove(&broker) {
            self.broker_by_order.remove(&old);
        }
        if let Some(old) = self.broker_by_order.remove(&id) {
            self.by_broker.remove(&old);
        }
        self.by_broker.insert(broker.clone(), id.clone());
        self.broker_by_order.insert(id, broker);
    }

    pub(crate) fn open_count(&self) -> usize {
        self.open.len()
    }

    pub(crate) fn open_orders(&self) -> impl Iterator<Item = &Order> {
        self.open.iter().filter_map(|id| self.orders.get(id))
    }

    pub(crate) fn open_for(
        &self,
        account: &TradingAccountId,
        instrument: &InstrumentId,
    ) -> impl Iterator<Item = &Order> {
        self.open_by_account_instrument
            .get(&(account.clone(), instrument.clone()))
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.orders.get(id))
    }

    pub(crate) fn open_for_account(&self, account: &TradingAccountId) -> Vec<&Order> {
        let ids: BTreeSet<_> = self
            .open_by_account_instrument
            .keys()
            .filter(|(owner, _)| owner == account)
            .flat_map(|(_, instrument)| self.open_for(account, instrument))
            .map(|order| &order.id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.orders.get(id))
            .collect()
    }

    pub(crate) fn executable_for(&self, instrument: &InstrumentId) -> impl Iterator<Item = &Order> {
        self.executable_by_instrument
            .get(instrument)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.orders.get(id))
    }
}

// Read-only map access keeps existing projections intact without exposing mutable bypasses.
impl Deref for OrderBook {
    type Target = BTreeMap<OrderId, Order>;

    fn deref(&self) -> &Self::Target {
        &self.orders
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_trading::{
        FixedPoint, OrderSide, OrderStatus, OrderType, TimeInForce, TradingProvenance,
    };

    fn order(number: u32, status: OrderStatus) -> Order {
        Order {
            id: OrderId::try_new(format!("order-{number}")).unwrap(),
            client_order_id: ClientOrderId::try_new(format!("client-{number}")).unwrap(),
            account_id: TradingAccountId::try_new(format!("account-{}", number % 3)).unwrap(),
            instrument_id: InstrumentId::try_new(format!("instrument-{}", number % 5)).unwrap(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GoodTillCancelled,
            quantity: FixedPoint::try_new(1, 0).unwrap(),
            filled_quantity: FixedPoint::try_new(0, 0).unwrap(),
            limit_price: Some(FixedPoint::try_new(10, 0).unwrap()),
            stop_price: None,
            status,
            submitted_unix_nanos: 1,
            provenance: TradingProvenance {
                venue_id: "test".into(),
                provider_id: "test".into(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: 1,
            },
        }
    }

    impl OrderBook {
        fn assert_indexes_match_recomputation(&self, broker_bindings: &BTreeMap<String, OrderId>) {
            let mut client = BTreeMap::new();
            let mut open = BTreeSet::new();
            let mut scoped = BTreeMap::<_, BTreeSet<_>>::new();
            let mut executable = BTreeMap::<_, BTreeSet<_>>::new();
            for (id, order) in &self.orders {
                client.insert(order.client_order_id.clone(), id.clone());
                if order.status.is_open() {
                    open.insert(id.clone());
                    scoped
                        .entry((order.account_id.clone(), order.instrument_id.clone()))
                        .or_default()
                        .insert(id.clone());
                }
                if order.status.is_executable() {
                    executable
                        .entry(order.instrument_id.clone())
                        .or_default()
                        .insert(id.clone());
                }
            }
            assert_eq!(self.by_client, client);
            assert_eq!(self.open, open);
            assert_eq!(self.open_by_account_instrument, scoped);
            assert_eq!(self.executable_by_instrument, executable);
            assert_eq!(&self.by_broker, broker_bindings);
            let reverse: BTreeMap<_, _> = broker_bindings
                .iter()
                .map(|(broker, id)| (id.clone(), broker.clone()))
                .collect();
            assert_eq!(self.broker_by_order, reverse);
            assert!(reverse.keys().all(|id| self.orders.contains_key(id)));
        }
    }

    #[test]
    fn seeded_operations_keep_every_order_index_consistent() {
        let mut book = OrderBook::default();
        let mut broker_bindings = BTreeMap::new();
        let mut seed = 0x9e37_79b9_u64;
        for step in 0..4_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let number = (seed % 120) as u32;
            let id = OrderId::try_new(format!("order-{number}")).unwrap();
            match seed % 4 {
                0 => {
                    book.remove(&id);
                    broker_bindings.retain(|_, owner| owner != &id);
                }
                1 => {
                    if book.get(&id).is_some() {
                        let broker = format!("broker-{number}");
                        book.bind_broker(broker.clone(), id.clone());
                        broker_bindings.insert(broker, id.clone());
                    }
                }
                _ => {
                    let status = if step % 3 == 0 {
                        OrderStatus::Filled
                    } else if step % 2 == 0 {
                        OrderStatus::Pending
                    } else {
                        OrderStatus::Working
                    };
                    let mut replacement = order(number, status);
                    if step % 7 == 0 {
                        replacement.instrument_id =
                            InstrumentId::try_new(format!("instrument-{}", (number + 1) % 5))
                                .unwrap();
                    }
                    book.insert(id, replacement);
                }
            }
            book.assert_indexes_match_recomputation(&broker_bindings);
        }
    }

    /// Run explicitly with `cargo test -p aeris_trading_runtime --release --locked
    /// -- book::tests::benchmark_50k_order_lookup --ignored --nocapture`.
    #[test]
    #[ignore = "manual performance measurement"]
    fn benchmark_50k_order_lookup() {
        use std::{hint::black_box, time::Instant};

        let mut book = OrderBook::default();
        for number in 0..50_000 {
            let order = order(number, OrderStatus::Working);
            book.insert(order.id.clone(), order);
        }
        let keys: Vec<_> = (0..1_000)
            .map(|number| ClientOrderId::try_new(format!("client-{}", 49_000 + number)).unwrap())
            .collect();
        let before = Instant::now();
        for key in &keys {
            black_box(
                book.values()
                    .find(|order| order.client_order_id == *black_box(key)),
            );
        }
        let scan = before.elapsed();
        let before = Instant::now();
        for key in &keys {
            black_box(book.by_client(black_box(key)));
        }
        let indexed = before.elapsed();
        println!(
            "50k orders, 1000 client lookups: before scan {:.3} ms; after index {:.3} ms",
            scan.as_secs_f64() * 1000.0,
            indexed.as_secs_f64() * 1000.0
        );
    }
}
