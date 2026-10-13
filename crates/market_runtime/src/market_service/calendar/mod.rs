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

/// Dated provider-published phase and the next known opening.
#[derive(Clone, Debug)]
pub struct ProviderCalendar(pub MarketSessionStatus);

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
#[derive(Default)]
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
    /// Select a source in descending order of specificity. A missing or
    /// unavailable provider projection must not shadow contract hours.
    pub fn resolve(
        &mut self,
        instrument: &InstallProviderInstrument,
        provider_status: Option<MarketSessionStatus>,
    ) {
        let source = if let Some(status) =
            provider_status.filter(|status| status.source == MarketSessionSource::ProviderCalendar)
        {
            CalendarSource::ProviderCalendar(ProviderCalendar(status))
        } else if let Some(hours) = instrument
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
        match self.sources.get(instrument_id) {
            Some(CalendarSource::ProviderCalendar(provider)) => provider.0.clone(),
            Some(CalendarSource::ContractHours(hours)) => {
                weekly_hours::weekly_hours_status(instrument_id, hours, at).unwrap_or_else(unknown)
            }
            Some(CalendarSource::ExchangeRule(rule)) => rule.status(instrument_id, at),
            Some(CalendarSource::AlwaysOpen) => MarketSessionStatus {
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
                session_start_unix_nanos: None,
                session_end_unix_nanos: None,
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
}
