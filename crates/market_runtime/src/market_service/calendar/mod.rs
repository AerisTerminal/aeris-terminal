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

/// Calendar capability advertised by catalog metadata, before a dated provider
/// projection is available. This does not assume a calendar for unknown venues.
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

/// Provider projection valid only over its advertised interval. The underlying
/// schedule remains available once that projection expires.
#[derive(Clone, Debug)]
pub struct ProviderCalendar {
    pub status: MarketSessionStatus,
    pub valid_from_utc_ms: i64,
    pub valid_until_utc_ms: i64,
    fallback: Box<CalendarSource>,
}

/// One instrument's selected calendar. A provider calendar takes precedence over
/// contract hours, which take precedence over exchange and continuous rules.
#[derive(Clone, Debug)]
pub enum CalendarSource {
    ProviderCalendar(ProviderCalendar),
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

impl CalendarSource {
    #[must_use]
    pub fn kind(&self) -> CalendarSourceKind {
        match self {
            Self::ProviderCalendar(_) => CalendarSourceKind::ProviderCalendar,
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
    /// Select a source in descending order of specificity. A missing or
    /// unavailable provider projection must not shadow contract hours.
    pub fn resolve(
        &mut self,
        instrument: &InstallProviderInstrument,
        provider_status: Option<MarketSessionStatus>,
    ) {
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
        let instrument_id = instrument.instrument_id.clone();
        self.insert(instrument_id.clone(), source);
        if let Some(status) =
            provider_status.filter(|status| status.source == MarketSessionSource::ProviderCalendar)
            && let (Some(start), Some(end)) = (
                status.session_start_unix_nanos,
                status.session_end_unix_nanos,
            )
        {
            self.publish_provider_calendar(
                &instrument_id,
                status,
                start.div_euclid(1_000_000),
                end.div_euclid(1_000_000),
            );
        }
    }

    /// Replace a dated provider projection without losing the catalog schedule.
    /// A malformed or open-ended interval cannot shadow an exchange rule.
    pub fn publish_provider_calendar(
        &mut self,
        instrument_id: &str,
        status: MarketSessionStatus,
        valid_from_utc_ms: i64,
        valid_until_utc_ms: i64,
    ) {
        if status.source != MarketSessionSource::ProviderCalendar
            || status.instrument_id != instrument_id
            || valid_from_utc_ms >= valid_until_utc_ms
        {
            return;
        }
        let previous = self
            .sources
            .remove(instrument_id)
            .unwrap_or(CalendarSource::Unknown);
        let fallback = match previous {
            CalendarSource::ProviderCalendar(provider) => provider.fallback,
            other => Box::new(other),
        };
        self.sources.insert(
            instrument_id.into(),
            CalendarSource::ProviderCalendar(ProviderCalendar {
                status,
                valid_from_utc_ms,
                valid_until_utc_ms,
                fallback,
            }),
        );
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
        // candle, not tomorrow's session. Do not invent a session on a holiday.
        if self.sources.get(instrument_id).is_some_and(|source| {
            matches!(source, CalendarSource::ProviderCalendar(provider)
                if (provider.valid_from_utc_ms..provider.valid_until_utc_ms).contains(&at_utc_ms))
        }) || window.open_utc_ms - at_utc_ms > 2 * 3_600_000
        {
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
        let source = match self.sources.get(instrument_id)? {
            CalendarSource::ProviderCalendar(provider) => provider.fallback.as_ref(),
            source => source,
        };
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

    /// Identity of a live bar. A provider holiday has no live bucket, and a
    /// dated provider projection is consulted before recurring exchange rules.
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
        let day = self.trading_day(instrument_id, at_utc_ms)?;
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
        let source = match self.sources.get(instrument_id) {
            Some(CalendarSource::ProviderCalendar(provider))
                if (provider.valid_from_utc_ms..provider.valid_until_utc_ms)
                    .contains(&at_utc_ms) =>
            {
                return provider.status.clone();
            }
            Some(CalendarSource::ProviderCalendar(provider)) => provider.fallback.as_ref(),
            Some(source) => source,
            None => return unknown(),
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
            _ => unknown(),
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
    fn calendar_source_priority_uses_provider_then_hours_then_venue() {
        let mut instrument = InstallProviderInstrument {
            instrument_id: "calendar-fixture".into(),
            venue_id: "CME".into(),
            ..Default::default()
        };
        let mut calendar = MarketCalendarService::default();
        let id = instrument.instrument_id.clone();
        instrument.venue_id = "CME".into();
        calendar.resolve(&instrument, None);
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
        calendar.resolve(&instrument, None);
        assert_eq!(calendar.source_kind(&id), CalendarSourceKind::ContractHours);
        calendar.resolve(
            &instrument,
            Some(MarketSessionStatus {
                instrument_id: id.clone(),
                phase: MarketSessionPhase::Closed,
                source: MarketSessionSource::ProviderCalendar,
                session_start_unix_nanos: Some(ms("2026-08-17T13:00:00Z") * 1_000_000),
                session_end_unix_nanos: Some(ms("2026-08-17T15:00:00Z") * 1_000_000),
                next_open_unix_nanos: None,
            }),
        );
        assert_eq!(
            calendar.source_kind(&id),
            CalendarSourceKind::ProviderCalendar
        );
    }

    #[test]
    fn calendar_provider_resolution_matrix_and_status_delegation() {
        let at = ms("2026-08-17T14:00:00Z");
        let mut service = MarketCalendarService::default();
        for (provider, venue, expected) in [
            ("rithmic", "CME", CalendarSourceKind::ExchangeRule),
            ("hyperliquid", "HYPERLIQUID", CalendarSourceKind::AlwaysOpen),
            ("ctrader", "", CalendarSourceKind::ContractHours),
            ("tastytrade", "NYSE", CalendarSourceKind::ProviderCalendar),
        ] {
            let instrument = InstallProviderInstrument {
                provider: provider.into(),
                instrument_id: provider.into(),
                venue_id: venue.into(),
                contract_metadata: (provider == "ctrader").then(|| {
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
            let published = (provider == "tastytrade").then(|| MarketSessionStatus {
                instrument_id: provider.into(),
                phase: MarketSessionPhase::Regular,
                source: MarketSessionSource::ProviderCalendar,
                session_start_unix_nanos: Some(at * 1_000_000 - 60_000_000_000),
                session_end_unix_nanos: Some(at * 1_000_000 + 60_000_000_000),
                next_open_unix_nanos: None,
            });
            service.resolve(&instrument, published);
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

    #[test]
    fn provider_calendar_expires_into_exchange_rule() {
        let instrument = InstallProviderInstrument {
            instrument_id: "future".into(),
            venue_id: "CME".into(),
            ..Default::default()
        };
        let mut calendar = MarketCalendarService::default();
        calendar.resolve(&instrument, None);
        let monday = ms("2026-08-17T18:00:00Z");
        calendar.publish_provider_calendar(
            "future",
            MarketSessionStatus {
                instrument_id: "future".into(),
                phase: MarketSessionPhase::Closed,
                source: MarketSessionSource::ProviderCalendar,
                session_start_unix_nanos: None,
                session_end_unix_nanos: None,
                next_open_unix_nanos: None,
            },
            monday - 60_000,
            monday + 60_000,
        );
        assert_eq!(calendar.phase("future", monday), SessionPhase::Closed);
        assert_eq!(
            calendar.phase("future", monday + 120_000),
            SessionPhase::Open,
            "a holiday projection cannot shadow exchange rules after its dated window"
        );
    }

    #[test]
    fn provider_holiday_overrides_an_open_exchange_rule() {
        let mut calendar = MarketCalendarService::default();
        calendar.insert(
            "future".into(),
            CalendarSource::ExchangeRule(ExchangeRule::CmeGlobex),
        );
        let midday = ms("2026-08-17T18:00:00Z");
        calendar.publish_provider_calendar(
            "future",
            MarketSessionStatus {
                instrument_id: "future".into(),
                phase: MarketSessionPhase::Closed,
                source: MarketSessionSource::ProviderCalendar,
                session_start_unix_nanos: None,
                session_end_unix_nanos: None,
                next_open_unix_nanos: None,
            },
            midday - 3_600_000,
            midday + 3_600_000,
        );
        assert_eq!(
            calendar.require_practice_session("future", midday),
            Err(CalendarSessionError::Closed)
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
        for (before, open) in [
            ("2026-02-08T22:59:00Z", "2026-02-08T23:00:00Z"),
            ("2026-03-08T21:59:00Z", "2026-03-08T22:00:00Z"),
            ("2026-11-01T22:59:00Z", "2026-11-01T23:00:00Z"),
        ] {
            assert_eq!(calendar.phase("future", ms(before)), SessionPhase::Closed);
            assert_eq!(
                calendar.next_transition("future", ms(before)),
                Some((ms(open), SessionPhase::Open))
            );
            assert_eq!(calendar.phase("future", ms(open)), SessionPhase::Open);
            assert_eq!(
                calendar
                    .session_window("future", ms(open))
                    .map(|window| window.open_utc_ms),
                Some(ms(open))
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
        assert_eq!(
            calendar.phase("cfe", ms("2026-08-17T18:00:00Z")),
            SessionPhase::Open
        );
        assert_eq!(
            calendar.phase("equity", ms("2026-08-17T13:00:00Z")),
            SessionPhase::Extended
        );
        assert_eq!(
            calendar.phase("equity", ms("2026-08-17T14:00:00Z")),
            SessionPhase::Open
        );
        assert_eq!(
            calendar.phase("equity", ms("2026-08-17T22:00:00Z")),
            SessionPhase::Extended
        );
        assert_eq!(
            calendar.phase("crypto", ms("2026-08-17T22:00:00Z")),
            SessionPhase::AlwaysOpen
        );
        assert_eq!(
            calendar.next_transition("crypto", ms("2026-08-17T22:00:00Z")),
            None
        );
        assert_eq!(
            calendar.phase("unlisted", ms("2026-08-17T22:00:00Z")),
            SessionPhase::Unknown
        );
        assert_eq!(
            calendar.session_window("unlisted", ms("2026-08-17T22:00:00Z")),
            None
        );
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
            calendar.next_transition("fx", ms("2026-10-10T16:00:00Z")),
            Some((ms("2026-10-11T21:00:00Z"), SessionPhase::Open))
        );
        assert_eq!(
            calendar.phase("fx", ms("2026-10-12T16:00:00Z")),
            SessionPhase::Open
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
