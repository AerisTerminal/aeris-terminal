//! The durable per-account risk session: when it started, which realized-P&L baseline its
//! daily meters measure from, and the boundary at which the owner rolls it over.

use aeris_instruments::calendar::chicago_utc_offset_seconds;
use aeris_trading::{FixedPoint, TradingAccountId};

use super::RiskProfile;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const DAY_SECONDS: i64 = 86_400;
const DAY_NANOS: i64 = DAY_SECONDS * NANOS_PER_SECOND;
/// 17:00 local time: the futures session close behind the Chicago boundary.
const CHICAGO_CLOSE_SECONDS: i64 = 17 * 3_600;

/// The local-time-of-day boundary at which an account's risk session rolls over.
///
/// The default is 00:00 UTC; accounts that trade futures conventionally configure the 17:00
/// America/Chicago close. That per-account default is the Q8 assumption recorded in the
/// stabilization plan and is applied by the desktop profile settings, not here.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RiskSessionBoundary {
    /// The session rolls at 00:00 UTC.
    #[default]
    UtcMidnight,
    /// The session rolls at 17:00 America/Chicago (22:00 UTC during CDT, 23:00 during CST).
    ChicagoClose,
}

impl RiskSessionBoundary {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UtcMidnight => "utc_midnight",
            Self::ChicagoClose => "chicago_close",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "utc_midnight" => Ok(Self::UtcMidnight),
            "chicago_close" => Ok(Self::ChicagoClose),
            _ => Err("risk session boundary is invalid".to_string()),
        }
    }

    /// The first boundary instant strictly after `unix_nanos`.
    #[must_use]
    pub fn next_after(self, unix_nanos: i64) -> i64 {
        match self {
            Self::UtcMidnight => (unix_nanos.div_euclid(DAY_NANOS) + 1) * DAY_NANOS,
            Self::ChicagoClose => {
                let seconds = unix_nanos.div_euclid(NANOS_PER_SECOND);
                let local = seconds + chicago_utc_offset_seconds(seconds);
                let mut close_local =
                    local.div_euclid(DAY_SECONDS) * DAY_SECONDS + CHICAGO_CLOSE_SECONDS;
                if close_local <= local {
                    close_local += DAY_SECONDS;
                }
                chicago_close_unix_seconds(close_local) * NANOS_PER_SECOND
            }
        }
    }

    /// The boundary instant at or before `unix_nanos`: the start of the session containing it.
    #[must_use]
    pub fn session_start_containing(self, unix_nanos: i64) -> i64 {
        match self {
            Self::UtcMidnight => unix_nanos.div_euclid(DAY_NANOS) * DAY_NANOS,
            Self::ChicagoClose => {
                let seconds = unix_nanos.div_euclid(NANOS_PER_SECOND);
                let local = seconds + chicago_utc_offset_seconds(seconds);
                let mut close_local =
                    local.div_euclid(DAY_SECONDS) * DAY_SECONDS + CHICAGO_CLOSE_SECONDS;
                if close_local > local {
                    close_local -= DAY_SECONDS;
                }
                chicago_close_unix_seconds(close_local) * NANOS_PER_SECOND
            }
        }
    }
}

/// Converts one Chicago local 17:00 timestamp to Unix seconds. The DST offset is recomputed
/// at the candidate instant until it agrees with itself; the offset changes only at 02:00
/// local, never between 17:00 local and its Unix projection, so this converges at once.
fn chicago_close_unix_seconds(close_local_seconds: i64) -> i64 {
    let mut utc = close_local_seconds - chicago_utc_offset_seconds(close_local_seconds);
    loop {
        let refined = close_local_seconds - chicago_utc_offset_seconds(utc);
        if refined == utc {
            return utc;
        }
        utc = refined;
    }
}

/// One account's durable risk session: when it started and the all-time realized-P&L
/// baseline its daily meters measure from. Rolled at `boundary` by the owner timer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskSession {
    pub account_id: TradingAccountId,
    pub boundary: RiskSessionBoundary,
    pub session_start_unix_nanos: i64,
    pub session_start_realized_pnl: FixedPoint,
}

impl RiskSession {
    /// The initial session seeded from a registering profile's baseline fields.
    pub(crate) fn from_profile(profile: &RiskProfile) -> Self {
        Self {
            account_id: profile.account_id.clone(),
            boundary: RiskSessionBoundary::default(),
            session_start_unix_nanos: profile.session_start_unix_nanos,
            session_start_realized_pnl: profile.session_start_realized_pnl,
        }
    }

    /// Validates the durable session identity and start.
    ///
    /// # Errors
    /// Returns an error when the session start is not a positive timestamp.
    pub fn validate(&self) -> Result<(), String> {
        if self.session_start_unix_nanos <= 0 {
            return Err("risk session start must be positive".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn nanos(rfc3339: &str) -> i64 {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("timestamp")
            .timestamp_nanos_opt()
            .expect("in range")
    }

    #[test]
    fn utc_midnight_rolls_at_the_day_boundary() {
        let boundary = RiskSessionBoundary::UtcMidnight;
        assert_eq!(
            boundary.next_after(nanos("2026-10-17T23:59:59Z")),
            nanos("2026-10-18T00:00:00Z")
        );
        assert_eq!(
            boundary.next_after(nanos("2026-10-18T00:00:00Z")),
            nanos("2026-10-19T00:00:00Z")
        );
        assert_eq!(
            boundary.session_start_containing(nanos("2026-10-17T12:00:00Z")),
            nanos("2026-10-17T00:00:00Z")
        );
    }

    #[test]
    fn chicago_close_rolls_at_2200z_during_cdt_and_2300z_during_cst() {
        let boundary = RiskSessionBoundary::ChicagoClose;
        // July: daylight time, UTC-5.
        assert_eq!(
            boundary.next_after(nanos("2026-07-01T21:59:59Z")),
            nanos("2026-07-01T22:00:00Z")
        );
        assert_eq!(
            boundary.session_start_containing(nanos("2026-07-01T23:00:00Z")),
            nanos("2026-07-01T22:00:00Z")
        );
        // January: standard time, UTC-6.
        assert_eq!(
            boundary.next_after(nanos("2026-01-15T22:30:00Z")),
            nanos("2026-01-15T23:00:00Z")
        );
        assert_eq!(
            boundary.session_start_containing(nanos("2026-01-15T23:00:00Z")),
            nanos("2026-01-15T23:00:00Z")
        );
    }

    #[test]
    fn chicago_close_stays_at_1700_local_across_the_spring_forward() {
        let boundary = RiskSessionBoundary::ChicagoClose;
        // 2026-03-08 springs forward at 02:00 local; 17:00 that day is already CDT.
        assert_eq!(
            boundary.next_after(nanos("2026-03-07T23:00:00Z")),
            nanos("2026-03-08T22:00:00Z")
        );
        // The autumn boundary before the fallback stays on CDT until 2026-11-01 02:00 local.
        assert_eq!(
            boundary.session_start_containing(nanos("2026-11-01T20:00:00Z")),
            nanos("2026-10-31T22:00:00Z")
        );
        assert_eq!(
            boundary.session_start_containing(nanos("2026-11-02T00:00:00Z")),
            nanos("2026-11-01T23:00:00Z")
        );
    }
}
