//! Report periods and the chart time zone that dates them.

use aeris_chart_integration::AerisChartView;
use aeris_localization::{panels, tr};
use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime};

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
/// A century of days; longer custom periods report the offsets of their first century.
const MAXIMUM_OFFSET_DAYS: usize = 36_525;

/// The span of exit times a report covers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ReportPeriod {
    #[default]
    LastSevenDays,
    ThisWeek,
    ThisMonth,
    ThisYear,
    Custom,
}

impl ReportPeriod {
    pub(super) const ALL: [Self; 5] = [
        Self::LastSevenDays,
        Self::ThisWeek,
        Self::ThisMonth,
        Self::ThisYear,
        Self::Custom,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::LastSevenDays => tr!(panels::REPORT_PERIOD_7D),
            Self::ThisWeek => tr!(panels::REPORT_PERIOD_WEEK),
            Self::ThisMonth => tr!(panels::REPORT_PERIOD_MONTH),
            Self::ThisYear => tr!(panels::REPORT_PERIOD_YEAR),
            Self::Custom => tr!(panels::REPORT_PERIOD_CUSTOM),
        }
    }
}

/// Wall time in the chart's selected IANA zone. Trade times stay UTC; only grouping by day,
/// weekday and hour and the printed times use the zone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReportZone {
    id: &'static str,
}

impl ReportZone {
    pub(super) const fn new(id: &'static str) -> Self {
        Self { id }
    }

    pub(super) const fn id(self) -> &'static str {
        self.id
    }

    fn offset_seconds(self, utc_seconds: i64) -> i64 {
        AerisChartView::time_zone_offset_seconds(self.id, utc_seconds).map_or(0, i64::from)
    }

    /// The zone's offset from UTC, in seconds, at a UTC instant.
    pub(super) fn offset_at(self, unix_nanos: i64) -> i64 {
        self.offset_seconds(unix_nanos.div_euclid(NANOS_PER_SECOND))
    }

    /// Every distinct offset the zone uses during the span's days, in order of first use:
    /// one for most periods, two or more when the period crosses a daylight-saving change.
    pub(super) fn offsets_during(self, span: ReportSpan) -> Vec<i64> {
        let mut offsets = Vec::new();
        for day in span
            .first_day
            .iter_days()
            .take_while(|day| *day <= span.last_day)
            .take(MAXIMUM_OFFSET_DAYS)
        {
            let start = self.midnight_unix_nanos(day);
            let end = day.succ_opt().map_or(start, |next| {
                self.midnight_unix_nanos(next)
                    .saturating_sub(NANOS_PER_SECOND)
            });
            for instant in [start, end] {
                let offset = self.offset_at(instant);
                if !offsets.contains(&offset) {
                    offsets.push(offset);
                }
            }
        }
        offsets
    }

    /// The local calendar date and time of a UTC instant.
    pub(super) fn local(self, unix_nanos: i64) -> NaiveDateTime {
        let utc_seconds = unix_nanos.div_euclid(NANOS_PER_SECOND);
        let local_seconds = utc_seconds.saturating_add(self.offset_seconds(utc_seconds));
        chrono::DateTime::from_timestamp(local_seconds, 0)
            .map(|time| time.naive_utc())
            .unwrap_or_default()
    }

    /// The UTC instant of local midnight starting `date`. The offset is looked up at the
    /// instant it produces, so a day starting on the far side of a DST change still lands on
    /// its own midnight.
    pub(super) fn midnight_unix_nanos(self, date: NaiveDate) -> i64 {
        let local_seconds = date.and_time(NaiveTime::MIN).and_utc().timestamp();
        let first_guess = local_seconds.saturating_sub(self.offset_seconds(local_seconds));
        local_seconds
            .saturating_sub(self.offset_seconds(first_guess))
            .saturating_mul(NANOS_PER_SECOND)
    }
}

/// Exit-time bounds of one report: start inclusive, end exclusive, and the local days shown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReportSpan {
    pub(super) start_unix_nanos: i64,
    pub(super) end_unix_nanos: i64,
    pub(super) first_day: NaiveDate,
    pub(super) last_day: NaiveDate,
}

/// Why custom dates cannot form a period.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PeriodProblem {
    FromDate,
    ToDate,
    Reversed,
}

impl PeriodProblem {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::FromDate => tr!(panels::REPORT_PROBLEM_FROM),
            Self::ToDate => tr!(panels::REPORT_PROBLEM_TO),
            Self::Reversed => tr!(panels::REPORT_PROBLEM_REVERSED),
        }
    }
}

/// A custom period's first and last day as the user typed them.
pub(super) struct CustomDates<'a> {
    pub(super) from: &'a str,
    pub(super) to: &'a str,
}

/// Formats a date the way the custom period fields read it back.
/// A UTC offset as `UTC+05:30` or `UTC-04:00`.
pub(super) fn offset_text(offset_seconds: i64) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let minutes = offset_seconds.unsigned_abs() / 60;
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

pub(super) fn date_text(date: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

/// Resolves `period` at `now_unix_nanos`. Preset periods end now; a custom period covers its
/// first and last local days completely.
pub(super) fn resolve_span(
    period: ReportPeriod,
    custom: &CustomDates<'_>,
    now_unix_nanos: i64,
    zone: ReportZone,
) -> Result<ReportSpan, PeriodProblem> {
    let today = zone.local(now_unix_nanos).date();
    let end_now = now_unix_nanos.saturating_add(1);
    let from_day = |first_day: NaiveDate| ReportSpan {
        start_unix_nanos: zone.midnight_unix_nanos(first_day),
        end_unix_nanos: end_now,
        first_day,
        last_day: today,
    };
    Ok(match period {
        ReportPeriod::LastSevenDays => {
            let start_unix_nanos =
                now_unix_nanos.saturating_sub(7 * SECONDS_PER_DAY * NANOS_PER_SECOND);
            ReportSpan {
                start_unix_nanos,
                end_unix_nanos: end_now,
                first_day: zone.local(start_unix_nanos).date(),
                last_day: today,
            }
        }
        ReportPeriod::ThisWeek => from_day(
            today
                .checked_sub_days(Days::new(u64::from(today.weekday().num_days_from_monday())))
                .unwrap_or(today),
        ),
        ReportPeriod::ThisMonth => from_day(today.with_day(1).unwrap_or(today)),
        ReportPeriod::ThisYear => {
            from_day(NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap_or(today))
        }
        ReportPeriod::Custom => {
            let first_day = parse_date(custom.from).ok_or(PeriodProblem::FromDate)?;
            let last_day = parse_date(custom.to).ok_or(PeriodProblem::ToDate)?;
            if first_day > last_day {
                return Err(PeriodProblem::Reversed);
            }
            let after_last = last_day.succ_opt().ok_or(PeriodProblem::ToDate)?;
            ReportSpan {
                start_unix_nanos: zone.midnight_unix_nanos(first_day),
                end_unix_nanos: zone.midnight_unix_nanos(after_last),
                first_day,
                last_day,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC: ReportZone = ReportZone::new("Etc/UTC");
    const NEW_YORK: ReportZone = ReportZone::new("America/New_York");
    /// Wednesday 2026-11-11 15:30:00 UTC.
    const NOW: i64 = 1_794_411_000 * NANOS_PER_SECOND;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("date")
    }

    fn span(period: ReportPeriod, zone: ReportZone) -> ReportSpan {
        resolve_span(period, &CustomDates { from: "", to: "" }, NOW, zone).expect("span")
    }

    #[test]
    fn preset_periods_start_at_local_calendar_boundaries_and_end_now() {
        assert_eq!(UTC.local(NOW).date(), day(2026, 11, 11));
        let week = span(ReportPeriod::ThisWeek, UTC);
        assert_eq!(week.first_day, day(2026, 11, 9), "weeks start on Monday");
        assert_eq!(
            week.start_unix_nanos,
            UTC.midnight_unix_nanos(day(2026, 11, 9))
        );
        assert_eq!(week.end_unix_nanos, NOW + 1);
        assert_eq!(
            span(ReportPeriod::ThisMonth, UTC).first_day,
            day(2026, 11, 1)
        );
        assert_eq!(span(ReportPeriod::ThisYear, UTC).first_day, day(2026, 1, 1));
        let seven = span(ReportPeriod::LastSevenDays, UTC);
        assert_eq!(
            seven.start_unix_nanos,
            NOW - 7 * SECONDS_PER_DAY * NANOS_PER_SECOND
        );
        assert_eq!(seven.first_day, day(2026, 11, 4));
    }

    #[test]
    fn local_midnight_follows_the_zone_offset_on_each_side_of_a_dst_change() {
        // New York is UTC-4 on 2026-10-31 and UTC-5 from 2026-11-01 (DST ends that morning).
        assert_eq!(
            NEW_YORK.midnight_unix_nanos(day(2026, 10, 31)),
            (UTC.midnight_unix_nanos(day(2026, 10, 31)) / NANOS_PER_SECOND + 4 * 3_600)
                * NANOS_PER_SECOND
        );
        assert_eq!(
            NEW_YORK.midnight_unix_nanos(day(2026, 11, 2)),
            (UTC.midnight_unix_nanos(day(2026, 11, 2)) / NANOS_PER_SECOND + 5 * 3_600)
                * NANOS_PER_SECOND
        );
        assert_eq!(
            NEW_YORK.local(NEW_YORK.midnight_unix_nanos(day(2026, 11, 2))),
            day(2026, 11, 2).and_time(NaiveTime::MIN)
        );
    }

    #[test]
    fn a_period_lists_every_offset_its_zone_uses() {
        let around_change = ReportSpan {
            start_unix_nanos: 0,
            end_unix_nanos: 0,
            first_day: day(2026, 10, 30),
            last_day: day(2026, 11, 3),
        };
        assert_eq!(
            NEW_YORK.offsets_during(around_change),
            [-4 * 3_600, -5 * 3_600]
        );
        // The change day alone still shows both offsets.
        let change_day = ReportSpan {
            first_day: day(2026, 11, 1),
            last_day: day(2026, 11, 1),
            ..around_change
        };
        assert_eq!(
            NEW_YORK.offsets_during(change_day),
            [-4 * 3_600, -5 * 3_600]
        );
        assert_eq!(UTC.offsets_during(around_change), [0]);
        assert_eq!(offset_text(-4 * 3_600), "UTC-04:00");
        assert_eq!(offset_text(5 * 3_600 + 30 * 60), "UTC+05:30");
        assert_eq!(offset_text(0), "UTC+00:00");
    }

    #[test]
    fn a_custom_period_covers_whole_days_and_rejects_bad_dates() {
        let custom =
            |from, to| resolve_span(ReportPeriod::Custom, &CustomDates { from, to }, NOW, UTC);
        let span = custom("2026-11-01", " 2026-11-03 ").expect("custom span");
        assert_eq!(
            span.start_unix_nanos,
            UTC.midnight_unix_nanos(day(2026, 11, 1))
        );
        assert_eq!(
            span.end_unix_nanos,
            UTC.midnight_unix_nanos(day(2026, 11, 4))
        );
        assert_eq!(span.last_day, day(2026, 11, 3));
        assert_eq!(
            custom("11/01/2026", "2026-11-03"),
            Err(PeriodProblem::FromDate)
        );
        assert_eq!(
            custom("2026-11-01", "2026-13-01"),
            Err(PeriodProblem::ToDate)
        );
        assert_eq!(
            custom("2026-11-05", "2026-11-03"),
            Err(PeriodProblem::Reversed)
        );
        assert_eq!(date_text(day(2026, 3, 7)), "2026-03-07");
    }
}
