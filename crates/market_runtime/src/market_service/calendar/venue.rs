//! One normalized venue classification for calendar and provider collection routing.
use aeris_contracts::InstallProviderInstrument;
use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};
pub use aeris_instruments::calendar::Venue;
use aeris_instruments::calendar::classify_venue;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::America::{Chicago, New_York};
use chrono_tz::Tz;

/// Normalize exchange codes published by different catalogs.
pub fn venue(code: &str) -> Option<Venue> {
    classify_venue(code)
}

/// Resolve the exchange identity supplied by a catalog, including the older
/// futures-symbol encoding.
pub fn instrument_venue(instrument: &InstallProviderInstrument) -> Option<Venue> {
    venue(&instrument.venue_id)
        .or_else(|| {
            instrument
                .provider_symbol
                .rsplit_once(':')
                .and_then(|(_, code)| venue(code))
        })
        .or_else(|| (instrument.provider == "hyperliquid").then_some(Venue::Continuous))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExchangeRule {
    CmeGlobex,
    Cfe,
    UsEquity,
}

impl ExchangeRule {
    pub fn from_venue(venue: Venue) -> Option<Self> {
        match venue {
            Venue::Cme => Some(Self::CmeGlobex),
            Venue::Cfe => Some(Self::Cfe),
            Venue::UsEquity => Some(Self::UsEquity),
            Venue::Continuous => None,
        }
    }

    pub fn status(self, instrument_id: &str, at_nanos: i64) -> MarketSessionStatus {
        let unknown = || MarketSessionStatus {
            instrument_id: instrument_id.into(),
            phase: MarketSessionPhase::Unknown,
            source: MarketSessionSource::Unavailable,
            session_start_unix_nanos: None,
            session_end_unix_nanos: None,
            next_open_unix_nanos: None,
        };
        let Ok(nanos) = u32::try_from(at_nanos.rem_euclid(1_000_000_000)) else {
            return unknown();
        };
        let Some(at) = DateTime::<Utc>::from_timestamp(at_nanos.div_euclid(1_000_000_000), nanos)
        else {
            return unknown();
        };
        let (zone, open_hour, close_hour, open_minute, close_minute) = match self {
            Self::CmeGlobex | Self::Cfe => (Chicago, 17, 16, 0, 0),
            Self::UsEquity => (New_York, 4, 20, 0, 0),
        };
        let local = at.with_timezone(&zone);
        let mut sessions = Vec::with_capacity(12);
        for offset in -2..=9 {
            let Some(day) = local
                .date_naive()
                .checked_add_signed(Duration::days(offset))
            else {
                continue;
            };
            let weekday = day.weekday();
            let (start, end) = match self {
                Self::CmeGlobex | Self::Cfe if weekday == Weekday::Sun => (
                    point(zone, day, open_hour, open_minute),
                    point(zone, day + Duration::days(1), close_hour, close_minute),
                ),
                Self::CmeGlobex | Self::Cfe
                    if matches!(
                        weekday,
                        Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu
                    ) =>
                {
                    (
                        point(zone, day, open_hour, open_minute),
                        point(zone, day + Duration::days(1), close_hour, close_minute),
                    )
                }
                Self::UsEquity if weekday != Weekday::Sat && weekday != Weekday::Sun => (
                    point(zone, day, open_hour, open_minute),
                    point(zone, day, close_hour, close_minute),
                ),
                _ => continue,
            };
            if let (Some(start), Some(end)) = (start, end) {
                sessions.push((start, end));
            }
        }
        let active = sessions
            .iter()
            .find(|(start, end)| *start <= at && at < *end);
        let next = sessions.iter().find(|(start, _)| *start > at);
        let (phase, start, end) = if let Some(&(start, end)) = active {
            if self == Self::UsEquity {
                let minute = local.hour() * 60 + local.minute();
                let regular_open = point(zone, local.date_naive(), 9, 30);
                let regular_close = point(zone, local.date_naive(), 16, 0);
                if (9 * 60 + 30..16 * 60).contains(&minute) {
                    (MarketSessionPhase::Regular, regular_open, regular_close)
                } else if minute < 9 * 60 + 30 {
                    (MarketSessionPhase::PreMarket, Some(start), regular_open)
                } else {
                    (MarketSessionPhase::PostMarket, regular_close, Some(end))
                }
            } else {
                (MarketSessionPhase::Regular, Some(start), Some(end))
            }
        } else if let Some(&(start, end)) = next {
            (MarketSessionPhase::Closed, Some(start), Some(end))
        } else {
            return unknown();
        };
        MarketSessionStatus {
            instrument_id: instrument_id.into(),
            phase,
            source: MarketSessionSource::ProviderCalendar,
            session_start_unix_nanos: start.and_then(|time| time.timestamp_nanos_opt()),
            session_end_unix_nanos: end.and_then(|time| time.timestamp_nanos_opt()),
            next_open_unix_nanos: next.and_then(|(start, _)| start.timestamp_nanos_opt()),
        }
    }
}

fn point(zone: Tz, day: NaiveDate, hour: u32, minute: u32) -> Option<DateTime<Utc>> {
    zone.with_ymd_and_hms(day.year(), day.month(), day.day(), hour, minute, 0)
        .earliest()
        .map(|time| time.to_utc())
}
