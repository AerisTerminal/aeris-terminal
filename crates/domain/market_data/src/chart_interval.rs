use core::fmt;
use std::num::{NonZeroU16, NonZeroU32};

/// Provider-neutral interval selected by chart chrome.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChartInterval {
    Tick100,
    Second1,
    Second5,
    Second10,
    Second15,
    Second30,
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
    pub const ALL: [Self; 20] = [
        Self::Tick100,
        Self::Second1,
        Self::Second5,
        Self::Second10,
        Self::Second15,
        Self::Second30,
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
            Self::Second1 => "1s",
            Self::Second5 => "5s",
            Self::Second10 => "10s",
            Self::Second15 => "15s",
            Self::Second30 => "30s",
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

    /// The interval in `offered` nearest to this one on the [`Self::ALL`] ladder: this interval
    /// when it is offered, otherwise the closest step, the finer one on a tie. A seconds chart
    /// therefore opens at one minute on a provider whose bars start there.
    #[must_use]
    pub fn nearest_offered(self, offered: &[Self]) -> Option<Self> {
        let rank = |interval: Self| {
            Self::ALL
                .iter()
                .position(|candidate| *candidate == interval)
                .unwrap_or_default()
        };
        let own = rank(self);
        offered
            .iter()
            .copied()
            .min_by_key(|candidate| (rank(*candidate).abs_diff(own), rank(*candidate)))
    }

    /// Returns whether this interval matches a type-ahead query such as `1`, `1m`, or `1M`.
    ///
    /// Minutes use a lowercase `m` and months use an uppercase `M`. Other units match
    /// case-insensitively against the catalog label (`h`, `D`, `W`, `t`).
    #[must_use]
    pub fn matches_typeahead(self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let Some((digits, unit)) = split_typeahead_query(query) else {
            return false;
        };
        let (period, interval_unit) = self.typeahead_parts();
        let period_text = period.to_string();
        match unit {
            None => period_text.starts_with(digits),
            Some(unit) => period_text == digits && typeahead_unit_matches(interval_unit, unit),
        }
    }

    const fn typeahead_parts(self) -> (u16, char) {
        match self {
            Self::Tick100 => (100, 't'),
            Self::Second1 => (1, 's'),
            Self::Second5 => (5, 's'),
            Self::Second10 => (10, 's'),
            Self::Second15 => (15, 's'),
            Self::Second30 => (30, 's'),
            Self::Minute1 => (1, 'm'),
            Self::Minute3 => (3, 'm'),
            Self::Minute5 => (5, 'm'),
            Self::Minute15 => (15, 'm'),
            Self::Minute30 => (30, 'm'),
            Self::Hour1 => (1, 'h'),
            Self::Hour2 => (2, 'h'),
            Self::Hour4 => (4, 'h'),
            Self::Hour8 => (8, 'h'),
            Self::Hour12 => (12, 'h'),
            Self::Day1 => (1, 'D'),
            Self::Day3 => (3, 'D'),
            Self::Week1 => (1, 'W'),
            Self::Month1 => (1, 'M'),
        }
    }

    #[must_use]
    pub fn aggregation(self) -> ChartAggregation {
        match self {
            Self::Tick100 => {
                ChartAggregation::Trades(NonZeroU32::new(100).unwrap_or(NonZeroU32::MIN))
            }
            Self::Second1 => fixed_seconds(1),
            Self::Second5 => fixed_seconds(5),
            Self::Second10 => fixed_seconds(10),
            Self::Second15 => fixed_seconds(15),
            Self::Second30 => fixed_seconds(30),
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
            Self::Second1 => Some(rithmic_time(RithmicTimeUnit::Second, 1)),
            Self::Second5 => Some(rithmic_time(RithmicTimeUnit::Second, 5)),
            Self::Second10 => Some(rithmic_time(RithmicTimeUnit::Second, 10)),
            Self::Second15 => Some(rithmic_time(RithmicTimeUnit::Second, 15)),
            Self::Second30 => Some(rithmic_time(RithmicTimeUnit::Second, 30)),
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
    Second,
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

fn split_typeahead_query(query: &str) -> Option<(&str, Option<char>)> {
    let digit_end = query.bytes().take_while(u8::is_ascii_digit).count();
    if digit_end == 0 {
        return None;
    }
    let (digits, rest) = query.split_at(digit_end);
    match rest.chars().count() {
        0 => Some((digits, None)),
        1 => Some((digits, rest.chars().next())),
        _ => None,
    }
}

const fn typeahead_unit_matches(interval_unit: char, typed: char) -> bool {
    match typed {
        's' | 'S' => interval_unit == 's',
        'm' => interval_unit == 'm',
        'M' => interval_unit == 'M',
        'h' | 'H' => interval_unit == 'h',
        'd' | 'D' => interval_unit == 'D',
        'w' | 'W' => interval_unit == 'W',
        't' | 'T' => interval_unit == 't',
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_legacy_intervals_plus_rithmic_ticks() {
        assert_eq!(ChartInterval::ALL.len(), 20);
        assert_eq!(ChartInterval::ALL[0].label(), "100t");
        assert_eq!(ChartInterval::ALL[1].label(), "1s");
        assert_eq!(ChartInterval::ALL[5].label(), "30s");
        assert_eq!(ChartInterval::ALL[6].label(), "1m");
        assert_eq!(ChartInterval::ALL[19].label(), "1M");
    }

    #[test]
    fn unoffered_intervals_fall_back_to_the_nearest_offered_step() {
        use ChartInterval::{
            Day1, Day3, Hour1, Minute1, Minute5, Month1, Second15, Second30, Tick100, Week1,
        };
        let minute_bars = [Minute1, Minute5, Hour1, Day1, Week1, Month1];
        assert_eq!(Second15.nearest_offered(&minute_bars), Some(Minute1));
        assert_eq!(Second30.nearest_offered(&minute_bars), Some(Minute1));
        assert_eq!(Tick100.nearest_offered(&minute_bars), Some(Minute1));
        assert_eq!(Minute5.nearest_offered(&minute_bars), Some(Minute5));
        assert_eq!(Day3.nearest_offered(&minute_bars), Some(Day1));
        assert_eq!(Second30.nearest_offered(&[]), None);
    }

    #[test]
    fn second_intervals_are_fixed_seconds_and_native_rithmic_second_bars() {
        for (interval, seconds) in [
            (ChartInterval::Second1, 1_u16),
            (ChartInterval::Second5, 5),
            (ChartInterval::Second10, 10),
            (ChartInterval::Second15, 15),
            (ChartInterval::Second30, 30),
        ] {
            assert_eq!(
                interval.aggregation(),
                ChartAggregation::FixedSeconds(NonZeroU32::from(NonZeroU16::new(seconds).unwrap()))
            );
            assert_eq!(
                interval.rithmic_aggregation(),
                Some(RithmicChartAggregation::Time {
                    unit: RithmicTimeUnit::Second,
                    period: NonZeroU16::new(seconds).unwrap()
                })
            );
            assert_eq!(interval.label(), format!("{seconds}s"));
        }
        assert!(ChartInterval::Second5.matches_typeahead("5s"));
        assert!(ChartInterval::Second5.matches_typeahead("5S"));
        assert!(!ChartInterval::Second5.matches_typeahead("5m"));
        assert!(!ChartInterval::Minute5.matches_typeahead("5s"));
    }

    #[test]
    fn typeahead_keeps_minutes_and_months_case_sensitive() {
        assert!(ChartInterval::Minute1.matches_typeahead("1"));
        assert!(ChartInterval::Minute1.matches_typeahead("1m"));
        assert!(!ChartInterval::Minute1.matches_typeahead("1M"));
        assert!(ChartInterval::Month1.matches_typeahead("1M"));
        assert!(!ChartInterval::Month1.matches_typeahead("1m"));
        assert!(ChartInterval::Minute15.matches_typeahead("1"));
        assert!(ChartInterval::Minute15.matches_typeahead("15"));
        assert!(ChartInterval::Minute15.matches_typeahead("15m"));
        assert!(!ChartInterval::Minute15.matches_typeahead("1m"));
        assert!(ChartInterval::Hour1.matches_typeahead("1h"));
        assert!(ChartInterval::Hour1.matches_typeahead("1H"));
        assert!(ChartInterval::Week1.matches_typeahead("1w"));
        assert!(ChartInterval::Week1.matches_typeahead("1W"));
        assert!(ChartInterval::Tick100.matches_typeahead("100t"));
        assert!(!ChartInterval::Minute5.matches_typeahead("m"));
        assert!(!ChartInterval::Minute1.matches_typeahead("1mx"));
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
