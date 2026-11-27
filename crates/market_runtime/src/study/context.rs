//! Deterministic execution metadata, read-only chart viewport context, and
//! non-repainting multi-timeframe / lower-timeframe request alignment helpers.

use super::StudyMarketSeries;
use aeris_market_data::BarPeriod;
use std::ops::Range;

/// Deterministic runtime-supplied execution metadata for one study invocation.
///
/// Studies must read bar/session timing from this contract rather than calling
/// ambient wall clocks so historical replay, live updates, and tests stay reproducible.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StudyExecutionMeta {
    /// `true` during initial or covering historical execution; `false` on live incremental waves.
    pub is_historical: bool,
    /// `true` when the primary series appended at least one new bar in this wave.
    pub is_new_bar: bool,
    /// `true` when the latest primary bar is confirmed (closed / not currently forming).
    pub is_confirmed_bar: bool,
    /// Deterministic evaluation timestamp in Unix nanoseconds.
    pub evaluation_time_unix_nanos: i64,
    /// Total row count on the primary output timeline.
    pub bar_count: usize,
    /// Primary market bar period when dependency 0 is a market series.
    pub bar_period: Option<BarPeriod>,
}

impl StudyExecutionMeta {
    /// Returns `true` when executing an incremental live update rather than a covering history pass.
    #[must_use]
    pub const fn is_realtime(self) -> bool {
        !self.is_historical
    }
}

/// Read-only visible chart window context supplied by the host for viewport-aware studies.
///
/// Ordinary studies ignore this context; studies that opt in via
/// [`super::NativeStudyProgram::with_chart_context`] receive deterministic recalculation
/// notifications when the visible window changes without gaining chart ownership.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StudyChartContext {
    pub visible_start_unix_nanos: Option<i64>,
    pub visible_end_unix_nanos: Option<i64>,
    pub visible_bar_count: Option<usize>,
}

impl StudyChartContext {
    /// Returns whether `timestamp_unix_nanos` falls inside the reported visible time window.
    #[must_use]
    pub fn contains_timestamp(self, timestamp_unix_nanos: i64) -> bool {
        match (self.visible_start_unix_nanos, self.visible_end_unix_nanos) {
            (Some(start), Some(end)) => {
                timestamp_unix_nanos >= start && timestamp_unix_nanos <= end
            }
            (Some(start), None) => timestamp_unix_nanos >= start,
            (None, Some(end)) => timestamp_unix_nanos <= end,
            (None, None) => false,
        }
    }
}

/// Confirmation semantics for higher-timeframe (HTF) requests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StudyMtfConfirmation {
    /// Only expose confirmed HTF bars (no lookahead/repainting on forming HTF bars).
    #[default]
    ConfirmedOnly,
    /// Expose the latest HTF bar whose open timestamp is `<= primary_ts`, including a forming tail.
    LatestIncludingForming,
}

/// Gap semantics for higher-timeframe (HTF) requests (Pine `barmerge.gaps_off` vs `gaps_on`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StudyMtfGapPolicy {
    /// Carry the resolved HTF bar forward across every intervening primary bar.
    #[default]
    CarryForward,
    /// Emit `Some(index)` only on the first primary bar that observes a newly available HTF bar.
    Gaps,
}

/// Reproducible higher-timeframe alignment policy for `align_higher_timeframe_bar`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StudyRequestContext {
    pub confirmation: StudyMtfConfirmation,
    pub gaps: StudyMtfGapPolicy,
}

/// Fixed duration in nanoseconds for deterministic clock-based `BarPeriod` values.
fn fixed_period_nanos(period: BarPeriod) -> Option<i64> {
    match period {
        BarPeriod::Time { seconds } => i64::from(seconds).checked_mul(1_000_000_000),
        _ => None,
    }
}

fn resolve_htf_index_at(
    primary_timestamp_unix_nanos: i64,
    htf: &StudyMarketSeries,
    confirmation: StudyMtfConfirmation,
) -> Option<usize> {
    let bars = htf.bars();
    if bars.is_empty() {
        return None;
    }
    let count = bars
        .partition_point(|bar| bar.exchange_timestamp_unix_nanos <= primary_timestamp_unix_nanos);
    let mut candidate = count.checked_sub(1)?;

    if confirmation == StudyMtfConfirmation::ConfirmedOnly {
        let is_forming_tail = candidate + 1 == bars.len() && htf.forming();
        let bar = bars.get(candidate)?;
        let still_open_by_clock = fixed_period_nanos(htf.series().period).is_some_and(|dur| {
            bar.exchange_timestamp_unix_nanos
                .checked_add(dur)
                .is_some_and(|close_ns| close_ns > primary_timestamp_unix_nanos)
        });
        if is_forming_tail || still_open_by_clock {
            candidate = candidate.checked_sub(1)?;
        }
    }
    Some(candidate)
}

/// Aligns one primary-series timestamp to a higher-timeframe `StudyMarketSeries` bar index
/// with explicit confirmation and gap semantics.
///
/// With `StudyMtfConfirmation::ConfirmedOnly` (the default), a higher-timeframe bar is never
/// exposed before it closes, preventing accidental lookahead and historical repainting.
#[must_use]
pub fn align_higher_timeframe_bar(
    primary_timestamp_unix_nanos: i64,
    previous_primary_timestamp_unix_nanos: Option<i64>,
    htf: &StudyMarketSeries,
    request: StudyRequestContext,
) -> Option<usize> {
    let current = resolve_htf_index_at(primary_timestamp_unix_nanos, htf, request.confirmation)?;
    if request.gaps == StudyMtfGapPolicy::Gaps
        && let Some(prev_ts) = previous_primary_timestamp_unix_nanos
        && resolve_htf_index_at(prev_ts, htf, request.confirmation) == Some(current)
    {
        return None;
    }
    Some(current)
}

/// Returns the half-open index range `[start, end)` of lower-timeframe (intrabar) bars
/// whose exchange timestamps lie inside `[bar_start_unix_nanos, bar_end_exclusive_unix_nanos)`.
///
/// Runs in `O(log N)` without allocating.
#[must_use]
pub fn intrabar_range(
    bar_start_unix_nanos: i64,
    bar_end_exclusive_unix_nanos: i64,
    ltf: &StudyMarketSeries,
) -> Range<usize> {
    if bar_start_unix_nanos >= bar_end_exclusive_unix_nanos {
        return 0..0;
    }
    let bars = ltf.bars();
    let start =
        bars.partition_point(|bar| bar.exchange_timestamp_unix_nanos < bar_start_unix_nanos);
    let end = bars
        .partition_point(|bar| bar.exchange_timestamp_unix_nanos < bar_end_exclusive_unix_nanos);
    start..end
}
