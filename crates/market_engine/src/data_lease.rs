use crate::{
    EngineError, InstrumentStreamKey, LeaseOwner, MarketDataLeaseId, MarketDemandKey,
    StreamRequirements,
};
use aeris_market_data::BarSeriesKey;
use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU64, NonZeroUsize},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DataLease {
    pub(crate) key: MarketDemandKey,
    pub(crate) streams: StreamRequirements,
}

pub(crate) struct DataLeaseRegistry {
    maximum_leases: NonZeroUsize,
    leases: BTreeMap<MarketDataLeaseId, DataLease>,
    owned_instruments: BTreeMap<LeaseOwner, BTreeMap<InstrumentStreamKey, MarketDataLeaseId>>,
    next_id: u64,
}

impl DataLeaseRegistry {
    pub(crate) fn new(maximum_leases: NonZeroUsize) -> Self {
        Self {
            maximum_leases,
            leases: BTreeMap::new(),
            owned_instruments: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub(crate) fn acquire(
        &mut self,
        key: MarketDemandKey,
        streams: StreamRequirements,
    ) -> Result<MarketDataLeaseId, EngineError> {
        if self.leases.len() == self.maximum_leases.get() {
            return Err(EngineError::DataLeaseLimitExceeded {
                maximum: self.maximum_leases,
            });
        }
        let raw = NonZeroU64::new(self.next_id).ok_or(EngineError::DataLeaseIdentityExhausted)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(EngineError::DataLeaseIdentityExhausted)?;
        let lease_id = MarketDataLeaseId(raw);
        self.leases.insert(lease_id, DataLease { key, streams });
        Ok(lease_id)
    }

    pub(crate) fn preflight_acquisitions(&self, additional: usize) -> Result<(), EngineError> {
        let requested = self
            .leases
            .len()
            .checked_add(additional)
            .ok_or(EngineError::CapacityOverflow)?;
        if requested > self.maximum_leases.get() {
            return Err(EngineError::DataLeaseLimitExceeded {
                maximum: self.maximum_leases,
            });
        }
        let additional = u64::try_from(additional).map_err(|_| EngineError::CapacityOverflow)?;
        self.next_id
            .checked_add(additional)
            .ok_or(EngineError::DataLeaseIdentityExhausted)?;
        Ok(())
    }

    pub(crate) fn replace(
        &mut self,
        lease_id: MarketDataLeaseId,
        key: MarketDemandKey,
        streams: StreamRequirements,
    ) -> Result<DataLease, EngineError> {
        let current = self
            .leases
            .get_mut(&lease_id)
            .ok_or(EngineError::UnknownDataLease(lease_id))?;
        Ok(std::mem::replace(current, DataLease { key, streams }))
    }

    pub(crate) fn current(&self, lease_id: MarketDataLeaseId) -> Option<&DataLease> {
        self.leases.get(&lease_id)
    }

    pub(crate) fn remove(&mut self, lease_id: MarketDataLeaseId) -> Option<DataLease> {
        let removed = self.leases.remove(&lease_id)?;
        if let MarketDemandKey::Instrument(key) = &removed.key {
            self.owned_instruments.retain(|_, leases| {
                leases.retain(|owned_key, id| owned_key != key || *id != lease_id);
                !leases.is_empty()
            });
        }
        Some(removed)
    }

    pub(crate) fn referenced_series(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.leases.values().filter_map(|lease| match &lease.key {
            MarketDemandKey::Series(series) => Some(series),
            MarketDemandKey::Instrument(_) => None,
        })
    }

    /// Changes an owner's entire instrument interest atomically after validation
    /// and preflight by the engine. Retained leases keep their identity.
    pub(crate) fn set_instruments(
        &mut self,
        owner: LeaseOwner,
        keys: BTreeSet<InstrumentStreamKey>,
        streams: StreamRequirements,
    ) -> Result<(), EngineError> {
        let previous = self
            .owned_instruments
            .get(&owner)
            .cloned()
            .unwrap_or_default();
        let additional = keys
            .iter()
            .filter(|key| !previous.contains_key(*key))
            .count();
        let released = previous.len()
            - keys
                .iter()
                .filter(|key| previous.contains_key(*key))
                .count();
        let requested = self.leases.len() - released + additional;
        if requested > self.maximum_leases.get() {
            return Err(EngineError::DataLeaseLimitExceeded {
                maximum: self.maximum_leases,
            });
        }
        let next_id = self
            .next_id
            .checked_add(u64::try_from(additional).map_err(|_| EngineError::CapacityOverflow)?)
            .ok_or(EngineError::DataLeaseIdentityExhausted)?;
        for (key, id) in &previous {
            if !keys.contains(key) {
                self.leases.remove(id);
            }
        }
        let mut current = BTreeMap::new();
        for key in keys {
            if let Some(&id) = previous.get(&key) {
                if let Some(lease) = self.leases.get_mut(&id) {
                    lease.streams = streams;
                }
                current.insert(key, id);
            } else {
                let id = MarketDataLeaseId(
                    NonZeroU64::new(self.next_id).ok_or(EngineError::DataLeaseIdentityExhausted)?,
                );
                self.next_id += 1;
                self.leases.insert(
                    id,
                    DataLease {
                        key: MarketDemandKey::Instrument(key.clone()),
                        streams,
                    },
                );
                current.insert(key, id);
            }
        }
        debug_assert_eq!(self.next_id, next_id);
        if current.is_empty() {
            self.owned_instruments.remove(&owner);
        } else {
            self.owned_instruments.insert(owner, current);
        }
        Ok(())
    }

    pub(crate) fn instrument_subscriptions(
        &self,
    ) -> BTreeMap<InstrumentStreamKey, StreamRequirements> {
        let mut subscriptions = BTreeMap::new();
        for lease in self.leases.values() {
            if let MarketDemandKey::Instrument(key) = &lease.key {
                subscriptions
                    .entry(key.clone())
                    .and_modify(|streams: &mut StreamRequirements| {
                        *streams = streams.union(lease.streams);
                    })
                    .or_insert(lease.streams);
            }
        }
        subscriptions
    }

    pub(crate) fn release_owner(&mut self, owner: LeaseOwner) {
        if let Some(leases) = self.owned_instruments.remove(&owner) {
            for id in leases.values() {
                self.leases.remove(id);
            }
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.leases.len()
    }

    pub(crate) const fn maximum(&self) -> NonZeroUsize {
        self.maximum_leases
    }
}
