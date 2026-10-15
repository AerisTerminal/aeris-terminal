//! The sole trading thread waits for commands or the next broker refresh deadline.

use super::Command;
use crate::{TradingCore, UNREALIZED_PNL_INTERVAL};
use std::sync::mpsc::{self, Receiver};

pub(crate) const OWNER_THREAD_NAME: &str = "aeris-trading-owner";

pub(crate) fn run(core: &mut TradingCore, commands: &Receiver<Command>) {
    let mut next_refresh = core.clock.monotonic();
    loop {
        let command = if core.wants_unrealized_pnl() {
            let now = core.clock.monotonic();
            if now >= next_refresh {
                core.on_timer(core.clock.wall_unix_nanos());
                next_refresh = now + UNREALIZED_PNL_INTERVAL;
            }
            match commands.recv_timeout(next_refresh.saturating_sub(core.clock.monotonic())) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            }
        } else {
            commands.recv().ok()
        };
        let Some(command) = command else {
            break;
        };
        if !core.handle(command, core.clock.wall_unix_nanos()) {
            break;
        }
    }
}
