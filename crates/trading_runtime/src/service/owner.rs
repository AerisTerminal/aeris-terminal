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
        if refresh_due || retention_due {
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
        let command = match commands.recv_timeout(deadline.saturating_sub(core.clock.monotonic())) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        };
        let Some(command) = command else {
            break;
        };
        if !core.handle(command, core.clock.wall_unix_nanos()) {
            break;
        }
    }
}
