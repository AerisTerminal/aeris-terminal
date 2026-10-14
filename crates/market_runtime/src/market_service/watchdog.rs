//! Provider-neutral, clock-driven liveness policy for live series.

use std::time::{Duration, Instant};

use aeris_instruments::SessionPhase;
use aeris_market_engine::ProviderHealth;

pub(super) const OPEN_STALL_THRESHOLD: Duration = Duration::from_secs(30);
/// Unknown calendars receive ten times the open-market grace period.
pub(super) const UNKNOWN_STALL_THRESHOLD: Duration = Duration::from_secs(300);

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
    fn watchdog_all_four_providers_recover_while_trading_open() {
        let now = Instant::now();
        let old = now
            .checked_sub(OPEN_STALL_THRESHOLD + Duration::from_secs(1))
            .expect("test clock");
        for (provider, phase) in [
            ("ctrader", SessionPhase::Open),
            ("tastytrade", SessionPhase::Open),
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
