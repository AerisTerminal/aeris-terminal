use crate::{EngineError, WorkspaceId};
use axiusflow_market_data::BarSeriesKey;
use std::{collections::BTreeSet, num::NonZeroUsize};

/// Provider-neutral metadata needed to rebuild one useful recent market series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotSetDescriptor {
    pub series: BarSeriesKey,
    pub account_id: String,
    pub provider_symbol: String,
    pub venue_id: String,
    pub display_symbol: String,
    pub price_scale: u8,
    pub quantity_scale: u8,
}

/// Bounded hot-set entry ranked for warm reconstruction and resource retention.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotSetEntry {
    pub descriptor: HotSetDescriptor,
    pub workspaces: BTreeSet<WorkspaceId>,
    pub pinned: bool,
    pub score: u64,
    pub last_used_unix_seconds: u64,
    pub provider_watermark: u64,
    pub series_watermark: u64,
    pub viewport: Option<(i64, i64)>,
    pub coverage: Option<(i64, i64)>,
}

/// Measured retention tier for one bounded hot-set identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotSetTier {
    Hot,
    Warm,
    Cold,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotSetRetention {
    pub entry: HotSetEntry,
    pub tier: HotSetTier,
}

/// Single bounded owner for recent/pinned series identities used by warm startup.
#[derive(Clone)]
pub struct HotSetManager {
    capacity: NonZeroUsize,
    entries: Vec<HotSetEntry>,
}

impl HotSetManager {
    #[must_use]
    pub const fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            entries: Vec::new(),
        }
    }

    /// Restores already-validated persisted entries and reapplies the capacity bound.
    ///
    /// # Errors
    /// Returns an error for invalid, duplicate, or over-pinned persisted metadata.
    pub fn restore(&mut self, entries: Vec<HotSetEntry>) -> Result<(), EngineError> {
        self.entries.clear();
        for entry in entries {
            Self::validate_entry(&entry)?;
            if self
                .entries
                .iter()
                .any(|current| current.descriptor.series == entry.descriptor.series)
            {
                return Err(EngineError::DuplicateHotSeries);
            }
            self.entries.push(entry);
        }
        self.enforce_capacity()?;
        Ok(())
    }

    /// Touches one workspace/series identity and returns the new ranked entry.
    ///
    /// # Errors
    /// Returns an error for invalid metadata, score overflow, or pinned-capacity exhaustion.
    pub fn touch(
        &mut self,
        workspace_id: WorkspaceId,
        descriptor: HotSetDescriptor,
        now_unix_seconds: u64,
    ) -> Result<HotSetEntry, EngineError> {
        Self::validate_descriptor(&descriptor)?;
        let next_score = self
            .entries
            .iter()
            .map(|entry| entry.score)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)?;
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.descriptor.series == descriptor.series)
        {
            entry.descriptor = descriptor;
            entry.workspaces.insert(workspace_id);
            entry.score = next_score;
            entry.last_used_unix_seconds = now_unix_seconds;
            return Ok(entry.clone());
        }
        let entry = HotSetEntry {
            descriptor,
            workspaces: BTreeSet::from([workspace_id]),
            pinned: false,
            score: next_score,
            last_used_unix_seconds: now_unix_seconds,
            provider_watermark: 0,
            series_watermark: 0,
            viewport: None,
            coverage: None,
        };
        self.entries.push(entry.clone());
        self.enforce_capacity()?;
        Ok(entry)
    }

    /// Updates the stable viewport for an existing hot series.
    ///
    /// # Errors
    /// Returns an error for an invalid range or unknown series.
    pub fn set_viewport(
        &mut self,
        series: &BarSeriesKey,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), EngineError> {
        if start_unix_nanos >= end_unix_nanos {
            return Err(EngineError::InvalidViewport);
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| &entry.descriptor.series == series)
            .ok_or(EngineError::UnknownHotSeries)?;
        entry.viewport = Some((start_unix_nanos, end_unix_nanos));
        Ok(())
    }

    /// Updates exact retained coverage/watermarks after a canonical publication.
    ///
    /// # Errors
    /// Returns an error for invalid coverage or an unknown series.
    pub fn update_coverage(
        &mut self,
        series: &BarSeriesKey,
        coverage: (i64, i64),
        provider_watermark: u64,
        series_watermark: u64,
    ) -> Result<(), EngineError> {
        if coverage.0 >= coverage.1 {
            return Err(EngineError::InvalidViewport);
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| &entry.descriptor.series == series)
            .ok_or(EngineError::UnknownHotSeries)?;
        entry.coverage = Some(coverage);
        entry.provider_watermark = provider_watermark;
        entry.series_watermark = series_watermark;
        Ok(())
    }

    /// Pins or unpins an existing series without bypassing the global bound.
    ///
    /// # Errors
    /// Returns an error when the series is not present.
    pub fn set_pinned(&mut self, series: &BarSeriesKey, pinned: bool) -> Result<(), EngineError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| &entry.descriptor.series == series)
            .ok_or(EngineError::UnknownHotSeries)?;
        entry.pinned = pinned;
        Ok(())
    }

    /// Returns highest-priority entries first without exposing mutable ownership.
    #[must_use]
    pub fn ranked(&self) -> Vec<HotSetEntry> {
        let mut entries = self.entries.clone();
        entries.sort_unstable_by(|left, right| {
            right
                .pinned
                .cmp(&left.pinned)
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| {
                    right
                        .last_used_unix_seconds
                        .cmp(&left.last_used_unix_seconds)
                })
                .then_with(|| left.descriptor.series.cmp(&right.descriptor.series))
        });
        entries
    }

    /// Classifies every bounded identity from active workspace membership, exact
    /// watchlist identity, explicit pins, recency, and a measured memory budget.
    #[must_use]
    pub fn classify(
        &self,
        active_workspaces: &BTreeSet<WorkspaceId>,
        watchlist: &BTreeSet<BarSeriesKey>,
        memory_budget_bytes: usize,
        estimated_entry_bytes: usize,
    ) -> Vec<HotSetRetention> {
        let capacity = memory_budget_bytes
            .checked_div(estimated_entry_bytes)
            .unwrap_or(0);
        let mut ranked = self.ranked();
        ranked.sort_unstable_by(|left, right| {
            hot_priority(right, active_workspaces, watchlist)
                .cmp(&hot_priority(left, active_workspaces, watchlist))
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.descriptor.series.cmp(&right.descriptor.series))
        });
        ranked
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let active = entry
                    .workspaces
                    .iter()
                    .any(|workspace| active_workspaces.contains(workspace));
                HotSetRetention {
                    tier: if active {
                        HotSetTier::Hot
                    } else if index < capacity {
                        HotSetTier::Warm
                    } else {
                        HotSetTier::Cold
                    },
                    entry,
                }
            })
            .collect()
    }

    fn enforce_capacity(&mut self) -> Result<(), EngineError> {
        while self.entries.len() > self.capacity.get() {
            let removable = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.pinned)
                .min_by_key(|(_, entry)| (entry.score, entry.last_used_unix_seconds))
                .map(|(index, _)| index)
                .ok_or(EngineError::HotSetPinnedLimitExceeded {
                    maximum: self.capacity,
                })?;
            self.entries.remove(removable);
        }
        Ok(())
    }

    fn validate_entry(entry: &HotSetEntry) -> Result<(), EngineError> {
        Self::validate_descriptor(&entry.descriptor)?;
        if entry.score == 0 {
            return Err(EngineError::InvalidHotSeries);
        }
        if entry.viewport.is_some_and(|(start, end)| start >= end)
            || entry.coverage.is_some_and(|(start, end)| start >= end)
        {
            return Err(EngineError::InvalidViewport);
        }
        Ok(())
    }

    fn validate_descriptor(descriptor: &HotSetDescriptor) -> Result<(), EngineError> {
        descriptor.series.validate()?;
        if descriptor.account_id.trim().is_empty()
            || descriptor.provider_symbol.trim().is_empty()
            || descriptor.venue_id.trim().is_empty()
            || descriptor.display_symbol.trim().is_empty()
            || descriptor.price_scale > 18
            || descriptor.quantity_scale > 18
        {
            return Err(EngineError::InvalidHotSeries);
        }
        Ok(())
    }
}

fn hot_priority(
    entry: &HotSetEntry,
    active_workspaces: &BTreeSet<WorkspaceId>,
    watchlist: &BTreeSet<BarSeriesKey>,
) -> u8 {
    if entry
        .workspaces
        .iter()
        .any(|workspace| active_workspaces.contains(workspace))
    {
        3
    } else if entry.pinned {
        2
    } else {
        u8::from(watchlist.contains(&entry.descriptor.series))
    }
}

#[cfg(test)]
mod tests {
    use super::{HotSetDescriptor, HotSetEntry, HotSetManager, HotSetTier};
    use crate::WorkspaceId;
    use axiusflow_market_data::{BarPeriod, BarSeriesKey};
    use std::{collections::BTreeSet, num::NonZeroU64, num::NonZeroUsize};

    fn descriptor(symbol: &str) -> HotSetDescriptor {
        HotSetDescriptor {
            series: BarSeriesKey {
                provider_id: "rithmic".to_string(),
                instrument_id: format!("rithmic:spot:{symbol}"),
                entitlement_id: "rithmic-public-market-data".to_string(),
                period: BarPeriod::time(60).expect("period"),
                definition_version: 1,
            },
            account_id: "rithmic-public".to_string(),
            provider_symbol: symbol.to_string(),
            venue_id: "rithmic".to_string(),
            display_symbol: symbol.to_string(),
            price_scale: 2,
            quantity_scale: 8,
        }
    }

    fn workspace(value: u64) -> WorkspaceId {
        WorkspaceId(NonZeroU64::new(value).expect("workspace"))
    }

    #[test]
    fn ranking_retains_pins_and_evicts_the_coldest_unpinned_entry() {
        let mut hot = HotSetManager::new(NonZeroUsize::new(2).expect("bound"));
        hot.touch(workspace(1), descriptor("BTC-USD"), 1)
            .expect("touch BTC");
        let btc = descriptor("BTC-USD").series;
        hot.set_pinned(&btc, true).expect("pin BTC");
        hot.touch(workspace(2), descriptor("ETH-USD"), 2)
            .expect("touch ETH");
        hot.touch(workspace(3), descriptor("SOL-USD"), 3)
            .expect("touch SOL");
        let ranked = hot.ranked();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].descriptor.series, btc);
        assert_eq!(ranked[1].descriptor.provider_symbol, "SOL-USD");
    }

    #[test]
    fn restore_rejects_duplicate_series_and_invalid_coverage() {
        let mut hot = HotSetManager::new(NonZeroUsize::new(2).expect("bound"));
        let entry = HotSetEntry {
            descriptor: descriptor("BTC-USD"),
            workspaces: BTreeSet::from([workspace(1)]),
            pinned: false,
            score: 1,
            last_used_unix_seconds: 1,
            provider_watermark: 0,
            series_watermark: 0,
            viewport: None,
            coverage: Some((5, 4)),
        };
        assert!(hot.restore(vec![entry]).is_err());
    }

    #[test]
    fn active_workspace_stays_hot_ahead_of_pins_watchlists_and_recency_under_pressure() {
        let mut hot = HotSetManager::new(NonZeroUsize::new(4).expect("bound"));
        hot.touch(workspace(1), descriptor("BTC-USD"), 1)
            .expect("active series");
        hot.touch(workspace(2), descriptor("ETH-USD"), 2)
            .expect("pinned series");
        hot.touch(workspace(3), descriptor("SOL-USD"), 3)
            .expect("watchlist series");
        hot.touch(workspace(4), descriptor("DOGE-USD"), 4)
            .expect("recent series");
        hot.set_pinned(&descriptor("ETH-USD").series, true)
            .expect("pin");
        let retained = hot.classify(
            &BTreeSet::from([workspace(1)]),
            &BTreeSet::from([descriptor("SOL-USD").series]),
            2_000,
            1_000,
        );
        assert_eq!(retained[0].entry.descriptor.provider_symbol, "BTC-USD");
        assert_eq!(retained[0].tier, HotSetTier::Hot);
        assert_eq!(retained[1].entry.descriptor.provider_symbol, "ETH-USD");
        assert_eq!(retained[1].tier, HotSetTier::Warm);
        assert!(
            retained[2..]
                .iter()
                .all(|entry| entry.tier == HotSetTier::Cold)
        );
    }

    #[test]
    fn recency_only_entry_remains_warm_when_memory_capacity_remains() {
        let mut hot = HotSetManager::new(NonZeroUsize::new(2).expect("bound"));
        hot.restore(vec![
            HotSetEntry {
                descriptor: descriptor("BTC-USD"),
                score: 1,
                last_used_unix_seconds: 1,
                provider_watermark: 1,
                series_watermark: 1,
                pinned: false,
                workspaces: BTreeSet::new(),
                viewport: None,
                coverage: None,
            },
            HotSetEntry {
                descriptor: descriptor("ETH-USD"),
                score: 2,
                last_used_unix_seconds: 2,
                provider_watermark: 1,
                series_watermark: 1,
                pinned: false,
                workspaces: BTreeSet::new(),
                viewport: None,
                coverage: None,
            },
        ])
        .expect("recency-only entries restore");

        let retained = hot.classify(&BTreeSet::new(), &BTreeSet::new(), 1_000, 1_000);

        assert_eq!(retained[0].entry.descriptor.provider_symbol, "ETH-USD");
        assert_eq!(retained[0].tier, HotSetTier::Warm);
        assert_eq!(retained[1].tier, HotSetTier::Cold);
    }
}
