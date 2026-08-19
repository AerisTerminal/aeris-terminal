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
        if entry.score == 0 || entry.workspaces.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::{HotSetDescriptor, HotSetEntry, HotSetManager};
    use crate::WorkspaceId;
    use axiusflow_market_data::{BarPeriod, BarSeriesKey};
    use std::{collections::BTreeSet, num::NonZeroU64, num::NonZeroUsize};

    fn descriptor(symbol: &str) -> HotSetDescriptor {
        HotSetDescriptor {
            series: BarSeriesKey {
                provider_id: "coinbase".to_string(),
                instrument_id: format!("coinbase:spot:{symbol}"),
                entitlement_id: "coinbase-public-market-data".to_string(),
                period: BarPeriod::time(60).expect("period"),
                definition_version: 1,
            },
            account_id: "coinbase-public".to_string(),
            provider_symbol: symbol.to_string(),
            venue_id: "coinbase".to_string(),
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
}
