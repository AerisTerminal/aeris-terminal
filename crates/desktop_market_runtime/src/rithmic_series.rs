use axiusflow_market_data::{
    ChartAggregation, ChartInterval, RithmicChartAggregation, RithmicTimeUnit,
};
use axiusflow_rithmic_protocol_adapter::{RithmicTimeBarResolution, TimeBarType};
use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicSeries(ChartInterval);

#[allow(non_upper_case_globals)]
#[allow(dead_code)]
impl RithmicSeries {
    pub const Tick: Self = Self(ChartInterval::Tick100);
    pub const Minute1: Self = Self(ChartInterval::Minute1);
    pub const Minute3: Self = Self(ChartInterval::Minute3);
    pub const Minute5: Self = Self(ChartInterval::Minute5);
    pub const Minute15: Self = Self(ChartInterval::Minute15);
    pub const Minute30: Self = Self(ChartInterval::Minute30);
    pub const Hour1: Self = Self(ChartInterval::Hour1);
    pub const Hour2: Self = Self(ChartInterval::Hour2);
    pub const Hour4: Self = Self(ChartInterval::Hour4);
    pub const Hour8: Self = Self(ChartInterval::Hour8);
    pub const Hour12: Self = Self(ChartInterval::Hour12);
    pub const Daily: Self = Self(ChartInterval::Day1);
    pub const Day3: Self = Self(ChartInterval::Day3);
    pub const Week1: Self = Self(ChartInterval::Week1);
    pub const Month1: Self = Self(ChartInterval::Month1);

    pub const ALL: [Self; 15] = [
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

    #[must_use]
    pub const fn label(self) -> &'static str {
        self.0.label()
    }

    #[must_use]
    pub const fn interval(self) -> ChartInterval {
        self.0
    }

    #[must_use]
    pub fn supports_native_history(self) -> bool {
        self.0.rithmic_aggregation().is_some()
    }

    #[must_use]
    pub fn interval_seconds(self) -> Option<u64> {
        match self.interval().aggregation() {
            ChartAggregation::FixedSeconds(seconds) => Some(u64::from(seconds.get())),
            ChartAggregation::CalendarMonth => Some(30 * 24 * 60 * 60),
            ChartAggregation::Trades(_) => None,
        }
    }

    /// Resolves the exact provider time-bar resolution.
    ///
    /// # Errors
    /// Returns an error when the series has no exact native history resolution.
    pub fn resolution(self) -> Result<RithmicTimeBarResolution, String> {
        if !self.supports_native_history() {
            return Err(format!(
                "{} has no exact Rithmic time-bar resolution",
                self.label()
            ));
        }
        let Some(RithmicChartAggregation::Time { unit, period }) =
            self.interval().rithmic_aggregation()
        else {
            return Err(format!(
                "{} has no exact Rithmic time-bar resolution",
                self.label()
            ));
        };
        let bar_type = match unit {
            RithmicTimeUnit::Minute => TimeBarType::Minute,
            RithmicTimeUnit::Day => TimeBarType::Daily,
        };
        RithmicTimeBarResolution::try_new(self.label(), bar_type, period)
            .map_err(|_| "Rithmic series is unavailable".to_string())
    }
}

impl From<ChartInterval> for RithmicSeries {
    fn from(interval: ChartInterval) -> Self {
        Self(interval)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicSeriesRequest {
    pub selection_generation: NonZeroUsize,
    pub series_generation: NonZeroUsize,
    pub series: RithmicSeries,
}
