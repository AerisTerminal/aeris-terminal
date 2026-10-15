use super::ClientId;
use std::{
    collections::BTreeSet,
    sync::{Condvar, Mutex},
    time::Duration,
};

/// Signals a client only when its coordinator-owned consumer queues have work.
/// The queues themselves remain on the coordinator thread; this edge never
/// exposes an event to a retired client or makes a producer wait for the UI.
#[derive(Default)]
pub(super) struct ConsumerOutbox {
    state: Mutex<OutboxState>,
    changed: Condvar,
}

#[derive(Default)]
struct OutboxState {
    attached: BTreeSet<ClientId>,
    ready: BTreeSet<ClientId>,
}

impl ConsumerOutbox {
    pub(super) fn attach(&self, client: ClientId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .attached
            .insert(client);
    }

    pub(super) fn notify(&self, client: ClientId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.ready.insert(client) {
            self.changed.notify_all();
        }
    }

    pub(super) fn notify_attached(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attached = state.attached.iter().copied().collect::<Vec<_>>();
        for client in attached {
            state.ready.insert(client);
        }
        self.changed.notify_all();
    }

    pub(super) fn retire(&self, client: ClientId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.attached.remove(&client);
        state.ready.remove(&client);
    }

    pub(super) fn wait(&self, client: ClientId, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| !state.ready.contains(&client))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.ready.remove(&client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{num::NonZeroU64, sync::Arc, thread};

    fn client(id: u64) -> ClientId {
        ClientId(NonZeroU64::new(id).expect("nonzero client"))
    }

    #[test]
    fn wait_is_scoped_to_client_and_consumes_one_notification() {
        let outbox = Arc::new(ConsumerOutbox::default());
        let waiting = Arc::clone(&outbox);
        let handle = thread::spawn(move || waiting.wait(client(2), Duration::from_secs(1)));
        outbox.notify(client(1));
        outbox.notify(client(2));
        assert!(handle.join().expect("waiter"));
        assert!(!outbox.wait(client(2), Duration::ZERO));
        assert!(outbox.wait(client(1), Duration::ZERO));
    }

    #[test]
    fn retired_client_cannot_consume_an_old_notification() {
        let outbox = ConsumerOutbox::default();
        outbox.notify(client(1));
        outbox.retire(client(1));
        assert!(!outbox.wait(client(1), Duration::ZERO));
    }
}
