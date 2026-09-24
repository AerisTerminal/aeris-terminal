use crate::{EngineError, MarketDataLeaseId, StreamRequirements};
use aeris_market_data::BarSeriesKey;
use std::{
    collections::BTreeMap,
    num::{NonZeroU64, NonZeroUsize},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DataLease {
    pub(crate) series: BarSeriesKey,
    pub(crate) streams: StreamRequirements,
}

pub(crate) struct DataLeaseRegistry {
    maximum_leases: NonZeroUsize,
    leases: BTreeMap<MarketDataLeaseId, DataLease>,
    next_id: u64,
}

impl DataLeaseRegistry {
    pub(crate) fn new(maximum_leases: NonZeroUsize) -> Self {
        Self {
            maximum_leases,
            leases: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub(crate) fn acquire(
        &mut self,
        series: BarSeriesKey,
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
        self.leases.insert(lease_id, DataLease { series, streams });
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
        series: BarSeriesKey,
        streams: StreamRequirements,
    ) -> Result<DataLease, EngineError> {
        let current = self
            .leases
            .get_mut(&lease_id)
            .ok_or(EngineError::UnknownDataLease(lease_id))?;
        Ok(std::mem::replace(current, DataLease { series, streams }))
    }

    pub(crate) fn current(&self, lease_id: MarketDataLeaseId) -> Option<&DataLease> {
        self.leases.get(&lease_id)
    }

    pub(crate) fn remove(&mut self, lease_id: MarketDataLeaseId) -> Option<DataLease> {
        self.leases.remove(&lease_id)
    }

    pub(crate) fn referenced_series(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.leases.values().map(|lease| &lease.series)
    }

    pub(crate) fn len(&self) -> usize {
        self.leases.len()
    }

    pub(crate) const fn maximum(&self) -> NonZeroUsize {
        self.maximum_leases
    }
}
