//! Bounded handoff between the broker reader and the trading owner.

use crate::{Command, ModifyOrder, PlaceOrder};
use aeris_trading::ClientOrderId;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

pub const INBOX_CAPACITY: usize = 1024;
pub const OUTBOUND_CAPACITY: usize = 64;
const RETRY_INTERVAL: Duration = Duration::from_millis(20);
const MAXIMUM_BLOCKED: Duration = Duration::from_secs(20);

pub enum VenueRequest {
    Place(PlaceOrder),
    Modify(ModifyOrder),
    Cancel(ClientOrderId),
}

pub struct VenueEvent {
    pub session_generation: u64,
    pub client_order_id: ClientOrderId,
    pub update: VenueUpdate,
    pub observed_unix_nanos: i64,
}

pub enum VenueUpdate {
    Accepted,
    Replaced,
    Cancelled,
    CancelRejected(String),
    Expired,
    Rejected(String),
}

#[derive(Default)]
struct PendingEvents {
    queue: VecDeque<VenueEvent>,
    drain_queued: bool,
}

/// Reader blocks instead of dropping broker events; the owner is the only consumer.
pub struct VenueInbox {
    pending: Mutex<PendingEvents>,
    space: Condvar,
    commands: SyncSender<Command>,
    stopping: Arc<AtomicBool>,
    blocked_limit: Duration,
}

impl VenueInbox {
    pub(crate) fn new(commands: SyncSender<Command>, stopping: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(PendingEvents::default()),
            space: Condvar::new(),
            commands,
            stopping,
            blocked_limit: MAXIMUM_BLOCKED,
        })
    }

    #[cfg(test)]
    fn with_blocked_limit(
        commands: SyncSender<Command>,
        stopping: Arc<AtomicBool>,
        blocked_limit: Duration,
    ) -> Arc<Self> {
        let mut inbox = Self::new(commands, stopping);
        Arc::get_mut(&mut inbox).expect("new inbox").blocked_limit = blocked_limit;
        inbox
    }

    /// Waits for space and owner wakeup. A persistently stalled owner causes the session
    /// reader to abandon this generation so the supervisor can reconnect and reconcile.
    /// Enqueues an event losslessly or asks the supervisor to reconnect after sustained overload.
    ///
    /// # Errors
    /// Returns an error on shutdown, owner loss, or a blocked reader beyond the deadline.
    pub fn push(&self, event: VenueEvent) -> Result<(), String> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let blocked_since = Instant::now();
        while pending.queue.len() == INBOX_CAPACITY {
            if self.stopping.load(Ordering::Acquire) {
                return Err("trading owner is stopping".into());
            }
            if blocked_since.elapsed() >= self.blocked_limit {
                return Err("venue inbox stalled; reconnect and reconcile required".into());
            }
            pending = self
                .space
                .wait_timeout(pending, RETRY_INTERVAL)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if self.stopping.load(Ordering::Acquire) {
            return Err("trading owner is stopping".into());
        }
        pending.queue.push_back(event);
        while !pending.drain_queued {
            match self.commands.try_send(Command::DrainVenueEvents) {
                Ok(()) => pending.drain_queued = true,
                Err(TrySendError::Disconnected(_)) => {
                    return Err("trading owner is unavailable".into());
                }
                Err(TrySendError::Full(_)) => {
                    if self.stopping.load(Ordering::Acquire) {
                        return Err("trading owner is stopping".into());
                    }
                    if blocked_since.elapsed() >= self.blocked_limit {
                        return Err("venue inbox stalled; reconnect and reconcile required".into());
                    }
                    pending = self
                        .space
                        .wait_timeout(pending, RETRY_INTERVAL)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn drain(&self) -> VecDeque<VenueEvent> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let drained = std::mem::take(&mut pending.queue);
        pending.drain_queued = false;
        self.space.notify_all();
        drained
    }

    pub(crate) fn wake_stopped(&self) {
        self.space.notify_all();
    }
}

mod owner;

#[cfg(test)]
mod scripted;
