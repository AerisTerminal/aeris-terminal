//! The sole trading thread waits for commands or the next broker refresh deadline.

use super::Command;
use crate::core::RETENTION_INTERVAL;
use crate::{TradingCore, UNREALIZED_PNL_INTERVAL};
use std::sync::mpsc::{self, Receiver};

pub(crate) const OWNER_THREAD_NAME: &str = "aeris-trading-owner";

pub(crate) fn run(core: &mut TradingCore, commands: &Receiver<Command>) {
    let mut next_refresh = core.clock.monotonic();
    let mut next_retention = core.clock.monotonic() + RETENTION_INTERVAL;
    loop {
        let now = core.clock.monotonic();
        let refresh_due = core.wants_unrealized_pnl() && now >= next_refresh;
        let retention_due = now >= next_retention;
        let wall_now = core.clock.wall_unix_nanos();
        let market_due = core
            .next_market_timeout_unix_nanos()
            .is_some_and(|deadline| wall_now >= deadline);
        if refresh_due || retention_due || market_due {
            core.on_timer(core.clock.wall_unix_nanos());
            if refresh_due {
                next_refresh = now + UNREALIZED_PNL_INTERVAL;
            }
            if retention_due {
                next_retention = now + RETENTION_INTERVAL;
            }
        }
        let deadline = if core.wants_unrealized_pnl() {
            next_refresh.min(next_retention)
        } else {
            next_retention
        };
        let wait = deadline.saturating_sub(core.clock.monotonic());
        let wait = core
            .next_market_timeout_unix_nanos()
            .map_or(wait, |market| {
                let until_market = market.saturating_sub(core.clock.wall_unix_nanos());
                // A failed SQLite commit leaves the deadline due. Retry on a bounded
                // cadence instead of spinning the owner at zero-duration timeouts.
                let until_market = if until_market == 0 {
                    1_000_000_000
                } else {
                    until_market
                };
                wait.min(std::time::Duration::from_nanos(
                    u64::try_from(until_market).unwrap_or_default(),
                ))
            });
        let command = match commands.recv_timeout(wait) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        };
        let Some(command) = command else {
            break;
        };
        // A command can wake the receiver exactly as its market deadline becomes
        // due. Expire first so a snapshot or a late quote cannot observe/fill it.
        let command_now = core.clock.wall_unix_nanos();
        if core
            .next_market_timeout_unix_nanos()
            .is_some_and(|market| command_now >= market)
        {
            core.on_timer(command_now);
        }
        if !core.handle(command, command_now) {
            break;
        }
    }
}
