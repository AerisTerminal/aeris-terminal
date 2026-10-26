//! The desktop's one subscription-backed trading view.
//!
//! Only the bridge waits on the owner. A single replaceable slot and a single
//! outstanding wake bound work even when the UI is temporarily suspended.

use aeris_trading_runtime::{SubscriptionUpdate, TradingSubscription, WorkingSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Default)]
struct Slot {
    newest: Option<SubscriptionUpdate>,
    wake_pending: bool,
}

impl Slot {
    /// Replace the pending update, returning true only for the first wake.
    fn offer(&mut self, mut update: SubscriptionUpdate) -> bool {
        let replace = self.newest.as_ref().is_none_or(|previous| {
            if update.event.revision == previous.event.revision {
                // Marks can move at the same durable revision. Keep the last
                // publication, but do not lose an earlier lagged resync.
                update.lagged |= previous.lagged;
            }
            update.event.revision >= previous.event.revision
        });
        if replace {
            self.newest = Some(update);
        }
        if replace && !self.wake_pending {
            self.wake_pending = true;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
pub(super) struct TradingStore {
    current: Option<WorkingSet>,
    resync_pending: bool,
    slot: Arc<Mutex<Slot>>,
    stop: Arc<AtomicBool>,
    bridge: Option<JoinHandle<()>>,
}

impl TradingStore {
    pub(super) fn start(
        &mut self,
        subscription: TradingSubscription,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        if self.stop.load(Ordering::Acquire) {
            return Err("trading bridge is already stopping".to_string());
        }
        let slot = Arc::clone(&self.slot);
        let stop = Arc::clone(&self.stop);
        let bridge = std::thread::Builder::new()
            .name("aeris-trading-desktop-bridge".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if let Some(update) = subscription.recv_timeout(Duration::from_secs(1)) {
                        let notify = {
                            let mut state = slot
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            state.offer(update)
                        };
                        if notify {
                            wake();
                        }
                    }
                }
                // Dropping the subscription unsubscribes on the owner thread.
            })
            .map_err(|error| format!("trading bridge could not start: {error}"))?;
        self.bridge = Some(bridge);
        Ok(())
    }

    /// Nonblocking on the UI thread. A lagged event is an authoritative resync.
    pub(super) fn drain(&mut self) -> Option<&WorkingSet> {
        let update = {
            let mut state = self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let update = state.newest.take();
            state.wake_pending = false;
            update
        }?;
        if self.current.as_ref().is_some_and(|current| {
            update.event.revision < current.revision
                || (!update.lagged
                    && update.event.revision == current.revision
                    && current == update.event.working_set.as_ref())
        }) {
            return None;
        }
        self.resync_pending |= update.lagged;
        self.current = Some(*update.event.working_set);
        self.current.as_ref()
    }

    pub(super) fn take_resync(&mut self) -> bool {
        std::mem::take(&mut self.resync_pending)
    }

    pub(super) fn current(&self) -> Option<&WorkingSet> {
        self.current.as_ref()
    }

    pub(super) fn has_pending(&self) -> bool {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wake_pending
    }

    /// Signal cancellation on the UI thread; join only on the shutdown worker.
    pub(super) fn stop(&mut self) -> Option<JoinHandle<()>> {
        self.stop.store(true, Ordering::Release);
        self.bridge.take()
    }
}

impl Drop for TradingStore {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_trading_runtime::clock::ManualClock;
    use aeris_trading_runtime::{TradingEvent, TradingEventCause};

    #[test]
    fn idle_minute_produces_no_trading_work() {
        let clock = ManualClock::new(0);
        let mut store = TradingStore::default();
        let mut store_changes = 0;
        let mut trading_notifies = 0;
        let mut working_set_reads = 0;
        let mut history_reads = 0;
        for _ in 0..60 {
            clock.advance(Duration::from_secs(1));
            if store.has_pending() {
                working_set_reads += 1;
                if store.drain().is_some() {
                    store_changes += 1;
                    trading_notifies += 1;
                }
                if store.take_resync() {
                    history_reads += 1;
                }
            }
        }
        assert_eq!(store_changes, 0);
        assert_eq!(trading_notifies, 0);
        assert_eq!(working_set_reads, 0);
        assert_eq!(history_reads, 0);
    }

    fn update(revision: u64, lagged: bool) -> SubscriptionUpdate {
        SubscriptionUpdate {
            lagged,
            event: TradingEvent {
                revision,
                cause: if lagged {
                    TradingEventCause::Resync
                } else {
                    TradingEventCause::Command
                },
                working_set: Box::new(WorkingSet {
                    revision,
                    accounts: vec![],
                    instruments: vec![],
                    orders: vec![],
                    order_events: vec![],
                    order_feed_states: vec![],
                    order_groups: vec![],
                    positions: vec![],
                    fills: vec![],
                    round_trips: vec![],
                    connected_broker_accounts: std::collections::BTreeSet::default(),
                    position_pnl: vec![],
                    account_pnl: vec![],
                    risk_profiles: vec![],
                    risk_meters: vec![],
                    risk_locks: vec![],
                    session_plans: vec![],
                    managed_brackets: vec![],
                    planned_legs: vec![],
                    protective_orders: vec![],
                    market_observation_error: None,
                    recovery_notices: vec![],
                    venue_notices: vec![],
                }),
            },
        }
    }

    #[test]
    fn out_of_order_revisions_keep_newest_and_lagged_replaces() {
        let mut store = TradingStore::default();
        {
            let mut slot = store.slot.lock().expect("slot");
            assert!(slot.offer(update(9, false)));
            assert!(!slot.offer(update(8, false)));
        }
        assert_eq!(store.drain().expect("nine").revision, 9);
        {
            let mut slot = store.slot.lock().expect("slot");
            assert!(slot.offer(update(8, false)));
        }
        assert!(store.drain().is_none());
        {
            let mut slot = store.slot.lock().expect("slot");
            let mut resync = update(9, true);
            resync.event.working_set.market_observation_error = Some("resynchronized".into());
            assert!(slot.offer(resync));
        }
        assert_eq!(
            store
                .drain()
                .expect("lagged replacement")
                .market_observation_error
                .as_deref(),
            Some("resynchronized")
        );
        assert_eq!(store.current().expect("latest").revision, 9);
        assert!(
            store.take_resync(),
            "a lag requires a downstream history reload"
        );
        assert!(!store.take_resync());
    }

    #[test]
    fn same_revision_mark_updates_replace_the_slot_without_losing_lag() {
        let mut store = TradingStore::default();
        let mut first = update(4, true);
        first.event.working_set.market_observation_error = Some("old mark".into());
        let mut last = update(4, false);
        last.event.working_set.market_observation_error = Some("new mark".into());
        {
            let mut slot = store.slot.lock().expect("slot");
            assert!(slot.offer(first));
            assert!(!slot.offer(last));
            assert!(slot.newest.as_ref().expect("one slot").lagged);
        }
        assert_eq!(
            store
                .drain()
                .expect("new mark")
                .market_observation_error
                .as_deref(),
            Some("new mark")
        );
        assert!(store.take_resync());
    }

    #[test]
    fn bridge_has_one_pending_ui_wake_and_nonblocking_drain() {
        let mut store = TradingStore::default();
        {
            let mut slot = store.slot.lock().expect("slot");
            assert!(slot.offer(update(1, false)));
            for revision in 2..=1_024 {
                assert!(!slot.offer(update(revision, false)));
            }
            assert!(slot.wake_pending);
            assert_eq!(
                slot.newest.as_ref().expect("one slot").event.revision,
                1_024
            );
        }
        assert!(
            store.has_pending(),
            "a new working set schedules a UI frame"
        );
        assert_eq!(store.drain().expect("latest").revision, 1_024);
        assert!(!store.has_pending());
        assert!(!store.slot.lock().expect("slot").wake_pending);
        assert!(store.drain().is_none()); // No recv, timeout or owner read on the UI path.
        assert!(store.slot.lock().expect("slot").offer(update(1_025, false)));
    }

    #[test]
    fn bridge_shutdown_drops_subscription_and_joins_within_receive_bound() {
        let root = std::env::temp_dir().join(format!(
            "aeris-trading-bridge-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&root).expect("unique test root");
        let service = aeris_trading_runtime::TradingService::start(
            aeris_trading_runtime::TradingServiceConfig {
                database_path: root.join("trading.sqlite3"),
                retention: aeris_trading_runtime::TradingRetention::default(),
                clock: Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
                execution: aeris_trading_runtime::ExecutionPolicy::default(),
            },
        )
        .expect("trading owner");
        let subscription = service.subscribe().expect("one subscription");
        let (woke, received) = std::sync::mpsc::sync_channel(1);
        let mut store = TradingStore::default();
        store
            .start(
                subscription,
                Arc::new(move || {
                    let _ = woke.try_send(());
                }),
            )
            .expect("named bridge");
        received
            .recv_timeout(Duration::from_secs(2))
            .expect("initial publication woke UI");
        assert!(store.drain().is_some());
        let started = std::time::Instant::now();
        store
            .stop()
            .expect("bridge join")
            .join()
            .expect("bridge exited");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown is bounded by one receive timeout"
        );
        assert!(store.drain().is_none());
        service
            .shutdown(Duration::from_secs(2))
            .expect("owner shutdown");
        std::fs::remove_dir_all(root).expect("remove test-owned root");
    }
}
