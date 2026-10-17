//! Market session status from provider-reported weekly trading hours.
//!
//! Some providers publish only a recurring weekly timetable rather than a dated
//! calendar. The status is a pure function of that timetable and the current
//! instant, so it needs no cache and no refresh; holidays are not represented.

use aeris_contracts::{
    MarketSessionPhase, MarketSessionSource, MarketSessionStatus, ProviderSessionHours,
};
use aeris_instruments::calendar::weekly_hours::{self as projection, WeeklyPhase, WeeklySegment};

struct ProviderSegment<'a>(&'a ProviderSessionHours);

impl WeeklySegment for ProviderSegment<'_> {
    fn weekday(&self) -> u32 {
        self.0.weekday
    }
    fn open_seconds(&self) -> u32 {
        self.0.open_seconds
    }
    fn close_seconds(&self) -> u32 {
        self.0.close_seconds
    }
    fn timezone(&self) -> &str {
        &self.0.timezone
    }
}

/// Trading-day roll within a continuous weekly session. Adjacent published
/// segments are merged for open/closed status, but a daily candle must still
/// roll at the contract's local opening time on every trading day.
pub(super) fn daily_open(hours: &[ProviderSessionHours], at_unix_nanos: i64) -> Option<i64> {
    let segments = hours.iter().map(ProviderSegment).collect::<Vec<_>>();
    projection::daily_open(&segments, at_unix_nanos)
}

/// Returns the session status the weekly `hours` imply at `now_unix_nanos`, or
/// `None` when there are no hours or they cannot be placed in time.
pub(super) fn weekly_hours_status(
    instrument_id: &str,
    hours: &[ProviderSessionHours],
    now_unix_nanos: i64,
) -> Option<MarketSessionStatus> {
    let segments = hours.iter().map(ProviderSegment).collect::<Vec<_>>();
    let status = projection::weekly_status(&segments, now_unix_nanos)?;
    Some(MarketSessionStatus {
        instrument_id: instrument_id.into(),
        phase: match status.phase {
            WeeklyPhase::Open => MarketSessionPhase::Regular,
            WeeklyPhase::Closed => MarketSessionPhase::Closed,
            WeeklyPhase::AlwaysOpen => MarketSessionPhase::AlwaysOpen,
        },
        source: MarketSessionSource::ProviderHours,
        session_start_unix_nanos: status.start,
        session_end_unix_nanos: status.end,
        next_open_unix_nanos: status.next_open,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    const NANOS: i64 = 1_000_000_000;

    fn utc(text: &str) -> i64 {
        DateTime::parse_from_rfc3339(text)
            .expect("test instant")
            .timestamp_nanos_opt()
            .expect("in range")
    }

    fn day(weekday: u32, open_seconds: u32, close_seconds: u32) -> ProviderSessionHours {
        ProviderSessionHours {
            weekday,
            open_seconds,
            close_seconds,
            timezone: "America/New_York".into(),
        }
    }

    /// Forex hours: Sunday 17:00 to Friday 17:00 New York time, one segment per day.
    fn forex_week() -> Vec<ProviderSessionHours> {
        vec![
            day(7, 17 * 3_600, 86_400),
            day(1, 0, 86_400),
            day(2, 0, 86_400),
            day(3, 0, 86_400),
            day(4, 0, 86_400),
            day(5, 0, 17 * 3_600),
        ]
    }

    #[test]
    fn the_weekend_is_closed_until_sunday_evening() {
        // Saturday 10 October 2026, noon in New York (EDT, UTC-4).
        let saturday = utc("2026-10-10T16:00:00Z");
        let status = weekly_hours_status("fx", &forex_week(), saturday).expect("hours");
        assert_eq!(status.phase, MarketSessionPhase::Closed);
        assert_eq!(status.source, MarketSessionSource::ProviderHours);
        let sunday_open = utc("2026-10-11T21:00:00Z");
        assert_eq!(status.next_open_unix_nanos, Some(sunday_open));
        assert_eq!(status.session_start_unix_nanos, Some(sunday_open));
        assert_eq!(
            status.session_end_unix_nanos,
            Some(utc("2026-10-16T21:00:00Z")),
            "back-to-back days read as one session until Friday's close"
        );
    }

    #[test]
    fn midweek_is_one_session_from_sunday_open_to_friday_close() {
        let wednesday = utc("2026-10-07T15:30:00Z");
        let status = weekly_hours_status("fx", &forex_week(), wednesday).expect("hours");
        assert_eq!(status.phase, MarketSessionPhase::Regular);
        assert_eq!(
            status.session_start_unix_nanos,
            Some(utc("2026-10-04T21:00:00Z"))
        );
        assert_eq!(
            status.session_end_unix_nanos,
            Some(utc("2026-10-09T21:00:00Z"))
        );
        assert_eq!(
            status.next_open_unix_nanos,
            Some(utc("2026-10-11T21:00:00Z"))
        );
    }

    #[test]
    fn the_close_follows_daylight_saving_time() {
        // Friday 6 November 2026 is after US clocks fall back, so 17:00 is 22:00 UTC.
        let friday = utc("2026-11-06T21:30:00Z");
        let status = weekly_hours_status("fx", &forex_week(), friday).expect("hours");
        assert_eq!(status.phase, MarketSessionPhase::Regular);
        assert_eq!(
            status.session_end_unix_nanos,
            Some(utc("2026-11-06T22:00:00Z"))
        );
        let after_close = friday + NANOS * 3_600;
        assert_eq!(
            weekly_hours_status("fx", &forex_week(), after_close)
                .expect("hours")
                .phase,
            MarketSessionPhase::Closed
        );
    }

    #[test]
    fn a_daily_break_closes_the_market_until_it_reopens() {
        // The shape a cTrader demo broker reports for EURUSD: 17:00:05 to 16:29:55
        // the next day, Sunday through Friday, split at midnight.
        let mut hours = Vec::new();
        for weekday in [7, 1, 2, 3, 4] {
            hours.push(day(weekday, 61_205, 86_400));
            hours.push(day(weekday % 7 + 1, 0, 59_395));
        }
        let monday_break = utc("2026-10-05T20:45:00Z");
        let status = weekly_hours_status("fx", &hours, monday_break).expect("hours");
        assert_eq!(status.phase, MarketSessionPhase::Closed);
        assert_eq!(
            status.next_open_unix_nanos,
            Some(utc("2026-10-05T21:00:05Z"))
        );
        let tuesday = utc("2026-10-06T15:00:00Z");
        let status = weekly_hours_status("fx", &hours, tuesday).expect("hours");
        assert_eq!(status.phase, MarketSessionPhase::Regular);
        assert_eq!(
            status.session_end_unix_nanos,
            Some(utc("2026-10-06T20:29:55Z"))
        );
        let saturday = utc("2026-10-10T16:00:00Z");
        assert_eq!(
            weekly_hours_status("fx", &hours, saturday)
                .expect("hours")
                .next_open_unix_nanos,
            Some(utc("2026-10-11T21:00:05Z"))
        );
    }

    #[test]
    fn hours_on_every_day_are_always_open() {
        let every_day = (1..=7)
            .map(|weekday| day(weekday, 0, 86_400))
            .collect::<Vec<_>>();
        let status =
            weekly_hours_status("btc", &every_day, utc("2026-10-10T16:00:00Z")).expect("hours");
        assert_eq!(status.phase, MarketSessionPhase::AlwaysOpen);
    }

    #[test]
    fn missing_or_unplaceable_hours_give_no_status() {
        let now = utc("2026-10-10T16:00:00Z");
        assert_eq!(weekly_hours_status("fx", &[], now), None);
        let mut unknown_zone = forex_week();
        unknown_zone[0].timezone = "Not/AZone".into();
        assert_eq!(weekly_hours_status("fx", &unknown_zone, now), None);
    }
}
