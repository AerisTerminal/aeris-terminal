//! Bounded broker request queue with an owner-held reservation through a SQLite commit.
//! A failed write drops the reservation, so no relay request escapes a rolled-back fact.

use std::collections::VecDeque;
use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::venue::VenueRequest;

struct QueueState {
    entries: VecDeque<VenueRequest>,
    senders: usize,
    receiving: bool,
}

struct Shared {
    state: Mutex<QueueState>,
    ready: Condvar,
    capacity: usize,
}

pub struct VenueRequestSender(Arc<Shared>);
pub struct VenueRequestReceiver(Arc<Shared>);

pub struct VenueReservation<'a> {
    shared: &'a Shared,
    state: MutexGuard<'a, QueueState>,
    request: VenueRequest,
}

/// # Panics
/// Panics if capacity is zero.
#[must_use]
pub fn bounded_venue_requests(capacity: usize) -> (VenueRequestSender, VenueRequestReceiver) {
    assert!(capacity > 0, "venue request capacity must be positive");
    let shared = Arc::new(Shared {
        state: Mutex::new(QueueState {
            entries: VecDeque::with_capacity(capacity),
            senders: 1,
            receiving: true,
        }),
        ready: Condvar::new(),
        capacity,
    });
    (
        VenueRequestSender(shared.clone()),
        VenueRequestReceiver(shared),
    )
}

impl Clone for VenueRequestSender {
    fn clone(&self) -> Self {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .senders += 1;
        Self(self.0.clone())
    }
}

impl Drop for VenueRequestSender {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.senders -= 1;
        self.0.ready.notify_all();
    }
}

impl Drop for VenueRequestReceiver {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.receiving = false;
        self.0.ready.notify_all();
    }
}

impl VenueRequestSender {
    /// # Errors
    /// Returns the unsent request if full or disconnected.
    pub fn try_reserve(
        &self,
        request: VenueRequest,
    ) -> Result<VenueReservation<'_>, TrySendError<Box<VenueRequest>>> {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.receiving {
            return Err(TrySendError::Disconnected(Box::new(request)));
        }
        if state.entries.len() >= self.0.capacity {
            return Err(TrySendError::Full(Box::new(request)));
        }
        Ok(VenueReservation {
            shared: &self.0,
            state,
            request,
        })
    }

    /// # Errors
    /// Returns the unsent request if full or disconnected.
    pub fn try_send(&self, request: VenueRequest) -> Result<(), TrySendError<Box<VenueRequest>>> {
        self.try_reserve(request)?.commit();
        Ok(())
    }

    /// # Errors
    /// Returns the unsent request if the receiver disconnects.
    pub fn send(&self, request: VenueRequest) -> Result<(), SendError<Box<VenueRequest>>> {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.receiving && state.entries.len() == self.0.capacity {
            state = self
                .0
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if !state.receiving {
            return Err(SendError(Box::new(request)));
        }
        state.entries.push_back(request);
        self.0.ready.notify_one();
        Ok(())
    }
}

impl VenueReservation<'_> {
    pub fn commit(mut self) {
        self.state.entries.push_back(self.request);
        self.shared.ready.notify_one();
    }
}

impl VenueRequestReceiver {
    /// # Errors
    /// Returns `Empty` or `Disconnected` when no request can be read.
    pub fn try_recv(&self) -> Result<VenueRequest, TryRecvError> {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = match state.entries.pop_front() {
            Some(request) => Ok(request),
            None if state.senders == 0 => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        };
        self.0.ready.notify_one();
        result
    }

    /// # Errors
    /// Returns `RecvError` when all senders disconnect and the queue has drained.
    pub fn recv(&self) -> Result<VenueRequest, RecvError> {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(request) = state.entries.pop_front() {
                self.0.ready.notify_one();
                return Ok(request);
            }
            if state.senders == 0 {
                return Err(RecvError);
            }
            state = self
                .0
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// # Errors
    /// Returns `Timeout` if no request arrives by the deadline, or `Disconnected`.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<VenueRequest, RecvTimeoutError> {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (next, _) = self
            .0
            .ready
            .wait_timeout_while(state, timeout, |current| {
                current.entries.is_empty() && current.senders > 0
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
        let result = match state.entries.pop_front() {
            Some(request) => Ok(request),
            None if state.senders == 0 => Err(RecvTimeoutError::Disconnected),
            None => Err(RecvTimeoutError::Timeout),
        };
        self.0.ready.notify_one();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> VenueRequest {
        VenueRequest::Reconcile {
            broker_account: "demo".into(),
            deals_since_unix_nanos: None,
        }
    }

    #[test]
    fn dropped_reservation_is_not_visible_and_capacity_recovers() {
        let (sender, receiver) = bounded_venue_requests(1);
        drop(sender.try_reserve(request()).expect("reserve"));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
        sender
            .try_reserve(request())
            .expect("reserve again")
            .commit();
        assert!(matches!(
            sender.try_send(request()),
            Err(TrySendError::Full(_))
        ));
        assert_eq!(receiver.recv().expect("committed"), request());
        sender.try_send(request()).expect("capacity restored");
    }

    #[test]
    fn disconnected_receiver_rejects_without_committing() {
        let (sender, receiver) = bounded_venue_requests(1);
        drop(receiver);
        assert!(matches!(
            sender.try_reserve(request()),
            Err(TrySendError::Disconnected(_))
        ));
    }
}
