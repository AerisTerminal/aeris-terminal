use axiusflow_market_data::ChartInterval;
use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSeries(ChartInterval);

#[allow(non_upper_case_globals)]
impl RithmicSeries {
    pub(crate) const Minute1: Self = Self(ChartInterval::Minute1);

    pub(crate) const fn label(self) -> &'static str {
        self.0.label()
    }

    pub(crate) const fn interval(self) -> ChartInterval {
        self.0
    }

    pub(crate) fn supports_native_history(self) -> bool {
        self.0.rithmic_aggregation().is_some()
    }
}

impl From<ChartInterval> for RithmicSeries {
    fn from(interval: ChartInterval) -> Self {
        Self(interval)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSeriesRequest {
    pub(crate) selection_generation: NonZeroUsize,
    pub(crate) series_generation: NonZeroUsize,
    pub(crate) series: RithmicSeries,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RithmicSeriesBrowser {
    pending: Option<RithmicSeriesRequest>,
    selected: Option<RithmicSeriesRequest>,
}

impl RithmicSeriesBrowser {
    pub(crate) fn accept(
        &mut self,
        selection_generation: NonZeroUsize,
        series_generation: NonZeroUsize,
    ) -> bool {
        let Some(request) = self.pending.take_if(|request| {
            request.selection_generation == selection_generation
                && request.series_generation == series_generation
        }) else {
            return false;
        };
        self.selected = Some(request);
        true
    }

    pub(crate) fn reject(&mut self, series_generation: NonZeroUsize) -> bool {
        self.pending
            .take_if(|request| request.series_generation == series_generation)
            .is_some()
    }

    pub(crate) fn reset(&mut self) {
        self.pending = None;
        self.selected = None;
    }

    pub(crate) const fn selected(&self) -> Option<RithmicSeriesRequest> {
        self.selected
    }

    pub(crate) const fn pending(&self) -> Option<RithmicSeriesRequest> {
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::{RithmicSeries, RithmicSeriesBrowser, RithmicSeriesRequest};
    use axiusflow_market_data::ChartInterval;
    use std::num::NonZeroUsize;

    fn request(series: RithmicSeries, series_generation: usize) -> RithmicSeriesRequest {
        RithmicSeriesRequest {
            selection_generation: NonZeroUsize::MIN,
            series_generation: NonZeroUsize::new(series_generation).unwrap_or(NonZeroUsize::MIN),
            series,
        }
    }

    /// Only the exact pending generations complete a selection, so a stale
    /// history reply can never swap the chart.
    #[test]
    fn mismatched_generations_do_not_complete_selection() {
        let mut browser = RithmicSeriesBrowser {
            pending: Some(request(RithmicSeries::Minute1, 3)),
            ..RithmicSeriesBrowser::default()
        };

        assert!(!browser.accept(
            NonZeroUsize::MIN,
            NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN)
        ));
        assert_eq!(browser.pending(), Some(request(RithmicSeries::Minute1, 3)));
        assert!(browser.accept(
            NonZeroUsize::MIN,
            NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN)
        ));
        assert_eq!(
            browser.selected().map(|request| request.series),
            Some(RithmicSeries::Minute1)
        );
    }

    /// Rejecting clears only the matching request; anything else stays
    /// pending so a superseded reply cannot strand the browser.
    #[test]
    fn reject_clears_only_the_matching_request() {
        let mut browser = RithmicSeriesBrowser {
            pending: Some(request(RithmicSeries::from(ChartInterval::Minute5), 4)),
            ..RithmicSeriesBrowser::default()
        };

        assert!(!browser.reject(NonZeroUsize::new(5).unwrap_or(NonZeroUsize::MIN)));
        assert_eq!(
            browser.pending(),
            Some(request(RithmicSeries::from(ChartInterval::Minute5), 4))
        );
        assert!(browser.reject(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN)));
        assert_eq!(browser.pending(), None);
    }

    /// Reset drops both pending and selected state for session retirement.
    #[test]
    fn reset_clears_pending_and_selected_state() {
        let mut browser = RithmicSeriesBrowser {
            pending: Some(request(RithmicSeries::Minute1, 3)),
            selected: Some(request(RithmicSeries::Minute1, 2)),
        };
        browser.reset();
        assert_eq!(browser.pending(), None);
        assert_eq!(browser.selected(), None);
    }
}
