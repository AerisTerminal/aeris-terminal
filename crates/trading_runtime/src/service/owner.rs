//! The sole trading thread waits for commands or the next timer deadline.

use super::Command;
use crate::core::RETENTION_INTERVAL;
use crate::publication::TradingEventCause;
use crate::{EQUITY_SAMPLE_INTERVAL, TradingCore};
use std::sync::mpsc::{self, Receiver};

pub(crate) const OWNER_THREAD_NAME: &str = "aeris-trading-owner";

pub(crate) fn run(core: &mut TradingCore, commands: &Receiver<Command>) {
    // Retention is due at startup and then hourly.
    let mut next_retention = core.clock.monotonic();
    // The equity timer samples every 60 s, starting one interval after open so a restart
    // replays the durable history unchanged before sampling resumes.
    let mut next_equity = core.clock.monotonic() + EQUITY_SAMPLE_INTERVAL;
    loop {
        let now = core.clock.monotonic();
        let retention_due = now >= next_retention;
        let equity_due = now >= next_equity;
        let wall_now = core.clock.wall_unix_nanos();
        let order_due = core
            .next_order_deadline_unix_nanos()
            .is_some_and(|deadline| wall_now >= deadline);
        if retention_due || equity_due || order_due {
            core.on_timer(core.clock.wall_unix_nanos());
            if retention_due {
                next_retention = now + RETENTION_INTERVAL;
            }
            if equity_due {
                next_equity = now + EQUITY_SAMPLE_INTERVAL;
            }
        }
        // Publishes the timer's changes and any coalesced mark flush that fell due.
        core.publish_changes(TradingEventCause::Timer);
        let mut deadline = next_retention.min(next_equity);
        if let Some(flush) = core.next_publication_flush() {
            deadline = deadline.min(flush);
        }
        let wait = deadline.saturating_sub(core.clock.monotonic());
        let wait = core
            .next_order_deadline_unix_nanos()
            .map_or(wait, |deadline| {
                let until_deadline = deadline.saturating_sub(core.clock.wall_unix_nanos());
                // A failed SQLite commit leaves the deadline due. Retry on a bounded
                // cadence instead of spinning the owner at zero-duration timeouts.
                let until_deadline = if until_deadline == 0 {
                    1_000_000_000
                } else {
                    until_deadline
                };
                wait.min(std::time::Duration::from_nanos(
                    u64::try_from(until_deadline).unwrap_or_default(),
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
        // A command can wake the receiver exactly as an order deadline becomes
        // due. Expire first so a snapshot or a late quote cannot observe/fill it.
        let command_now = core.clock.wall_unix_nanos();
        if core
            .next_order_deadline_unix_nanos()
            .is_some_and(|deadline| command_now >= deadline)
        {
            core.on_timer(command_now);
            core.publish_changes(TradingEventCause::Timer);
        }
        let cause = publication_cause(&command);
        if !core.handle(command, command_now) {
            break;
        }
        core.publish_changes(cause);
    }
}

/// The publication cause of one command: feed drains are market-driven, everything else is a
/// client command. Read-only commands (status, snapshot) publish nothing when state is unchanged.
fn publication_cause(command: &Command) -> TradingEventCause {
    match command {
        Command::DrainMarketFeed => TradingEventCause::Market,
        #[cfg(test)]
        Command::Observe(..) => TradingEventCause::Market,
        _ => TradingEventCause::Command,
    }
}
