use crate::{ChartId, DesktopHistoryError, HistoryPublication};
use axiusflow_desktop_storage::{SegmentAccessPolicy, SegmentIdentity};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    sync::{Arc, Weak},
};

struct CacheEntry<T> {
    publication: Arc<HistoryPublication<T>>,
    access_policy: Option<SegmentAccessPolicy>,
    decoded_bytes: usize,
    last_access: u64,
}

struct RetiredGeneration<T> {
    publication: Weak<HistoryPublication<T>>,
    decoded_bytes: usize,
}

/// Bounded worker-owned cache returning shared immutable chart generations.
pub(crate) struct SharedHistoryCache<T> {
    maximum_entries: NonZeroUsize,
    maximum_decoded_bytes: NonZeroUsize,
    maximum_charts: NonZeroUsize,
    maximum_generations: usize,
    decoded_bytes: usize,
    access_order: u64,
    entries: BTreeMap<SegmentIdentity, CacheEntry<T>>,
    retired: Vec<RetiredGeneration<T>>,
    chart_bindings: BTreeMap<ChartId, SegmentIdentity>,
    pinned_identities: BTreeSet<SegmentIdentity>,
}

impl<T> SharedHistoryCache<T> {
    #[must_use]
    pub(crate) fn new(
        maximum_entries: NonZeroUsize,
        maximum_decoded_bytes: NonZeroUsize,
        maximum_charts: NonZeroUsize,
    ) -> Self {
        Self {
            maximum_entries,
            maximum_decoded_bytes,
            maximum_charts,
            maximum_generations: maximum_entries.get().saturating_add(maximum_charts.get()),
            decoded_bytes: 0,
            access_order: 0,
            entries: BTreeMap::new(),
            retired: Vec::new(),
            chart_bindings: BTreeMap::new(),
            pinned_identities: BTreeSet::new(),
        }
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub(crate) fn decoded_bytes(&mut self) -> usize {
        self.prune_retired();
        self.decoded_bytes
    }

    #[must_use]
    pub(crate) fn decoded_bytes_for(&self, identity: &SegmentIdentity) -> Option<usize> {
        self.entries.get(identity).map(|entry| entry.decoded_bytes)
    }

    pub(crate) fn remove(&mut self, identity: &SegmentIdentity) {
        self.remove_entry(identity);
    }

    pub(crate) fn get(
        &mut self,
        identity: &SegmentIdentity,
    ) -> Option<(Arc<HistoryPublication<T>>, Option<SegmentAccessPolicy>)> {
        let access_order = self.next_access_order();
        self.entries.get_mut(identity).map(|entry| {
            entry.last_access = access_order;
            (Arc::clone(&entry.publication), entry.access_policy.clone())
        })
    }

    pub(crate) fn reserve_publish_capacity(
        &mut self,
        identity: &SegmentIdentity,
        decoded_bytes: usize,
        reserved_decoded_bytes: usize,
    ) -> Result<(), DesktopHistoryError> {
        let requested = decoded_bytes.saturating_add(reserved_decoded_bytes);
        if requested > self.maximum_decoded_bytes.get() {
            return Err(DesktopHistoryError::DecodedHistoryTooLarge {
                requested,
                maximum: self.maximum_decoded_bytes.get(),
            });
        }
        self.prepare_publish(identity, decoded_bytes, reserved_decoded_bytes)
    }

    /// Replaces one immutable generation after proving all cache bounds.
    ///
    /// # Errors
    ///
    /// Returns an error without removing the current generation when the new
    /// value exceeds the byte bound or no unbound entry can be evicted.
    pub(crate) fn publish(
        &mut self,
        identity: SegmentIdentity,
        publication: HistoryPublication<T>,
        decoded_bytes: usize,
        reserved_decoded_bytes: usize,
        access_policy: Option<SegmentAccessPolicy>,
    ) -> Result<Arc<HistoryPublication<T>>, DesktopHistoryError> {
        let requested = decoded_bytes.saturating_add(reserved_decoded_bytes);
        if requested > self.maximum_decoded_bytes.get() {
            return Err(DesktopHistoryError::DecodedHistoryTooLarge {
                requested,
                maximum: self.maximum_decoded_bytes.get(),
            });
        }
        self.prepare_publish(&identity, decoded_bytes, reserved_decoded_bytes)?;
        self.remove_entry(&identity);
        let publication = Arc::new(publication);
        let last_access = self.next_access_order();
        self.decoded_bytes = self.decoded_bytes.saturating_add(decoded_bytes);
        self.entries.insert(
            identity,
            CacheEntry {
                publication: Arc::clone(&publication),
                access_policy,
                decoded_bytes,
                last_access,
            },
        );
        Ok(publication)
    }

    /// Associates a bounded chart identity with an existing shared generation.
    ///
    /// # Errors
    ///
    /// Returns an error when a new binding would exceed the chart bound.
    pub(crate) fn bind_chart(
        &mut self,
        chart_id: ChartId,
        identity: &SegmentIdentity,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopHistoryError> {
        if !self.chart_bindings.contains_key(&chart_id)
            && self.chart_bindings.len() >= self.maximum_charts.get()
        {
            return Err(DesktopHistoryError::ChartLimitReached {
                maximum: self.maximum_charts.get(),
            });
        }
        self.chart_bindings.insert(chart_id, identity.clone());
        Ok(self.get(identity).map(|(publication, _)| publication))
    }

    pub(crate) fn unbind_chart(&mut self, chart_id: ChartId) {
        self.chart_bindings.remove(&chart_id);
    }

    pub(crate) fn pin(&mut self, identity: SegmentIdentity) {
        self.pinned_identities.insert(identity);
    }

    pub(crate) fn unpin(&mut self, identity: &SegmentIdentity) {
        self.pinned_identities.remove(identity);
    }

    fn prepare_publish(
        &mut self,
        identity: &SegmentIdentity,
        decoded_bytes: usize,
        reserved_decoded_bytes: usize,
    ) -> Result<(), DesktopHistoryError> {
        self.prune_retired();
        let replacement = self.entries.get(identity);
        let replacement_is_retained =
            replacement.is_some_and(|entry| Arc::strong_count(&entry.publication) > 1);
        let reclaimed_replacement_bytes = replacement.map_or(0, |entry| {
            if replacement_is_retained {
                0
            } else {
                entry.decoded_bytes
            }
        });
        let mut projected_entries = self
            .entries
            .len()
            .saturating_add(usize::from(!self.entries.contains_key(identity)));
        let mut projected_bytes = self
            .decoded_bytes
            .saturating_sub(reclaimed_replacement_bytes)
            .saturating_add(decoded_bytes)
            .saturating_add(reserved_decoded_bytes);
        let mut projected_generations = projected_entries
            .saturating_add(self.retired.len())
            .saturating_add(usize::from(replacement_is_retained));
        let mut candidates = self
            .entries
            .iter()
            .filter(|(candidate, _)| {
                *candidate != identity
                    && !self.pinned_identities.contains(*candidate)
                    && !self
                        .chart_bindings
                        .values()
                        .any(|bound_identity| bound_identity == *candidate)
            })
            .map(|(candidate, entry)| {
                (
                    entry.last_access,
                    candidate.clone(),
                    entry.decoded_bytes,
                    Arc::strong_count(&entry.publication) > 1,
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(last_access, _, _, _)| *last_access);
        let mut evictions = Vec::new();
        for (_, candidate, candidate_bytes, externally_retained) in candidates {
            if projected_entries <= self.maximum_entries.get()
                && projected_bytes <= self.maximum_decoded_bytes.get()
                && projected_generations <= self.maximum_generations
            {
                break;
            }
            projected_entries = projected_entries.saturating_sub(1);
            if !externally_retained {
                projected_bytes = projected_bytes.saturating_sub(candidate_bytes);
                projected_generations = projected_generations.saturating_sub(1);
            }
            evictions.push(candidate);
        }
        if projected_entries > self.maximum_entries.get()
            || projected_bytes > self.maximum_decoded_bytes.get()
            || projected_generations > self.maximum_generations
        {
            return Err(DesktopHistoryError::CacheFull {
                maximum_entries: self.maximum_entries.get(),
            });
        }
        for candidate in evictions {
            self.remove_entry(&candidate);
        }
        Ok(())
    }

    fn remove_entry(&mut self, identity: &SegmentIdentity) {
        let Some(removed) = self.entries.remove(identity) else {
            return;
        };
        if Arc::strong_count(&removed.publication) > 1 {
            self.retired.push(RetiredGeneration {
                publication: Arc::downgrade(&removed.publication),
                decoded_bytes: removed.decoded_bytes,
            });
        } else {
            self.decoded_bytes = self.decoded_bytes.saturating_sub(removed.decoded_bytes);
        }
    }

    fn prune_retired(&mut self) {
        let mut retained = Vec::with_capacity(self.retired.len());
        for generation in self.retired.drain(..) {
            if generation.publication.strong_count() == 0 {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(generation.decoded_bytes);
            } else {
                retained.push(generation);
            }
        }
        self.retired = retained;
    }

    fn next_access_order(&mut self) -> u64 {
        let current = self.access_order;
        self.access_order = self.access_order.saturating_add(1);
        current
    }
}
