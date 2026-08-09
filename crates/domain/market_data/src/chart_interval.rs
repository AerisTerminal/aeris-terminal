use core::fmt;
use std::num::{NonZeroU16, NonZeroU32};

/// Provider-neutral interval selected by chart chrome.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChartInterval {
    Tick100,
    Minute1,
    Minute3,
    Minute5,
    Minute15,
    Minute30,
    Hour1,
    Hour2,
    Hour4,
    Hour8,
    Hour12,
    Day1,
    Day3,
    Week1,
    Month1,
}

impl ChartInterval {
    pub const ALL: [Self; 15] = [
        Self::Tick100,
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
        Self::Day1,
        Self::Day3,
        Self::Week1,
        Self::Month1,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tick100 => "100t",
            Self::Minute1 => "1m",
            Self::Minute3 => "3m",
            Self::Minute5 => "5m",
            Self::Minute15 => "15m",
            Self::Minute30 => "30m",
            Self::Hour1 => "1h",
            Self::Hour2 => "2h",
            Self::Hour4 => "4h",
            Self::Hour8 => "8h",
            Self::Hour12 => "12h",
            Self::Day1 => "1D",
            Self::Day3 => "3D",
            Self::Week1 => "1W",
            Self::Month1 => "1M",
        }
    }

    #[must_use]
    pub fn aggregation(self) -> ChartAggregation {
        match self {
            Self::Tick100 => {
                ChartAggregation::Trades(NonZeroU32::new(100).unwrap_or(NonZeroU32::MIN))
            }
            Self::Minute1 => fixed_seconds(60),
            Self::Minute3 => fixed_seconds(3 * 60),
            Self::Minute5 => fixed_seconds(5 * 60),
            Self::Minute15 => fixed_seconds(15 * 60),
            Self::Minute30 => fixed_seconds(30 * 60),
            Self::Hour1 => fixed_seconds(60 * 60),
            Self::Hour2 => fixed_seconds(2 * 60 * 60),
            Self::Hour4 => fixed_seconds(4 * 60 * 60),
            Self::Hour8 => fixed_seconds(8 * 60 * 60),
            Self::Hour12 => fixed_seconds(12 * 60 * 60),
            Self::Day1 => fixed_seconds(24 * 60 * 60),
            Self::Day3 => fixed_seconds(3 * 24 * 60 * 60),
            Self::Week1 => fixed_seconds(7 * 24 * 60 * 60),
            Self::Month1 => ChartAggregation::CalendarMonth,
        }
    }

    /// Rithmic request or deterministic daily-session aggregation required by this interval.
    #[must_use]
    pub fn rithmic_aggregation(self) -> Option<RithmicChartAggregation> {
        match self {
            Self::Tick100 => Some(RithmicChartAggregation::Trades {
                trades_per_bar: NonZeroU16::new(100).unwrap_or(NonZeroU16::MIN),
            }),
            Self::Minute1 => Some(rithmic_time(RithmicTimeUnit::Minute, 1)),
            Self::Minute3 => Some(rithmic_time(RithmicTimeUnit::Minute, 3)),
            Self::Minute5 => Some(rithmic_time(RithmicTimeUnit::Minute, 5)),
            Self::Minute15 => Some(rithmic_time(RithmicTimeUnit::Minute, 15)),
            Self::Minute30 => Some(rithmic_time(RithmicTimeUnit::Minute, 30)),
            Self::Hour1 => Some(rithmic_time(RithmicTimeUnit::Minute, 60)),
            Self::Hour2 => Some(rithmic_time(RithmicTimeUnit::Minute, 120)),
            Self::Hour4 => Some(rithmic_time(RithmicTimeUnit::Minute, 240)),
            Self::Hour8 => Some(rithmic_time(RithmicTimeUnit::Minute, 480)),
            Self::Hour12 => Some(rithmic_time(RithmicTimeUnit::Minute, 720)),
            Self::Day1 => Some(rithmic_time(RithmicTimeUnit::Day, 1)),
            Self::Day3 => Some(rithmic_time(RithmicTimeUnit::Day, 3)),
            Self::Week1 => Some(RithmicChartAggregation::DailySessions {
                period: RithmicDailyAggregation::Week,
            }),
            Self::Month1 => Some(RithmicChartAggregation::DailySessions {
                period: RithmicDailyAggregation::Month,
            }),
        }
    }
}

impl fmt::Display for ChartInterval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartAggregation {
    Trades(NonZeroU32),
    FixedSeconds(NonZeroU32),
    CalendarMonth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicTimeUnit {
    Minute,
    Day,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicDailyAggregation {
    Week,
    Month,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicChartAggregation {
    Trades {
        trades_per_bar: NonZeroU16,
    },
    Time {
        unit: RithmicTimeUnit,
        period: NonZeroU16,
    },
    DailySessions {
        period: RithmicDailyAggregation,
    },
}

fn fixed_seconds(seconds: u32) -> ChartAggregation {
    ChartAggregation::FixedSeconds(NonZeroU32::new(seconds).unwrap_or(NonZeroU32::MIN))
}

fn rithmic_time(unit: RithmicTimeUnit, period: u16) -> RithmicChartAggregation {
    RithmicChartAggregation::Time {
        unit,
        period: NonZeroU16::new(period).unwrap_or(NonZeroU16::MIN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_legacy_intervals_plus_rithmic_ticks() {
        assert_eq!(ChartInterval::ALL.len(), 15);
        assert_eq!(ChartInterval::ALL[0].label(), "100t");
        assert_eq!(ChartInterval::ALL[1].label(), "1m");
        assert_eq!(ChartInterval::ALL[14].label(), "1M");
    }

    #[test]
    fn provider_neutral_aggregation_preserves_tick_and_calendar_boundaries() {
        assert_eq!(
            ChartInterval::Tick100.aggregation(),
            ChartAggregation::Trades(NonZeroU32::new(100).unwrap())
        );
        assert_eq!(
            ChartInterval::Hour12.aggregation(),
            ChartAggregation::FixedSeconds(NonZeroU32::new(43_200).unwrap())
        );
        assert_eq!(
            ChartInterval::Month1.aggregation(),
            ChartAggregation::CalendarMonth
        );
    }

    #[test]
    fn rithmic_capabilities_cover_every_exact_native_interval() {
        assert_eq!(
            ChartInterval::Tick100.rithmic_aggregation(),
            Some(RithmicChartAggregation::Trades {
                trades_per_bar: NonZeroU16::new(100).unwrap()
            })
        );
        assert_eq!(
            ChartInterval::Hour12.rithmic_aggregation(),
            Some(RithmicChartAggregation::Time {
                unit: RithmicTimeUnit::Minute,
                period: NonZeroU16::new(720).unwrap()
            })
        );
        assert_eq!(
            ChartInterval::Week1.rithmic_aggregation(),
            Some(RithmicChartAggregation::DailySessions {
                period: RithmicDailyAggregation::Week
            })
        );
        assert_eq!(
            ChartInterval::Month1.rithmic_aggregation(),
            Some(RithmicChartAggregation::DailySessions {
                period: RithmicDailyAggregation::Month
            })
        );
    }
}
