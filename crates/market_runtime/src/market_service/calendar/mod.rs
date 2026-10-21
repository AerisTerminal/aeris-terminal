//! Authoritative, provider-neutral session projections.
mod venue;
pub(crate) mod weekly_hours;

use std::collections::BTreeMap;
use std::fmt;

use aeris_contracts::{
    InstallProviderInstrument, MarketSessionPhase, MarketSessionSource, MarketSessionStatus,
    ProviderSessionHours,
};
use aeris_instruments::{CalendarSourceKind, SessionPhase, SessionWindow};
use chrono::{DateTime, Datelike, Utc};

pub(crate) use venue::{ExchangeRule, Venue, instrument_venue};

/// Calendar capability advertised by catalog metadata. This does not assume a
/// calendar for unknown venues.
#[must_use]
pub fn source_kind_for(instrument: &InstallProviderInstrument) -> CalendarSourceKind {
    if instrument
        .contract_metadata
        .as_ref()
        .is_some_and(|metadata| !metadata.session_hours.is_empty())
    {
        CalendarSourceKind::ContractHours
    } else {
        match instrument_venue(instrument) {
            Some(Venue::Continuous) => CalendarSourceKind::AlwaysOpen,
            Some(_) => CalendarSourceKind::ExchangeRule,
            None => CalendarSourceKind::Unknown,
        }
    }
}

/// One instrument's selected calendar. Contract hours take precedence over
/// exchange and continuous rules.
#[derive(Clone, Debug)]
pub enum CalendarSource {
    ContractHours(Vec<ProviderSessionHours>),
    ExchangeRule(ExchangeRule),
    AlwaysOpen,
    Unknown,
}

/// Period identity for live trade aggregation; unlike elapsed seconds these
/// identities change at exchange session boundaries, including DST changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalendarBarPeriod {
    Session(u32),
    Week,
    Month,
}

/// Instant a provider uses as the timestamp of a session, week or month bar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalendarBarStamp {
    /// The opening instant of the bar's first session.
    SessionOpen,
    /// 00:00 UTC of the bar's first trading date. For an equity this instant
    /// lies outside every session, so it names the bar only through its date.
    TradingDateMidnightUtc,
}

const DAY_MS: i64 = 86_400_000;

impl CalendarSource {
    #[must_use]
    pub fn kind(&self) -> CalendarSourceKind {
        match self {
            Self::ContractHours(_) => CalendarSourceKind::ContractHours,
            Self::ExchangeRule(_) => CalendarSourceKind::ExchangeRule,
            Self::AlwaysOpen => CalendarSourceKind::AlwaysOpen,
            Self::Unknown => CalendarSourceKind::Unknown,
        }
    }
}

/// Session lookup by installed instrument identity.
#[derive(Clone, Default)]
pub struct MarketCalendarService {
    sources: BTreeMap<String, CalendarSource>,
}

/// An installed instrument cannot admit a simulated order at this phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalendarSessionError {
    Closed,
    Unknown,
}

impl fmt::Display for CalendarSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("Practice market is closed"),
            Self::Unknown => f.write_str("Practice market session is unknown"),
        }
    }
}

impl MarketCalendarService {
    /// Copy only this instrument's calendar into a bounded live handoff.
    #[must_use]
    pub fn for_instrument(&self, instrument_id: &str) -> Self {
        let mut calendar = Self::default();
        if let Some(source) = self.sources.get(instrument_id) {
            calendar.insert(instrument_id.into(), source.clone());
        }
        calendar
    }
    /// Select a source in descending order of specificity.
    pub fn resolve(&mut self, instrument: &InstallProviderInstrument) {
        let source = if let Some(hours) = instrument
            .contract_metadata
            .as_ref()
            .map(|metadata| &metadata.session_hours)
            .filter(|hours| !hours.is_empty())
        {
            CalendarSource::ContractHours(hours.clone())
        } else if let Some(venue) = instrument_venue(instrument) {
            match venue {
                Venue::Continuous => CalendarSource::AlwaysOpen,
                _ => ExchangeRule::from_venue(venue)
                    .map_or(CalendarSource::Unknown, CalendarSource::ExchangeRule),
            }
        } else {
            CalendarSource::Unknown
        };
        self.insert(instrument.instrument_id.clone(), source);
    }

    pub fn insert(&mut self, instrument_id: String, source: CalendarSource) {
        self.sources.insert(instrument_id, source);
    }

    #[must_use]
    pub fn source_kind(&self, instrument_id: &str) -> CalendarSourceKind {
        self.sources
            .get(instrument_id)
            .map_or(CalendarSourceKind::Unknown, CalendarSource::kind)
    }

    #[must_use]
    pub fn phase(&self, instrument_id: &str, at_utc_ms: i64) -> SessionPhase {
        phase_from_status(self.status(instrument_id, at_utc_ms).phase)
    }

    /// Apply one session policy for every provider without guessing on unknown sources.
    ///
    /// # Errors
    /// Returns the precise closed or unknown phase instead of assuming tradability.
    pub fn require_practice_session(
        &self,
        instrument_id: &str,
        at_utc_ms: i64,
    ) -> Result<(), CalendarSessionError> {
        match self.phase(instrument_id, at_utc_ms) {
            SessionPhase::Open | SessionPhase::Extended | SessionPhase::AlwaysOpen => Ok(()),
            SessionPhase::Unknown => Err(CalendarSessionError::Unknown),
            SessionPhase::PreOpen | SessionPhase::Break | SessionPhase::Closed => {
                Err(CalendarSessionError::Closed)
            }
        }
    }

    #[must_use]
    pub fn session_window(&self, instrument_id: &str, at_utc_ms: i64) -> Option<SessionWindow> {
        let status = self.status(instrument_id, at_utc_ms);
        Some(SessionWindow {
            open_utc_ms: status.session_start_unix_nanos?.div_euclid(1_000_000),
            close_utc_ms: status.session_end_unix_nanos?.div_euclid(1_000_000),
        })
    }

    /// Start of the session containing a trade. Provider history uses the same
    /// opening instant for daily bars rather than the trade's arrival instant.
    #[must_use]
    pub fn session_open(&self, instrument_id: &str, at_utc_ms: i64) -> Option<i64> {
        if let Some(CalendarSource::ContractHours(hours)) = self.sources.get(instrument_id) {
            return matches!(
                self.phase(instrument_id, at_utc_ms),
                SessionPhase::Open | SessionPhase::Extended
            )
            .then(|| {
                weekly_hours::daily_open(hours, at_utc_ms.saturating_mul(1_000_000))
                    .map(|open| open.div_euclid(1_000_000))
            })
            .flatten();
        }
        let window = self.session_window(instrument_id, at_utc_ms)?;
        if window.open_utc_ms <= at_utc_ms && at_utc_ms < window.close_utc_ms {
            return Some(window.open_utc_ms);
        }
        // Prints just before an evening reopen belong to the preceding daily
        // candle, not tomorrow's session.
        if window.open_utc_ms - at_utc_ms > 2 * 3_600_000 {
            return None;
        }
        let prior = at_utc_ms.checked_sub(2 * 3_600_000)?;
        let window = self.session_window(instrument_id, prior)?;
        (window.open_utc_ms <= prior && prior < window.close_utc_ms).then_some(window.open_utc_ms)
    }

    /// Date (Unix days) attributed to a print by the instrument's calendar.
    /// CME's trading date starts on the previous evening.
    #[must_use]
    pub fn trading_day(&self, instrument_id: &str, at_utc_ms: i64) -> Option<i64> {
        let source = self.sources.get(instrument_id)?;
        if let CalendarSource::ExchangeRule(ExchangeRule::CmeGlobex) = source {
            return aeris_instruments::calendar::ExchangeCalendar::for_code("CME")
                .map(|rule| rule.trading_day(at_utc_ms.div_euclid(1_000)));
        }
        if matches!(source, CalendarSource::Unknown) {
            return None;
        }
        let open = self
            .session_open(instrument_id, at_utc_ms)
            .unwrap_or(at_utc_ms);
        if let CalendarSource::ExchangeRule(ExchangeRule::Cfe) = source {
            // Evening sessions are assigned to the following local trading
            // date, including the Sunday reopen after the weekend.
            let at = DateTime::<Utc>::from_timestamp_millis(open)?;
            let local = at.with_timezone(&chrono_tz::America::Chicago);
            return Some(
                local
                    .date_naive()
                    .succ_opt()?
                    .and_hms_opt(0, 0, 0)?
                    .and_utc()
                    .timestamp()
                    .div_euclid(86_400),
            );
        }
        if let CalendarSource::ContractHours(hours) = source {
            let timezone: chrono_tz::Tz = hours.first()?.timezone.parse().ok()?;
            let local = DateTime::<Utc>::from_timestamp_millis(open)?.with_timezone(&timezone);
            let roll = hours
                .iter()
                .any(|segment| segment.open_seconds > 0 && segment.open_seconds < 86_400);
            let day = if roll {
                local.date_naive().succ_opt()?
            } else {
                local.date_naive()
            };
            return Some(
                day.and_hms_opt(0, 0, 0)?
                    .and_utc()
                    .timestamp()
                    .div_euclid(86_400),
            );
        }
        Some(open.div_euclid(86_400_000))
    }

    /// Identity of a live bar. A closed session has no live bucket.
    #[must_use]
    pub fn bar_bucket(
        &self,
        instrument_id: &str,
        at_utc_ms: i64,
        period: CalendarBarPeriod,
    ) -> Option<i64> {
        if self.session_open(instrument_id, at_utc_ms).is_none()
            && self.phase(instrument_id, at_utc_ms) != SessionPhase::AlwaysOpen
        {
            return None;
        }
        day_bucket(self.trading_day(instrument_id, at_utc_ms)?, period)
    }

    /// Identity of a bar from the timestamp its provider stamped on it.
    #[must_use]
    pub fn stamped_bar_bucket(
        &self,
        instrument_id: &str,
        stamp_utc_ms: i64,
        period: CalendarBarPeriod,
        stamp: CalendarBarStamp,
    ) -> Option<i64> {
        match stamp {
            CalendarBarStamp::SessionOpen => self.bar_bucket(instrument_id, stamp_utc_ms, period),
            CalendarBarStamp::TradingDateMidnightUtc => {
                day_bucket(stamp_utc_ms.div_euclid(DAY_MS), period)
            }
        }
    }

    /// Timestamp the provider gives the bar that a trade at `at_utc_ms` opens.
    #[must_use]
    pub fn bar_open_stamp(
        &self,
        instrument_id: &str,
        at_utc_ms: i64,
        period: CalendarBarPeriod,
        stamp: CalendarBarStamp,
    ) -> i64 {
        match stamp {
            CalendarBarStamp::SessionOpen => self
                .session_open(instrument_id, at_utc_ms)
                .unwrap_or(at_utc_ms),
            CalendarBarStamp::TradingDateMidnightUtc => self
                .trading_day(instrument_id, at_utc_ms)
                .and_then(|day| day_bucket(day, period))
                .and_then(|bucket| bucket_first_day(bucket, period))
                .and_then(|day| day.checked_mul(DAY_MS))
                .unwrap_or(at_utc_ms),
        }
    }

    #[must_use]
    pub fn next_transition(
        &self,
        instrument_id: &str,
        at_utc_ms: i64,
    ) -> Option<(i64, SessionPhase)> {
        let status = self.status(instrument_id, at_utc_ms);
        let at = at_utc_ms.checked_mul(1_000_000)?;
        let end = status.session_end_unix_nanos.filter(|end| *end > at);
        let open = status.next_open_unix_nanos.filter(|open| *open > at);
        match (end, open) {
            (Some(end), Some(open)) if open < end => {
                Some((open.div_euclid(1_000_000), SessionPhase::Open))
            }
            (Some(end), _) => Some((
                end.div_euclid(1_000_000),
                match status.phase {
                    MarketSessionPhase::Closed | MarketSessionPhase::PreMarket => {
                        SessionPhase::Open
                    }
                    MarketSessionPhase::Regular
                        if matches!(
                            self.sources.get(instrument_id),
                            Some(CalendarSource::ExchangeRule(ExchangeRule::UsEquity))
                        ) =>
                    {
                        SessionPhase::Extended
                    }
                    _ => SessionPhase::Closed,
                },
            )),
            (None, Some(open)) => Some((open.div_euclid(1_000_000), SessionPhase::Open)),
            _ => None,
        }
    }

    #[must_use]
    pub fn status(&self, instrument_id: &str, at_utc_ms: i64) -> MarketSessionStatus {
        let unknown = || MarketSessionStatus {
            instrument_id: instrument_id.into(),
            phase: MarketSessionPhase::Unknown,
            source: MarketSessionSource::Unavailable,
            session_start_unix_nanos: None,
            session_end_unix_nanos: None,
            next_open_unix_nanos: None,
        };
        let Some(at) = at_utc_ms.checked_mul(1_000_000) else {
            return unknown();
        };
        let Some(source) = self.sources.get(instrument_id) else {
            return unknown();
        };
        match source {
            CalendarSource::ContractHours(hours) => {
                weekly_hours::weekly_hours_status(instrument_id, hours, at).unwrap_or_else(unknown)
            }
            CalendarSource::ExchangeRule(rule) => rule.status(instrument_id, at),
            CalendarSource::AlwaysOpen => MarketSessionStatus {
                phase: MarketSessionPhase::AlwaysOpen,
                source: MarketSessionSource::Continuous,
                ..unknown()
            },
            CalendarSource::Unknown => unknown(),
        }
    }
}

/// Bar identity of a trading date (Unix days). Weeks start on Monday.
fn day_bucket(day: i64, period: CalendarBarPeriod) -> Option<i64> {
    match period {
        CalendarBarPeriod::Session(days) if days > 0 => Some(day.div_euclid(i64::from(days))),
        CalendarBarPeriod::Session(_) => None,
        CalendarBarPeriod::Week => Some((day + 3).div_euclid(7)),
        CalendarBarPeriod::Month => {
            let date = DateTime::<Utc>::from_timestamp(day.checked_mul(86_400)?, 0)?;
            Some(i64::from(date.year()) * 12 + i64::from(date.month()))
        }
    }
}

/// First trading date (Unix days) of a bar identity from `day_bucket`.
fn bucket_first_day(bucket: i64, period: CalendarBarPeriod) -> Option<i64> {
    match period {
        CalendarBarPeriod::Session(days) if days > 0 => bucket.checked_mul(i64::from(days)),
        CalendarBarPeriod::Session(_) => None,
        CalendarBarPeriod::Week => bucket.checked_mul(7)?.checked_sub(3),
        CalendarBarPeriod::Month => {
            let months = bucket.checked_sub(1)?;
            let year = i32::try_from(months.div_euclid(12)).ok()?;
            let month = u32::try_from(months.rem_euclid(12) + 1).ok()?;
            let first = chrono::NaiveDate::from_ymd_opt(year, month, 1)?;
            Some(
                first
                    .and_hms_opt(0, 0, 0)?
                    .and_utc()
                    .timestamp()
                    .div_euclid(86_400),
            )
        }
    }
}

fn phase_from_status(value: MarketSessionPhase) -> SessionPhase {
    match value {
        MarketSessionPhase::Regular => SessionPhase::Open,
        MarketSessionPhase::PreMarket
        | MarketSessionPhase::PostMarket
        | MarketSessionPhase::Overnight => SessionPhase::Extended,
        MarketSessionPhase::Closed => SessionPhase::Closed,
        MarketSessionPhase::AlwaysOpen => SessionPhase::AlwaysOpen,
        MarketSessionPhase::Unknown => SessionPhase::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    #[test]
    fn calendar_source_priority_uses_hours_then_venue() {
        let mut instrument = InstallProviderInstrument {
            instrument_id: "calendar-fixture".into(),
            venue_id: "CME".into(),
            ..Default::default()
        };
        let mut calendar = MarketCalendarService::default();
        let id = instrument.instrument_id.clone();
        instrument.venue_id = "CME".into();
        calendar.resolve(&instrument);
        assert_eq!(calendar.source_kind(&id), CalendarSourceKind::ExchangeRule);
        instrument.contract_metadata = Some(Box::new(aeris_contracts::ProviderContractMetadata {
            session_hours: vec![ProviderSessionHours {
                weekday: 1,
                open_seconds: 0,
                close_seconds: 3_600,
                timezone: "UTC".into(),
            }],
            ..Default::default()
        }));
        calendar.resolve(&instrument);
        assert_eq!(calendar.source_kind(&id), CalendarSourceKind::ContractHours);
    }

    #[test]
    fn calendar_provider_resolution_matrix_and_status_delegation() {
        let at = ms("2026-08-17T14:00:00Z");
        let mut service = MarketCalendarService::default();
        for (provider, venue, expected) in [
            ("rithmic", "CME", CalendarSourceKind::ExchangeRule),
            ("hyperliquid", "HYPERLIQUID", CalendarSourceKind::AlwaysOpen),
            ("contract-hours", "", CalendarSourceKind::ContractHours),
        ] {
            let instrument = InstallProviderInstrument {
                provider: provider.into(),
                instrument_id: provider.into(),
                venue_id: venue.into(),
                contract_metadata: (provider == "contract-hours").then(|| {
                    Box::new(aeris_contracts::ProviderContractMetadata {
                        session_hours: vec![ProviderSessionHours {
                            weekday: 1,
                            open_seconds: 0,
                            close_seconds: 86_400,
                            timezone: "UTC".into(),
                        }],
                        ..Default::default()
                    })
                }),
                ..Default::default()
            };
            service.resolve(&instrument);
            assert_eq!(service.source_kind(provider), expected, "{provider}");
            assert_eq!(
                service.phase(provider, at),
                phase_from_status(service.status(provider, at).phase),
                "{provider}: calendar status and phase disagree"
            );
        }
    }

    #[test]
    fn calendar_practice_sessions_are_uniform_across_providers() {
        let mut calendar = MarketCalendarService::default();
        calendar.insert("crypto".into(), CalendarSource::AlwaysOpen);
        calendar.insert(
            "cme".into(),
            CalendarSource::ExchangeRule(ExchangeRule::CmeGlobex),
        );
        let saturday = ms("2026-10-10T18:00:00Z");
        assert_eq!(
            calendar.require_practice_session("crypto", saturday),
            Ok(())
        );
        assert_eq!(
            calendar.require_practice_session("cme", saturday),
            Err(CalendarSessionError::Closed)
        );
        assert_eq!(
            calendar.require_practice_session("unknown", saturday),
            Err(CalendarSessionError::Unknown)
        );
    }

    fn ms(text: &str) -> i64 {
        text.parse::<DateTime<Utc>>()
            .expect("UTC instant")
            .timestamp_millis()
    }

    #[test]
    fn calendar_cme_roll_and_dst_weeks() {
        let mut calendar = MarketCalendarService::default();
        calendar.insert(
            "future".into(),
            CalendarSource::ExchangeRule(ExchangeRule::CmeGlobex),
        );
        for (before, open, close) in [
            (
                "2026-02-08T22:59:00Z",
                "2026-02-08T23:00:00Z",
                "2026-02-09T22:00:00Z",
            ),
            (
                "2026-03-08T21:59:00Z",
                "2026-03-08T22:00:00Z",
                "2026-03-09T21:00:00Z",
            ),
            (
                "2026-11-01T22:59:00Z",
                "2026-11-01T23:00:00Z",
                "2026-11-02T22:00:00Z",
            ),
        ] {
            assert_eq!(calendar.phase("future", ms(before)), SessionPhase::Closed);
            assert_eq!(
                calendar.session_window("future", ms(before)),
                Some(SessionWindow {
                    open_utc_ms: ms(open),
                    close_utc_ms: ms(close)
                })
            );
            assert_eq!(
                calendar.next_transition("future", ms(before)),
                Some((ms(open), SessionPhase::Open))
            );
            assert_eq!(calendar.phase("future", ms(open)), SessionPhase::Open);
            assert_eq!(
                calendar.session_window("future", ms(open)),
                Some(SessionWindow {
                    open_utc_ms: ms(open),
                    close_utc_ms: ms(close)
                })
            );
            assert_eq!(
                calendar.next_transition("future", ms(open)),
                Some((ms(close), SessionPhase::Closed))
            );
        }
    }

    #[test]
    fn calendar_cme_week_month_and_trading_day_match_exchange_buckets() {
        use aeris_instruments::calendar::{ExchangeCalendar, ExchangeCalendarPeriod};
        let exchange = ExchangeCalendar::for_code("XCME").expect("CME venue");
        for (before, after) in [
            ("2026-08-16T21:59:00Z", "2026-08-16T22:01:00Z"),
            ("2026-03-08T21:59:00Z", "2026-03-08T22:01:00Z"),
            ("2026-11-01T22:59:00Z", "2026-11-01T23:01:00Z"),
        ] {
            let before = ms(before).div_euclid(1_000);
            let after = ms(after).div_euclid(1_000);
            assert_ne!(exchange.trading_day(before), exchange.trading_day(after));
            assert_ne!(
                exchange.bucket(before, ExchangeCalendarPeriod::Week),
                exchange.bucket(after, ExchangeCalendarPeriod::Week)
            );
        }
        let before = ms("2026-08-31T21:59:00Z").div_euclid(1_000);
        let after = ms("2026-08-31T22:01:00Z").div_euclid(1_000);
        assert_ne!(
            exchange.bucket(before, ExchangeCalendarPeriod::Month),
            exchange.bucket(after, ExchangeCalendarPeriod::Month)
        );
    }

    #[test]
    fn calendar_matrix_cfe_equities_continuous_and_unknown() {
        let mut calendar = MarketCalendarService::default();
        calendar.insert(
            "cfe".into(),
            CalendarSource::ExchangeRule(ExchangeRule::Cfe),
        );
        calendar.insert(
            "equity".into(),
            CalendarSource::ExchangeRule(ExchangeRule::UsEquity),
        );
        calendar.insert("crypto".into(), CalendarSource::AlwaysOpen);
        for (instrument, at, phase, window, transition) in [
            (
                "cfe",
                "2026-08-17T18:00:00Z",
                SessionPhase::Open,
                Some(("2026-08-16T22:00:00Z", "2026-08-17T21:00:00Z")),
                Some(("2026-08-17T21:00:00Z", SessionPhase::Closed)),
            ),
            (
                "equity",
                "2026-08-17T13:00:00Z",
                SessionPhase::Extended,
                Some(("2026-08-17T08:00:00Z", "2026-08-17T13:30:00Z")),
                Some(("2026-08-17T13:30:00Z", SessionPhase::Open)),
            ),
            (
                "equity",
                "2026-08-17T14:00:00Z",
                SessionPhase::Open,
                Some(("2026-08-17T13:30:00Z", "2026-08-17T20:00:00Z")),
                Some(("2026-08-17T20:00:00Z", SessionPhase::Extended)),
            ),
            (
                "equity",
                "2026-08-17T22:00:00Z",
                SessionPhase::Extended,
                Some(("2026-08-17T20:00:00Z", "2026-08-18T00:00:00Z")),
                Some(("2026-08-18T00:00:00Z", SessionPhase::Closed)),
            ),
            (
                "crypto",
                "2026-08-17T22:00:00Z",
                SessionPhase::AlwaysOpen,
                None,
                None,
            ),
            (
                "unlisted",
                "2026-08-17T22:00:00Z",
                SessionPhase::Unknown,
                None,
                None,
            ),
        ] {
            let at = ms(at);
            assert_eq!(
                calendar.phase(instrument, at),
                phase,
                "{instrument} at {at}"
            );
            assert_eq!(
                calendar.session_window(instrument, at),
                window.map(|(open, close)| SessionWindow {
                    open_utc_ms: ms(open),
                    close_utc_ms: ms(close)
                }),
                "{instrument} at {at}"
            );
            assert_eq!(
                calendar.next_transition(instrument, at),
                transition.map(|(time, phase)| (ms(time), phase)),
                "{instrument} at {at}"
            );
        }
    }

    #[test]
    fn calendar_venue_codes_are_shared() {
        for code in [
            "CME", "XCME", "CBOT", "XCBT", "COMEX", "XCEC", "NYMEX", "XNYM",
        ] {
            assert_eq!(venue::venue(code), Some(Venue::Cme));
        }
        assert_eq!(venue::venue("XCFE"), Some(Venue::Cfe));
        assert_eq!(venue::venue("XNYS"), Some(Venue::UsEquity));
        assert_eq!(venue::venue("UNKNOWN"), None);
    }

    #[test]
    fn calendar_fx_contract_hours_close_over_weekend() {
        let mut calendar = MarketCalendarService::default();
        let hours = [
            (7, 17 * 3_600, 86_400),
            (1, 0, 86_400),
            (2, 0, 86_400),
            (3, 0, 86_400),
            (4, 0, 86_400),
            (5, 0, 17 * 3_600),
        ]
        .into_iter()
        .map(
            |(weekday, open_seconds, close_seconds)| ProviderSessionHours {
                weekday,
                open_seconds,
                close_seconds,
                timezone: "America/New_York".into(),
            },
        )
        .collect();
        calendar.insert("fx".into(), CalendarSource::ContractHours(hours));
        assert_eq!(
            calendar.source_kind("fx"),
            CalendarSourceKind::ContractHours
        );
        assert_eq!(
            calendar.phase("fx", ms("2026-10-10T16:00:00Z")),
            SessionPhase::Closed
        );
        assert_eq!(
            calendar.session_window("fx", ms("2026-10-10T16:00:00Z")),
            Some(SessionWindow {
                open_utc_ms: ms("2026-10-11T21:00:00Z"),
                close_utc_ms: ms("2026-10-16T21:00:00Z")
            })
        );
        assert_eq!(
            calendar.next_transition("fx", ms("2026-10-10T16:00:00Z")),
            Some((ms("2026-10-11T21:00:00Z"), SessionPhase::Open))
        );
        assert_eq!(
            calendar.phase("fx", ms("2026-10-12T16:00:00Z")),
            SessionPhase::Open
        );
        assert_eq!(
            calendar.session_window("fx", ms("2026-10-12T16:00:00Z")),
            Some(SessionWindow {
                open_utc_ms: ms("2026-10-11T21:00:00Z"),
                close_utc_ms: ms("2026-10-16T21:00:00Z")
            })
        );
        assert_eq!(
            calendar.next_transition("fx", ms("2026-10-12T16:00:00Z")),
            Some((ms("2026-10-16T21:00:00Z"), SessionPhase::Closed))
        );
    }

    #[test]
    fn fx_daily_roll_does_not_follow_merged_weekly_open() {
        let mut calendar = MarketCalendarService::default();
        let hours = [7, 1, 2, 3, 4, 5]
            .into_iter()
            .map(|weekday| ProviderSessionHours {
                weekday,
                open_seconds: if weekday == 7 { 17 * 3_600 } else { 0 },
                close_seconds: if weekday == 5 { 17 * 3_600 } else { 86_400 },
                timezone: "America/New_York".into(),
            })
            .collect();
        calendar.insert("fx".into(), CalendarSource::ContractHours(hours));
        let before = ms("2026-08-17T20:59:00Z");
        let after = ms("2026-08-17T21:01:00Z");
        assert_eq!(calendar.phase("fx", before), SessionPhase::Open);
        assert_eq!(
            calendar.session_open("fx", before),
            Some(ms("2026-08-16T21:00:00Z"))
        );
        assert_eq!(
            calendar.session_open("fx", after),
            Some(ms("2026-08-17T21:00:00Z"))
        );
        assert_ne!(
            calendar.bar_bucket("fx", before, CalendarBarPeriod::Session(1)),
            calendar.bar_bucket("fx", after, CalendarBarPeriod::Session(1))
        );
    }
}
