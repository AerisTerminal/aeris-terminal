//! Pure projection of bounded recurring IANA-zone session segments.
//! Dated provider holidays are not represented by this schedule.

use crate::{MAXIMUM_SESSION_SEGMENTS, SessionHours};
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeDelta, TimeZone};
use chrono_tz::Tz;

const SEARCH_DAYS: i64 = 8;
const SECONDS_PER_DAY: u32 = 86_400;

/// The small shared vocabulary of persisted and provider weekly segments.
pub trait WeeklySegment {
    fn weekday(&self) -> u32;
    fn open_seconds(&self) -> u32;
    fn close_seconds(&self) -> u32;
    fn timezone(&self) -> &str;
}

impl WeeklySegment for SessionHours {
    fn weekday(&self) -> u32 {
        u32::from(self.weekday)
    }
    fn open_seconds(&self) -> u32 {
        self.open_seconds
    }
    fn close_seconds(&self) -> u32 {
        self.close_seconds
    }
    fn timezone(&self) -> &str {
        &self.timezone
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WeeklyPhase {
    Open,
    Closed,
    AlwaysOpen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeeklyStatus {
    pub phase: WeeklyPhase,
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub next_open: Option<i64>,
}

/// Local opening instant for a trading-day candle inside a merged session.
#[must_use]
pub fn daily_open<S: WeeklySegment>(hours: &[S], at_unix_nanos: i64) -> Option<i64> {
    if hours.len() > MAXIMUM_SESSION_SEGMENTS {
        return None;
    }
    let first = hours.first()?;
    let zone: Tz = first.timezone().parse().ok()?;
    let roll = hours
        .iter()
        .filter(|segment| segment.open_seconds() > 0 && segment.open_seconds() < SECONDS_PER_DAY)
        .map(WeeklySegment::open_seconds)
        .min()
        .unwrap_or(0);
    let at = DateTime::from_timestamp_nanos(at_unix_nanos);
    let today = at.with_timezone(&zone).date_naive();
    let candidate = local_instant(zone, today, roll)?;
    if candidate <= at_unix_nanos {
        Some(candidate)
    } else {
        local_instant(zone, today.pred_opt()?, roll)
    }
}

/// Projects the recurring timetable at the supplied instant, including its
/// session end. A closed acceptance instant has no containing session and
/// must not be used to invent an expiry for an old DAY order.
#[must_use]
pub fn weekly_status<S: WeeklySegment>(hours: &[S], at_unix_nanos: i64) -> Option<WeeklyStatus> {
    if hours.is_empty() || hours.len() > MAXIMUM_SESSION_SEGMENTS {
        return None;
    }
    if hours.iter().any(|segment| {
        !(1..=7).contains(&segment.weekday())
            || segment.open_seconds() >= SECONDS_PER_DAY
            || segment.close_seconds() > SECONDS_PER_DAY
            || segment.open_seconds() == segment.close_seconds()
    }) {
        return None;
    }
    if (1..=7).all(|weekday| {
        hours.iter().any(|segment| {
            segment.weekday() == weekday
                && segment.open_seconds() == 0
                && segment.close_seconds() == SECONDS_PER_DAY
        })
    }) {
        return Some(WeeklyStatus {
            phase: WeeklyPhase::AlwaysOpen,
            start: None,
            end: None,
            next_open: None,
        });
    }
    let sessions = dated_sessions(hours, at_unix_nanos)?;
    let next_open = sessions
        .iter()
        .map(|(start, _)| *start)
        .find(|start| *start > at_unix_nanos);
    if let Some((start, end)) = sessions
        .iter()
        .find(|(start, end)| (*start..*end).contains(&at_unix_nanos))
    {
        return Some(WeeklyStatus {
            phase: WeeklyPhase::Open,
            start: Some(*start),
            end: Some(*end),
            next_open,
        });
    }
    let (start, end) = sessions.iter().find(|(start, _)| *start > at_unix_nanos)?;
    Some(WeeklyStatus {
        phase: WeeklyPhase::Closed,
        start: Some(*start),
        end: Some(*end),
        next_open,
    })
}

fn dated_sessions<S: WeeklySegment>(hours: &[S], at_unix_nanos: i64) -> Option<Vec<(i64, i64)>> {
    let at = DateTime::from_timestamp_nanos(at_unix_nanos);
    let mut sessions = Vec::with_capacity(hours.len() * 3);
    for segment in hours {
        let zone: Tz = segment.timezone().parse().ok()?;
        let today = at.with_timezone(&zone).date_naive();
        for offset in -SEARCH_DAYS..=SEARCH_DAYS {
            let date = today.checked_add_signed(TimeDelta::days(offset))?;
            if date.weekday().number_from_monday() != segment.weekday() {
                continue;
            }
            let start = local_instant(zone, date, segment.open_seconds())?;
            let close_date = if segment.close_seconds() < segment.open_seconds() {
                date.succ_opt()?
            } else {
                date
            };
            let end = local_instant(zone, close_date, segment.close_seconds())?;
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
