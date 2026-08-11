use axiusflow_market_data::{ChartAggregation, ChartInterval};
use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSeries(ChartInterval);

#[allow(non_upper_case_globals)]
#[allow(dead_code)]
impl RithmicSeries {
    pub(crate) const Tick: Self = Self(ChartInterval::Tick100);
    pub(crate) const Minute1: Self = Self(ChartInterval::Minute1);
    pub(crate) const Minute3: Self = Self(ChartInterval::Minute3);
    pub(crate) const Minute5: Self = Self(ChartInterval::Minute5);
    pub(crate) const Minute15: Self = Self(ChartInterval::Minute15);
    pub(crate) const Minute30: Self = Self(ChartInterval::Minute30);
    pub(crate) const Hour1: Self = Self(ChartInterval::Hour1);
    pub(crate) const Hour2: Self = Self(ChartInterval::Hour2);
    pub(crate) const Hour4: Self = Self(ChartInterval::Hour4);
    pub(crate) const Hour8: Self = Self(ChartInterval::Hour8);
    pub(crate) const Hour12: Self = Self(ChartInterval::Hour12);
    pub(crate) const Daily: Self = Self(ChartInterval::Day1);
    pub(crate) const Day3: Self = Self(ChartInterval::Day3);
    pub(crate) const Week1: Self = Self(ChartInterval::Week1);
    pub(crate) const Month1: Self = Self(ChartInterval::Month1);

    pub(crate) const ALL: [Self; 15] = [
        Self::Tick,
        Self::Minute1,
        Self::Minute3,
        Self::Minute5,
        Self::Minute15,
        Self::Minute30,
        Self::Hour1,
        Self::Hour2,
        Self::Hour4,
        Self::Hour8,
        Self::Hour12,
        Self::Daily,
        Self::Day3,
        Self::Week1,
        Self::Month1,
    ];

    pub(crate) const fn label(self) -> &'static str {
        self.0.label()
    }

    pub(crate) const fn interval(self) -> ChartInterval {
        self.0
    }

    pub(crate) fn supports_native_history(self) -> bool {
        self.0.rithmic_aggregation().is_some()
    }

    pub(crate) fn interval_seconds(self) -> Option<u64> {
        match self.interval().aggregation() {
            ChartAggregation::FixedSeconds(seconds) => Some(u64::from(seconds.get())),
            ChartAggregation::CalendarMonth => Some(30 * 24 * 60 * 60),
            ChartAggregation::Trades(_) => None,
        }
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
