//! Provider-neutral, clock-driven liveness policy for live series.

use std::time::{Duration, Instant};

use aeris_instruments::SessionPhase;
use aeris_market_engine::ProviderHealth;

pub(super) const OPEN_STALL_THRESHOLD: Duration = Duration::from_secs(30);
/// Unknown calendars receive ten times the open-market grace period.
pub(super) const UNKNOWN_STALL_THRESHOLD: Duration = Duration::from_secs(300);
/// A quiet series' threshold doubles after each recovery no trade followed,
/// at most this many times: one recovery per eight minutes on an open market.
const MAXIMUM_QUIET_RECOVERY_DOUBLINGS: u32 = 4;

/// The extra silence a trade-built series is allowed after `recoveries`
/// consecutive stall recoveries that no trade followed.
pub(super) fn quiet_recovery_grace(recoveries: u32) -> Duration {
    OPEN_STALL_THRESHOLD * ((1_u32 << recoveries.min(MAXIMUM_QUIET_RECOVERY_DOUBLINGS)) - 1)
}

pub(super) fn stalled(
    now: Instant,
    last_progress: Instant,
    health: ProviderHealth,
    phase: SessionPhase,
    connected: bool,
    history_ready: bool,
) -> bool {
    if health != ProviderHealth::Online || !connected || !history_ready {
        return false;
    }
    let threshold = match phase {
        SessionPhase::Open | SessionPhase::Extended | SessionPhase::AlwaysOpen => {
            OPEN_STALL_THRESHOLD
        }
        SessionPhase::Unknown => UNKNOWN_STALL_THRESHOLD,
        SessionPhase::PreOpen | SessionPhase::Break | SessionPhase::Closed => return false,
    };
    now.duration_since(last_progress) >= threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_open_and_always_open_sessions_recover_while_trading_open() {
        let now = Instant::now();
        let old = now
            .checked_sub(OPEN_STALL_THRESHOLD + Duration::from_secs(1))
            .expect("test clock");
        for (provider, phase) in [
            ("rithmic", SessionPhase::Open),
            ("hyperliquid", SessionPhase::AlwaysOpen),
        ] {
            assert!(
                stalled(now, old, ProviderHealth::Online, phase, true, true),
                "{provider}"
            );
        }
    }

    #[test]
    fn watchdog_quiet_recovery_grace_doubles_the_threshold_up_to_a_bound() {
        let thresholds = (0..7)
            .map(|recoveries| (OPEN_STALL_THRESHOLD + quiet_recovery_grace(recoveries)).as_secs())
            .collect::<Vec<_>>();
        assert_eq!(thresholds, [30, 60, 120, 240, 480, 480, 480]);
        assert_eq!(quiet_recovery_grace(u32::MAX), Duration::from_secs(450));
    }

    #[test]
    fn watchdog_closed_candle_series_does_not_recover() {
        let now = Instant::now();
        assert!(!stalled(
            now,
            now.checked_sub(UNKNOWN_STALL_THRESHOLD)
                .expect("test clock"),
            ProviderHealth::Online,
            SessionPhase::Closed,
            true,
            true,
        ));
    }

    #[test]
    fn watchdog_unknown_uses_conservative_threshold() {
        let now = Instant::now();
        assert!(!stalled(
            now,
            now.checked_sub(OPEN_STALL_THRESHOLD).expect("test clock"),
            ProviderHealth::Online,
            SessionPhase::Unknown,
            true,
            true,
        ));
        assert!(stalled(
            now,
            now.checked_sub(UNKNOWN_STALL_THRESHOLD)
                .expect("test clock"),
            ProviderHealth::Online,
            SessionPhase::Unknown,
            true,
            true,
        ));
    }
}
