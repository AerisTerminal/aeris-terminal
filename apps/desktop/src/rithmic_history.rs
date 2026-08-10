pub(crate) use axiusflow_desktop_market_runtime::rithmic_series::{
    RithmicSeries, RithmicSeriesRequest,
};
use std::num::NonZeroUsize;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RithmicSeriesBrowser {
    next_generation: usize,
    pending: Option<RithmicSeriesRequest>,
    selected: Option<RithmicSeriesRequest>,
}

impl RithmicSeriesBrowser {
    pub(crate) fn select(
        &mut self,
        selection_generation: NonZeroUsize,
        series: RithmicSeries,
    ) -> RithmicSeriesRequest {
        self.next_generation = self.next_generation.saturating_add(1).max(1);
        let request = RithmicSeriesRequest {
            selection_generation,
            series_generation: NonZeroUsize::new(self.next_generation).unwrap_or(NonZeroUsize::MIN),
            series,
        };
        self.pending = Some(request);
        request
    }

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
    use super::{RithmicSeries, RithmicSeriesBrowser};
    use std::num::NonZeroUsize;

    #[test]
    fn series_browser_fences_replaced_selection_and_series_generations() {
        let mut browser = RithmicSeriesBrowser::default();
        let selection = NonZeroUsize::MIN;
        let first = browser.select(selection, RithmicSeries::Minute1);
        assert_eq!(browser.pending(), Some(first));
        let second = browser.select(selection, RithmicSeries::Minute5);
        assert_eq!(browser.pending(), Some(second));
        assert!(!browser.accept(first.selection_generation, first.series_generation));
        assert!(browser.accept(second.selection_generation, second.series_generation));
        let replacement = browser.select(
            NonZeroUsize::new(2).expect("selection generation is nonzero"),
            RithmicSeries::Daily,
        );
        assert!(!browser.accept(second.selection_generation, second.series_generation));
        assert!(browser.accept(
            replacement.selection_generation,
            replacement.series_generation
        ));
        browser.reset();
        assert_eq!(browser.pending(), None);
        assert_eq!(browser.selected(), None);
    }
}
