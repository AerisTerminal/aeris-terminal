use crate::{HistoryPageRequest, HistoryRange, ProviderHistoryError, RequestPriority};

/// Authoritative state of one requested history range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageClass {
    Complete,
    Partial,
    ConfirmedEmpty,
    Missing,
    Invalidated,
    Quarantined,
}

/// One non-overlapping classified portion of a coverage request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoverageSpan {
    pub range: HistoryRange,
    pub class: CoverageClass,
}

/// Normalized coverage facts for one exact provider series identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CoverageSnapshot {
    complete: Vec<HistoryRange>,
    confirmed_empty: Vec<HistoryRange>,
    invalidated: Vec<HistoryRange>,
    quarantined: Vec<HistoryRange>,
}

/// Classified coverage and the minimum ranges that require provider repair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoveragePlan {
    classification: CoverageClass,
    spans: Vec<CoverageSpan>,
    repairs: Vec<HistoryRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrioritizedRepair {
    pub request: HistoryPageRequest,
    pub priority: RequestPriority,
}

impl CoverageSnapshot {
    /// Builds normalized coverage facts. Ranges may overlap and arrive unordered.
    ///
    /// # Errors
    /// Returns an error when any range is empty or reversed.
    pub fn try_new(
        complete: Vec<HistoryRange>,
        confirmed_empty: Vec<HistoryRange>,
        invalidated: Vec<HistoryRange>,
        quarantined: Vec<HistoryRange>,
    ) -> Result<Self, ProviderHistoryError> {
        Ok(Self {
            complete: normalize(complete)?,
            confirmed_empty: normalize(confirmed_empty)?,
            invalidated: normalize(invalidated)?,
            quarantined: normalize(quarantined)?,
        })
    }

    /// Classifies every part of a requested range and coalesces its repair work.
    ///
    /// Invalidated and quarantined facts take precedence over otherwise usable data.
    /// A range is `Partial` only when more than one classification is present.
    ///
    /// # Errors
    /// Returns an error when the requested range is empty or reversed.
    pub fn plan(&self, requested: HistoryRange) -> Result<CoveragePlan, ProviderHistoryError> {
        requested.span_nanos()?;
        let mut boundaries = vec![requested.start_unix_nanos, requested.end_unix_nanos];
        for range in self.all_ranges() {
            let start = range.start_unix_nanos.max(requested.start_unix_nanos);
            let end = range.end_unix_nanos.min(requested.end_unix_nanos);
            if start < end {
                boundaries.extend([start, end]);
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut spans: Vec<CoverageSpan> = Vec::new();
        for pair in boundaries.windows(2) {
            let range = HistoryRange {
                start_unix_nanos: pair[0],
                end_unix_nanos: pair[1],
            };
            let class = self.class_at(range);
            if let Some(previous) = spans.last_mut()
                && previous.class == class
                && previous.range.end_unix_nanos == range.start_unix_nanos
            {
                previous.range.end_unix_nanos = range.end_unix_nanos;
            } else {
                spans.push(CoverageSpan { range, class });
            }
        }
        let classification = spans.first().map_or(CoverageClass::Missing, |first| {
            if spans.iter().all(|span| span.class == first.class) {
                first.class
            } else {
                CoverageClass::Partial
            }
        });
        let repairs = coalesce(
            spans
                .iter()
                .filter(|span| needs_repair(span.class))
                .map(|span| span.range)
                .collect(),
        );
        Ok(CoveragePlan {
            classification,
            spans,
            repairs,
        })
    }

    fn all_ranges(&self) -> impl Iterator<Item = HistoryRange> + '_ {
        self.complete
            .iter()
            .chain(&self.confirmed_empty)
            .chain(&self.invalidated)
            .chain(&self.quarantined)
            .copied()
    }

    /// Returns normalized provider-confirmed empty ranges for durable recovery.
    #[must_use]
    pub fn confirmed_empty_ranges(&self) -> &[HistoryRange] {
        &self.confirmed_empty
    }

    fn class_at(&self, range: HistoryRange) -> CoverageClass {
        if covers(&self.invalidated, range) {
            CoverageClass::Invalidated
        } else if covers(&self.quarantined, range) {
            CoverageClass::Quarantined
        } else if covers(&self.complete, range) {
            CoverageClass::Complete
        } else if covers(&self.confirmed_empty, range) {
            CoverageClass::ConfirmedEmpty
        } else {
            CoverageClass::Missing
        }
    }
}

impl CoveragePlan {
    #[must_use]
    pub const fn classification(&self) -> CoverageClass {
        self.classification
    }

    #[must_use]
    pub fn spans(&self) -> &[CoverageSpan] {
        &self.spans
    }

    #[must_use]
    pub fn repair_ranges(&self) -> &[HistoryRange] {
        &self.repairs
    }

    pub(crate) fn prioritized_repairs(
        &self,
        base: &HistoryPageRequest,
        visible: HistoryRange,
    ) -> Vec<PrioritizedRepair> {
        let mut repairs = self
            .repairs
            .iter()
            .map(|range| {
                let mut request = base.clone();
                request.range = *range;
                request.continuation = None;
                PrioritizedRepair {
                    request,
                    priority: if overlaps(*range, visible) {
                        RequestPriority::Visible
                    } else {
                        RequestPriority::AdjacentPrefetch
                    },
                }
            })
            .collect::<Vec<_>>();
        repairs.sort_by_key(|repair| (repair.priority, repair.request.range));
        repairs
    }
}

fn normalize(ranges: Vec<HistoryRange>) -> Result<Vec<HistoryRange>, ProviderHistoryError> {
    for range in &ranges {
        range.span_nanos()?;
    }
    Ok(coalesce(ranges))
}

fn coalesce(mut ranges: Vec<HistoryRange>) -> Vec<HistoryRange> {
    ranges.sort_unstable();
    let mut merged: Vec<HistoryRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start_unix_nanos <= previous.end_unix_nanos
        {
            previous.end_unix_nanos = previous.end_unix_nanos.max(range.end_unix_nanos);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn covers(ranges: &[HistoryRange], candidate: HistoryRange) -> bool {
    ranges.iter().any(|range| {
        range.start_unix_nanos <= candidate.start_unix_nanos
            && range.end_unix_nanos >= candidate.end_unix_nanos
    })
}

const fn overlaps(left: HistoryRange, right: HistoryRange) -> bool {
    left.start_unix_nanos < right.end_unix_nanos && right.start_unix_nanos < left.end_unix_nanos
}

const fn needs_repair(class: CoverageClass) -> bool {
    matches!(
        class,
        CoverageClass::Missing | CoverageClass::Invalidated | CoverageClass::Quarantined
    )
}
