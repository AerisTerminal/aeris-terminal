//! One bounded FIFO for a pane's runtime envelopes and local presentation outcomes.

use super::MarketWorkerSendError;
use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

const CONTROL_RESERVE: usize = 8;

struct PaneProjectionQueue<T> {
    queue: Mutex<VecDeque<T>>,
    capacity: usize,
    is_publication: fn(&T) -> bool,
    sender_count: AtomicUsize,
    receiver_alive: AtomicBool,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    wake_pending: AtomicBool,
    publications_enabled: AtomicBool,
}

fn wake<T>(state: &PaneProjectionQueue<T>) {
    if !state.receiver_alive.load(Ordering::Acquire)
        || state.wake_pending.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let callback = state
        .wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    match callback {
        Some(callback) => callback(),
        None => state.wake_pending.store(false, Ordering::Release),
    }
}

pub struct PaneProjectionSender<T> {
    state: Arc<PaneProjectionQueue<T>>,
}

pub struct PaneProjectionReceiver<T> {
    state: Arc<PaneProjectionQueue<T>>,
}

impl<T> Clone for PaneProjectionSender<T> {
    fn clone(&self) -> Self {
        self.state.sender_count.fetch_add(1, Ordering::Relaxed);
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<T> Drop for PaneProjectionSender<T> {
    fn drop(&mut self) {
        if self.state.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            wake(&self.state);
        }
    }
}

impl<T> PaneProjectionSender<T> {
    /// The producer reserves capacity before polling canonical runtime events.
    /// Full is explicit backpressure, never permission to silently drop an event.
    pub fn send(&self, event: T) -> Result<(), MarketWorkerSendError> {
        if !self.state.receiver_alive.load(Ordering::Acquire) {
            return Err(MarketWorkerSendError::Disconnected);
        }
        if !self.state.publications_enabled.load(Ordering::Acquire)
            && (self.state.is_publication)(&event)
        {
            return Ok(());
        }
        {
            let mut queue = self
                .state
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.state.receiver_alive.load(Ordering::Acquire) {
                return Err(MarketWorkerSendError::Disconnected);
            }
            let limit = if (self.state.is_publication)(&event) {
                self.state.capacity
            } else {
                self.state.capacity.saturating_add(CONTROL_RESERVE)
            };
            if queue.len() >= limit {
                return Err(MarketWorkerSendError::Full);
            }
            queue.push_back(event);
        }
        wake(&self.state);
        Ok(())
    }

    pub fn occupancy(&self) -> (usize, usize) {
        let current = self
            .state
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        (current, self.state.capacity)
    }
}

impl<T> PaneProjectionReceiver<T> {
    pub(super) fn set_market_publications_enabled(&self, enabled: bool) {
        let mut queue = self
            .state
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.state.publications_enabled.load(Ordering::Acquire) == enabled {
            return;
        }
        queue.retain(|event| !(self.state.is_publication)(event));
        self.state
            .publications_enabled
            .store(enabled, Ordering::Release);
    }

    pub fn set_wake(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        *self
            .state
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(callback);
        if !self
            .state
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
        {
            wake(&self.state);
        }
    }

    #[must_use]
    pub fn drain(&self) -> (Vec<T>, bool) {
        self.drain_up_to(usize::MAX)
    }

    /// Drain at most one frame's budget; wake again when any event remains.
    pub fn drain_up_to(&self, limit: usize) -> (Vec<T>, bool) {
        let mut queue = self
            .state
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let end = limit.min(queue.len());
        let events = queue.drain(..end).collect();
        drop(queue);
        self.state.wake_pending.store(false, Ordering::Release);
        let (queued, disconnected) = {
            let queue = self
                .state
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                !queue.is_empty(),
                self.state.sender_count.load(Ordering::Acquire) == 0,
            )
        };
        if queued {
            wake(&self.state);
        }
        (events, disconnected)
    }
}

impl<T> Drop for PaneProjectionReceiver<T> {
    fn drop(&mut self) {
        self.state.receiver_alive.store(false, Ordering::Release);
        *self
            .state
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.state
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

#[must_use]
pub fn pane_projection_channel<T>(
    capacity: NonZeroUsize,
    is_publication: fn(&T) -> bool,
) -> (PaneProjectionSender<T>, PaneProjectionReceiver<T>) {
    let state = Arc::new(PaneProjectionQueue {
        queue: Mutex::new(VecDeque::with_capacity(capacity.get().saturating_add(1))),
        capacity: capacity.get(),
        is_publication,
        sender_count: AtomicUsize::new(1),
        receiver_alive: AtomicBool::new(true),
        wake: Mutex::new(None),
        wake_pending: AtomicBool::new(false),
        publications_enabled: AtomicBool::new(true),
    });
    (
        PaneProjectionSender {
            state: Arc::clone(&state),
        },
        PaneProjectionReceiver { state },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_projection_fifo_budget_and_overflow() {
        let (sender, receiver) =
            pane_projection_channel(NonZeroUsize::new(10_000).unwrap(), |_| true);
        for sequence in 0..10_000 {
            sender.send(sequence).unwrap();
        }
        assert_eq!(sender.send(10_000), Err(MarketWorkerSendError::Full));
        for frame in 0..157 {
            let (events, disconnected) = receiver.drain_up_to(64);
            let start = frame * 64;
            assert_eq!(
                events,
                (start..(start + 64).min(10_000)).collect::<Vec<_>>()
            );
            assert!(!disconnected);
        }
        assert_eq!(sender.occupancy().0, 0);
    }
}
