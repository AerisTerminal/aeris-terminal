//! Bounded displayed-provenance retention for the chart view.

use axiusflow_application::{MarketEventProvenance, ProvenancedMarketBar, ReplaySnapshot};
use std::{collections::BTreeMap, num::NonZeroUsize};

pub(crate) const DEFAULT_CHART_SERIES_MAX_POINTS: usize = 4_096;

#[derive(Debug)]
pub(crate) struct DisplayedProvenance {
    by_source_sequence: BTreeMap<u64, MarketEventProvenance>,
    max_items: NonZeroUsize,
}

impl DisplayedProvenance {
    pub(crate) fn empty() -> Self {
        Self {
            by_source_sequence: BTreeMap::new(),
            max_items: NonZeroUsize::new(DEFAULT_CHART_SERIES_MAX_POINTS)
                .unwrap_or(NonZeroUsize::MIN),
        }
    }

    pub(crate) fn from_snapshot(snapshot: &ReplaySnapshot) -> Self {
        Self::from_snapshot_with_limit(
            snapshot,
            NonZeroUsize::new(DEFAULT_CHART_SERIES_MAX_POINTS).unwrap_or(NonZeroUsize::MIN),
        )
    }

    pub(crate) fn from_snapshot_with_limit(
        snapshot: &ReplaySnapshot,
        max_items: NonZeroUsize,
    ) -> Self {
        let mut history = Self {
            by_source_sequence: BTreeMap::new(),
            max_items,
        };
        history.replace_snapshot(snapshot);
        history
    }

    pub(crate) fn replace_snapshot(&mut self, snapshot: &ReplaySnapshot) {
        self.by_source_sequence.clear();
        self.extend(snapshot.bars());
    }

    pub(crate) fn extend(&mut self, items: &[ProvenancedMarketBar]) {
        self.by_source_sequence.extend(
            items
                .iter()
                .map(|item| (item.value().source_sequence, item.provenance().clone())),
        );
        while self.by_source_sequence.len() > self.max_items.get() {
            self.by_source_sequence.pop_first();
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.by_source_sequence.len()
    }

    pub(crate) fn get(&self, source_sequence: u64) -> Option<&MarketEventProvenance> {
        self.by_source_sequence.get(&source_sequence)
    }

    pub(crate) fn latest(&self) -> Option<&MarketEventProvenance> {
        self.by_source_sequence
            .last_key_value()
            .map(|(_, provenance)| provenance)
    }
}
