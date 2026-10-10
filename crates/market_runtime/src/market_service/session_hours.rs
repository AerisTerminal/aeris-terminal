//! Market session status from provider-reported weekly trading hours.
//!
//! Some providers publish only a recurring weekly timetable rather than a dated
//! calendar. The status is a pure function of that timetable and the current
//! instant, so it needs no cache and no refresh; holidays are not represented.

use aeris_contracts::{
    MarketSessionPhase, MarketSessionSource, MarketSessionStatus, ProviderSessionHours,
};
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeDelta, TimeZone};
use chrono_tz::Tz;

/// Days searched either side of now. A merged session may run up to a week, so
/// eight days always reach both the start of the current session and the next open.
const SEARCH_DAYS: i64 = 8;
const SECONDS_PER_DAY: u32 = 86_400;

/// Returns the session status the weekly `hours` imply at `now_unix_nanos`, or
/// `None` when there are no hours or they cannot be placed in time.
pub(super) fn weekly_hours_status(
    instrument_id: &str,
    hours: &[ProviderSessionHours],
    now_unix_nanos: i64,
) -> Option<MarketSessionStatus> {
    if hours.is_empty() {
        return None;
    }
    let status = |phase, start, end, next_open| MarketSessionStatus {
        instrument_id: instrument_id.into(),
        phase,
        source: MarketSessionSource::ProviderHours,
        session_start_unix_nanos: start,
        session_end_unix_nanos: end,
        next_open_unix_nanos: next_open,
    };
    if covers_every_day(hours) {
        return Some(status(MarketSessionPhase::AlwaysOpen, None, None, None));
    }
    let sessions = dated_sessions(hours, now_unix_nanos)?;
    let next_open = sessions
        .iter()
        .map(|(start, _)| *start)
        .find(|start| *start > now_unix_nanos);
    if let Some((start, end)) = sessions
        .iter()
        .find(|(start, end)| (*start..*end).contains(&now_unix_nanos))
    {
        return Some(status(
            MarketSessionPhase::Regular,
            Some(*start),
            Some(*end),
            next_open,
        ));
    }
    let (start, end) = sessions.iter().find(|(start, _)| *start > now_unix_nanos)?;
    Some(status(
        MarketSessionPhase::Closed,
        Some(*start),
        Some(*end),
        next_open,
    ))
}

fn covers_every_day(hours: &[ProviderSessionHours]) -> bool {
    (1..=7).all(|weekday| {
        hours.iter().any(|segment| {
            segment.weekday == weekday
                && segment.open_seconds == 0
                && segment.close_seconds == SECONDS_PER_DAY
        })
    })
}

/// Places every segment on the dates around now and merges sessions that touch,
/// so a week of back-to-back days reads as one session from open to close.
fn dated_sessions(hours: &[ProviderSessionHours], now_unix_nanos: i64) -> Option<Vec<(i64, i64)>> {
    let now = DateTime::from_timestamp_nanos(now_unix_nanos);
    let mut sessions = Vec::new();
    for segment in hours {
        let zone: Tz = segment.timezone.parse().ok()?;
        let today = now.with_timezone(&zone).date_naive();
        for offset in -SEARCH_DAYS..=SEARCH_DAYS {
            let date = today.checked_add_signed(TimeDelta::days(offset))?;
            if date.weekday().number_from_monday() != segment.weekday {
                continue;
            }
            let start = local_instant(zone, date, segment.open_seconds)?;
            let end = local_instant(zone, date, segment.close_seconds)?;
            if start < end {
                sessions.push((start, end));
            }
        }
    }
    sessions.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(sessions.len());
    for (start, end) in sessions {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    Some(merged)
}

/// The instant `seconds` after local midnight on `date`. A wall time skipped by a
/// daylight-saving change resolves to the hour after it, as clocks do.
fn local_instant(zone: Tz, date: NaiveDate, seconds: u32) -> Option<i64> {
    let local = date
        .and_time(NaiveTime::MIN)
        .checked_add_signed(TimeDelta::seconds(i64::from(seconds)))?;
    zone.from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            zone.from_local_datetime(&(local + TimeDelta::hours(1)))
                .earliest()
        })?
        .timestamp_nanos_opt()
}

#[cfg(test)]
mod tests {
    use super::*;

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
