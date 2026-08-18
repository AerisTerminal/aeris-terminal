const SECONDS_PER_DAY: i64 = 86_400;
const CME_SESSION_ROLL_SECONDS: i64 = 17 * 60 * 60;

/// Calendar aggregation requested for one Rithmic session series.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicCalendarPeriod {
    Week,
    Month,
}

/// Opaque exchange-calendar bucket used to compare history and live events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicCalendarBucket(i64);

/// Provider-owned exchange calendar for the currently supported Rithmic venues.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicExchangeCalendar {
    kind: CalendarKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CalendarKind {
    CmeGlobex,
}

impl RithmicExchangeCalendar {
    /// Resolves the exchange calendar without leaking provider metadata above the adapter.
    #[must_use]
    pub fn for_venue(venue_id: &str) -> Option<Self> {
        matches!(venue_id, "CME" | "CBOT" | "COMEX" | "NYMEX").then_some(Self {
            kind: CalendarKind::CmeGlobex,
        })
    }

    /// Returns the calendar week or month containing one exchange timestamp.
    #[must_use]
    pub fn bucket(
        self,
        exchange_timestamp_seconds: i64,
        period: RithmicCalendarPeriod,
    ) -> RithmicCalendarBucket {
        match self.kind {
            CalendarKind::CmeGlobex => {
                let trading_day = cme_trading_day(exchange_timestamp_seconds);
                match period {
                    RithmicCalendarPeriod::Week => {
                        let monday = trading_day - (trading_day + 3).rem_euclid(7);
                        RithmicCalendarBucket(monday)
                    }
                    RithmicCalendarPeriod::Month => {
                        let (year, month, _) = civil_from_days(trading_day);
                        RithmicCalendarBucket(year * 12 + i64::from(month))
                    }
                }
            }
        }
    }
}

fn cme_trading_day(timestamp_seconds: i64) -> i64 {
    let local_seconds = timestamp_seconds + chicago_utc_offset_seconds(timestamp_seconds);
    let local_day = local_seconds.div_euclid(SECONDS_PER_DAY);
    let seconds_of_day = local_seconds.rem_euclid(SECONDS_PER_DAY);
    local_day + i64::from(seconds_of_day >= CME_SESSION_ROLL_SECONDS)
}

fn chicago_utc_offset_seconds(timestamp_seconds: i64) -> i64 {
    let utc_day = timestamp_seconds.div_euclid(SECONDS_PER_DAY);
    let (year, _, _) = civil_from_days(utc_day);
    let (daylight_start_day, daylight_end_day) = if year >= 2007 {
        (
            nth_weekday_of_month(year, 3, 0, 2),
            nth_weekday_of_month(year, 11, 0, 1),
        )
    } else {
        (
            nth_weekday_of_month(year, 4, 0, 1),
            last_weekday_of_month(year, 10, 0),
        )
    };
    let daylight_start = daylight_start_day * SECONDS_PER_DAY + 8 * 60 * 60;
    let daylight_end = daylight_end_day * SECONDS_PER_DAY + 7 * 60 * 60;
    if timestamp_seconds >= daylight_start && timestamp_seconds < daylight_end {
        -5 * 60 * 60
    } else {
        -6 * 60 * 60
    }
}

fn nth_weekday_of_month(year: i64, month: u32, weekday_sunday_zero: i64, nth: i64) -> i64 {
    let first = days_from_civil(year, month, 1);
    let first_weekday = (first + 4).rem_euclid(7);
    let day = 1 + (weekday_sunday_zero - first_weekday).rem_euclid(7) + 7 * (nth - 1);
    days_from_civil(year, month, u32::try_from(day).unwrap_or(1))
}

fn last_weekday_of_month(year: i64, month: u32, weekday_sunday_zero: i64) -> i64 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let last = days_from_civil(next_year, next_month, 1) - 1;
    let last_weekday = (last + 4).rem_euclid(7);
    last - (last_weekday - weekday_sunday_zero).rem_euclid(7)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_prime = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(unix_day: i64) -> (i64, u32, u32) {
    let day = unix_day + 719_468;
    let era = day.div_euclid(146_097);
    let day_of_era = day - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_seconds(year: i64, month: u32, day: u32, hour: i64) -> i64 {
        days_from_civil(year, month, day) * SECONDS_PER_DAY + hour * 60 * 60
    }

    #[test]
    fn cme_week_rolls_at_the_sunday_evening_session_boundary() {
        let calendar = RithmicExchangeCalendar::for_venue("CME").expect("CME calendar");
        let before = utc_seconds(2026, 8, 16, 21);
        let after = utc_seconds(2026, 8, 16, 22);

        assert_ne!(
            calendar.bucket(before, RithmicCalendarPeriod::Week),
            calendar.bucket(after, RithmicCalendarPeriod::Week)
        );
    }

    #[test]
    fn cme_month_uses_the_trading_date_across_dst() {
        let calendar = RithmicExchangeCalendar::for_venue("CME").expect("CME calendar");
        let august_session = utc_seconds(2026, 8, 31, 21);
        let september_session = utc_seconds(2026, 8, 31, 22);

        assert_ne!(
            calendar.bucket(august_session, RithmicCalendarPeriod::Month),
            calendar.bucket(september_session, RithmicCalendarPeriod::Month)
        );
        assert_eq!(
            chicago_utc_offset_seconds(utc_seconds(2026, 1, 15, 12)),
            -21_600
        );
        assert_eq!(
            chicago_utc_offset_seconds(utc_seconds(2026, 7, 15, 12)),
            -18_000
        );
    }

    #[test]
    fn chicago_dst_transitions_follow_the_rules_active_for_the_history_year() {
        assert_eq!(
            chicago_utc_offset_seconds(utc_seconds(2006, 3, 20, 12)),
            -21_600
        );
        assert_eq!(
            chicago_utc_offset_seconds(utc_seconds(2006, 4, 2, 8)),
            -18_000
        );
        assert_eq!(
            chicago_utc_offset_seconds(utc_seconds(2006, 10, 29, 7)),
            -21_600
        );
        assert_eq!(
            chicago_utc_offset_seconds(utc_seconds(2007, 3, 11, 8)),
            -18_000
        );
        assert_eq!(
            chicago_utc_offset_seconds(utc_seconds(2007, 11, 4, 7)),
            -21_600
        );
    }

    #[test]
    fn unsupported_venues_do_not_receive_a_guessed_calendar() {
        assert_eq!(RithmicExchangeCalendar::for_venue("UNKNOWN"), None);
    }
}
