use super::{
    BTreeMap, BTreeSet, Coordinator, MarketStream, OrderLevelSubscription, ProviderOrderBook,
    StreamRequirements,
};
use aeris_observability::diagnostic;

type OrderBookIdentity = (String, String, String);

/// Canonical book identities current demand needs, by what it needs them for.
#[derive(Default)]
struct OrderBookDemand {
    required: BTreeSet<OrderBookIdentity>,
    depth: BTreeSet<OrderBookIdentity>,
    order_levels: BTreeSet<OrderBookIdentity>,
}

impl OrderBookDemand {
    fn insert(&mut self, identity: OrderBookIdentity, streams: StreamRequirements) {
        if streams.contains(MarketStream::Depth) {
            self.depth.insert(identity.clone());
        }
        if streams.contains(MarketStream::OrderLevels) {
            self.order_levels.insert(identity.clone());
        }
        if [
            MarketStream::Depth,
            MarketStream::OrderLevels,
            MarketStream::Trades,
            MarketStream::Quotes,
        ]
        .into_iter()
        .any(|stream| streams.contains(stream))
        {
            self.required.insert(identity);
        }
    }
}

impl Coordinator<'_> {
    pub(in super::super) fn reconcile_order_books(&mut self) {
        let demand = self.required_order_book_identities();
        let mut retained_keys = self
            .engine
            .instrument_subscriptions()
            .map(|(key, _)| key)
            .collect::<BTreeSet<_>>();
        if self.trading_feed.attached() {
            for (series, status) in self.engine.subscriptions() {
                if status.streams.contains(MarketStream::Quotes)
                    && let Ok(instrument) =
                        aeris_instruments::InstrumentId::try_new(series.instrument_id)
                {
                    retained_keys.insert(super::super::InstrumentStreamKey {
                        provider: series.provider_id,
                        instrument,
                        entitlement: Some(series.entitlement_id),
                    });
                }
            }
        }
        self.quotes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(&retained_keys);
        let required = self
            .catalog
            .values()
            .filter(|instrument| {
                demand.required.contains(&(
                    instrument.provider.clone(),
                    instrument.instrument_id.clone(),
                    instrument.entitlement_id.clone(),
                ))
            })
            .cloned()
            .collect::<Vec<_>>();
        self.order_books.retain(|identity, book| {
            required.iter().any(|instrument| {
                (
                    instrument.provider.clone(),
                    instrument.instrument_id.clone(),
                ) == *identity
                    && instrument.entitlement_id == book.instrument.entitlement_id
                    && instrument.price_scale == book.instrument.price_scale
                    && instrument.quantity_scale == book.instrument.quantity_scale
                    && instrument.provider_symbol == book.instrument.provider_symbol
            })
        });
        for instrument in required {
            let identity = (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            );
            let depth_subscribed = demand.depth.contains(&(
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
                instrument.entitlement_id.clone(),
            ));
            if let Some(book) = self.order_books.get_mut(&identity) {
                // Catalog selection counters describe consumers, not a new book.
                // Keep canonical revisions monotonic across metadata refreshes;
                // transport generations are fenced by provider_depth itself.
                book.update_instrument(instrument);
                book.calendar = self.calendar.for_instrument(&book.instrument.instrument_id);
                book.set_depth_subscribed(depth_subscribed);
            } else {
                let Some(descriptor) = self.providers.descriptor(&instrument.provider) else {
                    diagnostic!("Aeris market instrument references an unregistered provider");
                    continue;
                };
                let mut book =
                    ProviderOrderBook::new(instrument, descriptor.capabilities.depth.is_none());
                book.calendar = self.calendar.for_instrument(&book.instrument.instrument_id);
                book.set_depth_subscribed(depth_subscribed);
                self.order_books.insert(identity, book);
            }
        }
        for (provider, instrument_id) in self.grant_order_level_subscriptions(&demand.order_levels)
        {
            self.broadcast_order_book(&provider, &instrument_id);
        }
        // Like the canonical image, a display image stops being current once nothing
        // subscribes to depth; keeping it would republish a stale book on resubscription.
        self.display_depth.retain(|identity, display| {
            self.engine
                .provider_status(&identity.0)
                .and_then(|status| status.generation)
                .is_some_and(|generation| generation.0.get() == display.provider_generation)
                && self
                    .order_books
                    .get(identity)
                    .is_some_and(|book| !book.publishes_top_of_book_only())
        });
    }

    fn required_order_book_identities(&self) -> OrderBookDemand {
        let mut demand = OrderBookDemand::default();
        for (series, subscription) in self.engine.subscriptions() {
            demand.insert(
                (
                    series.provider_id.clone(),
                    series.instrument_id.clone(),
                    series.entitlement_id.clone(),
                ),
                subscription.streams,
            );
        }
        for (key, streams) in self.engine.instrument_subscriptions() {
            demand.insert(
                (
                    key.provider,
                    key.instrument.as_str().to_string(),
                    key.entitlement.unwrap_or_default(),
                ),
                streams,
            );
        }
        demand
    }

    /// Grants each provider's bounded order-level subscriptions and returns
    /// the books whose grant changed. A book keeps its grant while demanded,
    /// so new demand never evicts a working book; other demanded books are
    /// granted in identity order until the provider's budget is spent.
    fn grant_order_level_subscriptions(
        &mut self,
        demanded: &BTreeSet<OrderBookIdentity>,
    ) -> Vec<(String, String)> {
        let is_demanded = |(provider, instrument_id): &(String, String),
                           book: &ProviderOrderBook| {
            demanded.contains(&(
                provider.clone(),
                instrument_id.clone(),
                book.instrument.entitlement_id.clone(),
            ))
        };
        let mut granted = BTreeMap::<String, usize>::new();
        let mut previous = BTreeMap::new();
        for (identity, book) in &mut self.order_books {
            let subscription = book.order_level_subscription();
            previous.insert(identity.clone(), subscription);
            if subscription == OrderLevelSubscription::Admitted && is_demanded(identity, book) {
                *granted.entry(identity.0.clone()).or_default() += 1;
            } else {
                book.set_order_level_subscription(OrderLevelSubscription::Off);
            }
        }
        for (identity, book) in &mut self.order_books {
            if book.order_level_subscription() == OrderLevelSubscription::Admitted
                || !is_demanded(identity, book)
            {
                continue;
            }
            let budget = self
                .providers
                .descriptor(&identity.0)
                .map_or(0, |descriptor| {
                    descriptor.capabilities.maximum_order_level_instruments
                });
            let count = granted.entry(identity.0.clone()).or_default();
            book.set_order_level_subscription(if *count < budget {
                *count += 1;
                OrderLevelSubscription::Admitted
            } else {
                OrderLevelSubscription::BudgetExhausted
            });
        }
        self.order_books
            .iter()
            .filter(|(identity, book)| {
                previous.get(*identity) != Some(&book.order_level_subscription())
            })
            .map(|(identity, _)| identity.clone())
            .collect()
    }
}
