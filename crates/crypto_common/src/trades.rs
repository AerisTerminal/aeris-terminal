//! Bounded trade-ID deduplication ring for reconnect-safe trade ingestion.

use std::collections::{HashSet, VecDeque};

/// Bounded FIFO + hash set remembering recently delivered trade IDs.
#[derive(Clone, Debug)]
pub struct TradeDedupRing<T> {
    capacity: usize,
    order: VecDeque<T>,
    seen: HashSet<T>,
}

impl<T: Eq + std::hash::Hash + Clone> TradeDedupRing<T> {
    /// Creates a ring retaining at most `capacity` recent trade IDs.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            order: VecDeque::with_capacity(cap),
            seen: HashSet::with_capacity(cap),
        }
    }

    /// Inserts `id` and returns `true` if it was not recently seen.
    pub fn insert(&mut self, id: T) -> bool {
        if !self.seen.insert(id.clone()) {
            return false;
        }
        if self.order.len() == self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        self.order.push_back(id);
        true
    }

    /// Clears all remembered IDs.
    pub fn clear(&mut self) {
        self.order.clear();
        self.seen.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_ring_evicts_oldest_at_capacity() {
        let mut ring = TradeDedupRing::new(2);
        assert!(ring.insert(1_u64));
        assert!(!ring.insert(1_u64));
        assert!(ring.insert(2_u64));
        assert!(ring.insert(3_u64));
        // 1 was evicted
        assert!(ring.insert(1_u64));
        assert!(!ring.insert(3_u64));
    }
}
