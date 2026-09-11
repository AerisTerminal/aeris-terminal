use crate::{ConsumerId, MarketDataLeaseId, StreamRequirements};
use axiusflow_market_data::BarSeriesKey;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionStatus {
    pub consumer_count: usize,
    pub streams: StreamRequirements,
}

struct SharedSubscription {
    consumers: BTreeMap<ConsumerId, StreamRequirements>,
    leases: BTreeMap<MarketDataLeaseId, StreamRequirements>,
}

pub(crate) struct SubscriptionRegistry {
    subscriptions: BTreeMap<BarSeriesKey, SharedSubscription>,
}

impl SubscriptionRegistry {
    pub(crate) fn new() -> Self {
        Self {
            subscriptions: BTreeMap::new(),
        }
    }

    pub(crate) fn replace(
        &mut self,
        consumer_id: ConsumerId,
        previous: Option<(&BarSeriesKey, StreamRequirements)>,
        series: &BarSeriesKey,
        streams: StreamRequirements,
    ) {
        if let Some((previous_series, previous_streams)) = previous
            && (previous_series != series || previous_streams != streams)
        {
            self.remove(consumer_id, previous_series);
        }
        self.subscriptions
            .entry(series.clone())
            .or_insert_with(|| SharedSubscription {
                consumers: BTreeMap::new(),
                leases: BTreeMap::new(),
            })
            .consumers
            .insert(consumer_id, streams);
    }

    pub(crate) fn remove(&mut self, consumer_id: ConsumerId, series: &BarSeriesKey) {
        let remove_series = self
            .subscriptions
            .get_mut(series)
            .is_some_and(|subscription| {
                subscription.consumers.remove(&consumer_id);
                subscription.consumers.is_empty() && subscription.leases.is_empty()
            });
        if remove_series {
            self.subscriptions.remove(series);
        }
    }

    pub(crate) fn replace_lease(
        &mut self,
        lease_id: MarketDataLeaseId,
        previous: Option<(&BarSeriesKey, StreamRequirements)>,
        series: &BarSeriesKey,
        streams: StreamRequirements,
    ) {
        if let Some((previous_series, previous_streams)) = previous
            && (previous_series != series || previous_streams != streams)
        {
            self.remove_lease(lease_id, previous_series);
        }
        self.subscriptions
            .entry(series.clone())
            .or_insert_with(|| SharedSubscription {
                consumers: BTreeMap::new(),
                leases: BTreeMap::new(),
            })
            .leases
            .insert(lease_id, streams);
    }

    pub(crate) fn remove_lease(&mut self, lease_id: MarketDataLeaseId, series: &BarSeriesKey) {
        let remove_series = self
            .subscriptions
            .get_mut(series)
            .is_some_and(|subscription| {
                subscription.leases.remove(&lease_id);
                subscription.consumers.is_empty() && subscription.leases.is_empty()
            });
        if remove_series {
            self.subscriptions.remove(series);
        }
    }

    pub(crate) fn status(&self, series: &BarSeriesKey) -> Option<SubscriptionStatus> {
        self.subscriptions.get(series).map(|subscription| {
            let streams = subscription
                .consumers
                .values()
                .chain(subscription.leases.values())
                .copied()
                .fold(StreamRequirements::NONE, StreamRequirements::union);
            SubscriptionStatus {
                consumer_count: subscription.consumers.len(),
                streams,
            }
        })
    }

    pub(crate) fn all(&self) -> Vec<(BarSeriesKey, SubscriptionStatus)> {
        self.subscriptions
            .keys()
            .filter_map(|series| self.status(series).map(|status| (series.clone(), status)))
            .collect()
    }

    pub(crate) fn contains(&self, series: &BarSeriesKey) -> bool {
        self.subscriptions.contains_key(series)
    }

    pub(crate) fn len(&self) -> usize {
        self.subscriptions.len()
    }
}
