//! The desktop's one subscription-backed trading view.
//!
//! Only the bridge waits on the owner. A single replaceable slot and a single
//! outstanding wake bound work even when the UI is temporarily suspended.

use aeris_instruments::InstrumentId;
use aeris_trading::{Fill, Order, OrderEvent, OrderId, TradingAccountId};
use aeris_trading_runtime::{SubscriptionUpdate, TradingSubscription, WorkingSet};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(test)]
#[derive(Default)]
pub(super) struct TradingWorkCounts {
    pub(super) received_sets: std::sync::atomic::AtomicUsize,
    pub(super) applied_sets: std::sync::atomic::AtomicUsize,
    pub(super) trading_wakes: std::sync::atomic::AtomicUsize,
    pub(super) pane_notifies: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
struct Slot {
    newest: Option<SubscriptionUpdate>,
    wake_pending: bool,
}

/// Index offsets, never copies of owner facts. Rebuilt once when a publication is adopted.
#[derive(Default)]
struct TradingIndexes {
    orders: BTreeMap<(TradingAccountId, InstrumentId), Vec<usize>>,
    fills: BTreeMap<OrderId, Vec<usize>>,
    events: BTreeMap<OrderId, Vec<usize>>,
}

fn account_rows_changed<T: PartialEq>(
    previous: &[T],
    next: &[T],
    account: &TradingAccountId,
    owner: impl Fn(&T) -> &TradingAccountId,
) -> bool {
    previous
        .iter()
        .filter(|row| owner(row) == account)
        .ne(next.iter().filter(|row| owner(row) == account))
}

fn published_accounts(set: &WorkingSet) -> BTreeSet<TradingAccountId> {
    let mut ids: BTreeSet<_> = set.accounts.iter().map(|row| row.id.clone()).collect();
    ids.extend(set.account_pnl.iter().map(|row| row.account_id.clone()));
    ids.extend(set.risk_profiles.iter().map(|row| row.account_id.clone()));
    ids.extend(set.risk_meters.iter().map(|row| row.account_id.clone()));
    ids.extend(set.risk_locks.iter().map(|row| row.account_id.clone()));
    ids.extend(set.session_plans.iter().map(|row| row.account_id.clone()));
    ids.extend(set.connected_broker_accounts.iter().cloned());
    ids
}

fn published_selections(
    set: &WorkingSet,
    indexes: &TradingIndexes,
) -> BTreeSet<(TradingAccountId, InstrumentId)> {
    let mut keys: BTreeSet<_> = indexes.orders.keys().cloned().collect();
    keys.extend(
        set.positions
            .iter()
            .map(|row| (row.account_id.clone(), row.instrument_id.clone())),
    );
    keys.extend(set.position_pnl.iter().map(|row| {
        (
            row.position.account_id.clone(),
            row.position.instrument_id.clone(),
        )
    }));
    keys.extend(
        set.planned_legs
            .iter()
            .map(|row| (row.account_id.clone(), row.instrument_id.clone())),
    );
    keys.extend(
        set.fills
            .iter()
            .map(|row| (row.account_id.clone(), row.instrument_id.clone())),
    );
    keys
}

impl TradingIndexes {
    fn build(set: &WorkingSet) -> Self {
        let mut indexes = Self::default();
        for (index, order) in set.orders.iter().enumerate() {
            indexes
                .orders
                .entry((order.account_id.clone(), order.instrument_id.clone()))
                .or_default()
                .push(index);
        }
        for (index, fill) in set.fills.iter().enumerate() {
            indexes
                .fills
                .entry(fill.order_id.clone())
                .or_default()
                .push(index);
        }
        for (index, event) in set.order_events.iter().enumerate() {
            indexes
                .events
                .entry(event.order_id.clone())
                .or_default()
                .push(index);
        }
        indexes
    }
}

/// Borrowed account/instrument slice of one adopted owner publication.
pub(super) struct PaneTradingView<'a> {
    set: &'a WorkingSet,
    indexes: &'a TradingIndexes,
    selection: (TradingAccountId, InstrumentId),
}

impl PaneTradingView<'_> {
    pub(super) fn working_set(&self) -> &WorkingSet {
        self.set
    }

    pub(super) fn account(&self) -> Option<&aeris_trading::TradingAccount> {
        self.set
            .accounts
            .iter()
            .find(|account| account.id == self.selection.0)
    }

    pub(super) fn positions(&self) -> impl Iterator<Item = &aeris_trading_runtime::PositionPnl> {
        self.set.position_pnl.iter().filter(|position| {
            position.position.account_id == self.selection.0
                && position.position.instrument_id == self.selection.1
        })
    }

    pub(super) fn position_protection(&self) -> aeris_trading_runtime::PositionProtection {
        self.set
            .position_protection(&self.selection.0, &self.selection.1)
    }

    pub(super) fn planned_legs(&self) -> impl Iterator<Item = &aeris_trading_runtime::PlannedLeg> {
        self.set.planned_legs.iter().filter(|leg| {
            leg.account_id == self.selection.0 && leg.instrument_id == self.selection.1
        })
    }

    fn brackets(&self) -> impl Iterator<Item = &aeris_trading_runtime::BracketView> {
        self.set.managed_brackets.iter().filter(|bracket| {
            self.orders().any(|order| {
                bracket.entry_client_order_id == order.client_order_id
                    || bracket.stop_client_order_id.as_ref() == Some(&order.client_order_id)
                    || bracket
                        .target_client_order_ids
                        .contains(&order.client_order_id)
            })
        })
    }

    pub(super) fn orders(&self) -> impl Iterator<Item = &Order> {
        self.indexes
            .orders
            .get(&self.selection)
            .into_iter()
            .flatten()
            .map(|&index| &self.set.orders[index])
    }

    pub(super) fn fills(&self, order: &OrderId) -> impl Iterator<Item = &Fill> {
        self.indexes
            .fills
            .get(order)
            .into_iter()
            .flatten()
            .map(|&index| &self.set.fills[index])
    }

    pub(super) fn events(&self, order: &OrderId) -> impl Iterator<Item = &OrderEvent> {
        self.indexes
            .events
            .get(order)
            .into_iter()
            .flatten()
            .map(|&index| &self.set.order_events[index])
    }

    fn changed_from(&self, previous: &Self) -> bool {
        if self.account() != previous.account()
            || !self.positions().eq(previous.positions())
            || !self.planned_legs().eq(previous.planned_legs())
            || !self.brackets().eq(previous.brackets())
            || self.position_protection() != previous.position_protection()
            || self
                .set
                .positions
                .iter()
                .filter(|position| {
                    position.account_id == self.selection.0
                        && position.instrument_id == self.selection.1
                })
                .ne(previous.set.positions.iter().filter(|position| {
                    position.account_id == self.selection.0
                        && position.instrument_id == self.selection.1
                }))
            || self
                .set
                .fills
                .iter()
                .filter(|fill| {
                    fill.account_id == self.selection.0 && fill.instrument_id == self.selection.1
                })
                .ne(previous.set.fills.iter().filter(|fill| {
                    fill.account_id == self.selection.0 && fill.instrument_id == self.selection.1
                }))
        {
            return true;
        }
        if !self.orders().eq(previous.orders()) {
            return true;
        }
        self.orders().chain(previous.orders()).any(|order| {
            !self.fills(&order.id).eq(previous.fills(&order.id))
                || !self.events(&order.id).eq(previous.events(&order.id))
        })
    }
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
    indexes: TradingIndexes,
    changed_selections: BTreeSet<(TradingAccountId, InstrumentId)>,
    changed_accounts: BTreeSet<TradingAccountId>,
    unselected_accounts_changed: bool,
    all_panes_changed: bool,
    resync_pending: bool,
    slot: Arc<Mutex<Slot>>,
    stop: Arc<AtomicBool>,
    bridge: Option<JoinHandle<()>>,
    #[cfg(test)]
    counts: Option<Arc<TradingWorkCounts>>,
}

impl TradingStore {
    pub(super) fn start(
        &mut self,
        subscription: TradingSubscription,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        self.start_bridge(
            subscription,
            wake,
            Duration::from_secs(1),
            #[cfg(test)]
            None,
        )
    }

    fn start_bridge(
        &mut self,
        subscription: TradingSubscription,
        wake: Arc<dyn Fn() + Send + Sync>,
        receive_timeout: Duration,
        #[cfg(test)] timeout_tick: Option<std::sync::mpsc::Sender<()>>,
    ) -> Result<(), String> {
        if self.stop.load(Ordering::Acquire) {
            return Err("trading bridge is already stopping".to_string());
        }
        let slot = Arc::clone(&self.slot);
        let stop = Arc::clone(&self.stop);
        #[cfg(test)]
        let counts = self.counts.clone();
        let bridge = std::thread::Builder::new()
            .name("aeris-trading-desktop-bridge".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if let Some(update) = subscription.recv_timeout(receive_timeout) {
                        #[cfg(test)]
                        if let Some(counts) = &counts {
                            counts.received_sets.fetch_add(1, Ordering::Relaxed);
                        }
                        let notify = {
                            let mut state = slot
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            state.offer(update)
                        };
                        if notify {
                            #[cfg(test)]
                            if let Some(counts) = &counts {
                                counts.trading_wakes.fetch_add(1, Ordering::Relaxed);
                            }
                            wake();
                        }
                    } else {
                        #[cfg(test)]
                        if let Some(tick) = &timeout_tick {
                            let _ = tick.send(());
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
        let next = *update.event.working_set;
        let next_indexes = TradingIndexes::build(&next);
        self.changed_selections.clear();
        self.changed_accounts.clear();
        self.unselected_accounts_changed = self.current.as_ref().is_none_or(|previous| {
            previous.accounts.first().map(|account| &account.id)
                != next.accounts.first().map(|account| &account.id)
        });
        self.all_panes_changed = self.current.as_ref().is_none_or(|previous| {
            previous.market_observation_error != next.market_observation_error
                || previous.recovery_notices != next.recovery_notices
                || previous.venue_notices != next.venue_notices
        });
        if !self.all_panes_changed
            && let Some(previous) = self.current.as_ref()
        {
            // Account-wide facts affect every instrument of that account; rows tied to
            // an instrument only affect the matching pane. Iterate published keys,
            // never the Cartesian product of every account and instrument.
            let previous_accounts = published_accounts(previous);
            let next_accounts = published_accounts(&next);
            for id in previous_accounts.union(&next_accounts) {
                if account_rows_changed(&previous.accounts, &next.accounts, id, |row| &row.id)
                    || account_rows_changed(&previous.account_pnl, &next.account_pnl, id, |row| {
                        &row.account_id
                    })
                    || previous.connected_broker_accounts.contains(id)
                        != next.connected_broker_accounts.contains(id)
                    || account_rows_changed(
                        &previous.risk_profiles,
                        &next.risk_profiles,
                        id,
                        |row| &row.account_id,
                    )
                    || account_rows_changed(&previous.risk_meters, &next.risk_meters, id, |row| {
                        &row.account_id
                    })
                    || account_rows_changed(&previous.risk_locks, &next.risk_locks, id, |row| {
                        &row.account_id
                    })
                    || account_rows_changed(
                        &previous.session_plans,
                        &next.session_plans,
                        id,
                        |row| &row.account_id,
                    )
                {
                    self.changed_accounts.insert(id.clone());
                }
            }
            let old_selections = published_selections(previous, &self.indexes);
            let next_selections = published_selections(&next, &next_indexes);
            for selection in old_selections.union(&next_selections) {
                let old_view = PaneTradingView {
                    set: previous,
                    indexes: &self.indexes,
                    selection: selection.clone(),
                };
                let new_view = PaneTradingView {
                    set: &next,
                    indexes: &next_indexes,
                    selection: selection.clone(),
                };
                if new_view.changed_from(&old_view) {
                    self.changed_selections.insert(selection.clone());
                }
            }
        }
        self.current = Some(next);
        self.indexes = next_indexes;
        #[cfg(test)]
        if let Some(counts) = &self.counts {
            counts.applied_sets.fetch_add(1, Ordering::Relaxed);
        }
        self.current.as_ref()
    }

    /// Counts the actual pane-update path in desktop tests.
    #[cfg(test)]
    pub(super) fn note_pane_notify(&self) {
        if let Some(counts) = &self.counts {
            counts.pane_notifies.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn take_resync(&mut self) -> bool {
        std::mem::take(&mut self.resync_pending)
    }

    pub(super) fn current(&self) -> Option<&WorkingSet> {
        self.current.as_ref()
    }

    pub(super) fn pane_view(
        &self,
        account: &TradingAccountId,
        instrument: &InstrumentId,
    ) -> PaneTradingView<'_> {
        PaneTradingView {
            set: self
                .current
                .as_ref()
                .expect("pane view requires a publication"),
            indexes: &self.indexes,
            selection: (account.clone(), instrument.clone()),
        }
    }

    pub(super) fn pane_changed(
        &self,
        account: Option<&TradingAccountId>,
        instrument: Option<&InstrumentId>,
    ) -> bool {
        self.all_panes_changed
            || match (account, instrument) {
                (Some(account), Some(instrument)) => {
                    self.changed_accounts.contains(account)
                        || self
                            .changed_selections
                            .contains(&(account.clone(), instrument.clone()))
                }
                (Some(account), None) => self.changed_accounts.contains(account),
                // An unselected account or instrument has no order-scoped view.
                (None, _) => self.unselected_accounts_changed,
            }
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
        let root = std::env::temp_dir().join(format!(
            "aeris-trading-idle-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&root).expect("unique test root");
        let clock = Arc::new(ManualClock::new(0));
        let service = aeris_trading_runtime::TradingService::start(
            aeris_trading_runtime::TradingServiceConfig {
                database_path: root.join("trading.sqlite3"),
                retention: aeris_trading_runtime::TradingRetention::default(),
                clock: clock.clone(),
                execution: aeris_trading_runtime::ExecutionPolicy::default(),
            },
        )
        .expect("trading owner");
        let counts = Arc::new(TradingWorkCounts::default());
        let mut store = TradingStore::default();
        store.counts = Some(counts.clone());
        let (wake, woken) = std::sync::mpsc::channel();
        let (tick, timeout) = std::sync::mpsc::channel();
        store
            .start_bridge(
                service.subscribe().expect("subscribe"),
                Arc::new(move || {
                    let _ = wake.send(());
                }),
                Duration::from_millis(1),
                Some(tick),
            )
            .expect("start bridge");
        woken
            .recv_timeout(Duration::from_secs(2))
            .expect("initial snapshot wake");
        assert!(store.drain().is_some(), "initial snapshot applied");
        assert_eq!(counts.received_sets.load(Ordering::Relaxed), 1);
        assert_eq!(counts.applied_sets.load(Ordering::Relaxed), 1);
        assert_eq!(counts.trading_wakes.load(Ordering::Relaxed), 1);
        let initial = store.current().expect("initial working set").clone();
        assert_eq!(service.history_read_count(), 0);
        for _ in 0..60 {
            timeout
                .recv_timeout(Duration::from_secs(2))
                .expect("one real bridge receive timed out");
            clock.advance(Duration::from_secs(1));
        }
        assert_eq!(counts.received_sets.load(Ordering::Relaxed), 1);
        assert_eq!(counts.applied_sets.load(Ordering::Relaxed), 1);
        assert_eq!(counts.trading_wakes.load(Ordering::Relaxed), 1);
        assert_eq!(counts.pane_notifies.load(Ordering::Relaxed), 0);
        assert_eq!(service.history_read_count(), 0);
        assert!(!store.has_pending());
        assert!(store.drain().is_none());
        assert_eq!(store.current(), Some(&initial));
        // Prove the history seam counts the real service call, not a modeled
        // resync flag or a test-local variable.
        service
            .history(aeris_trading_runtime::HistoryQuery::default())
            .expect("empty history page");
        assert_eq!(service.history_read_count(), 1);
        store.stop().expect("bridge").join().expect("bridge joined");
        service.shutdown(Duration::from_secs(2)).expect("shutdown");
        std::fs::remove_dir_all(root).expect("remove test-owned root");
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

    fn indexed_fixture() -> (
        SubscriptionUpdate,
        aeris_trading::TradingAccountId,
        aeris_trading::TradingAccountId,
        aeris_instruments::InstrumentId,
    ) {
        use aeris_trading::{
            ClientOrderId, Fill, FillId, FixedPoint, Order, OrderEvent, OrderEventId,
            OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, TimeInForce,
            TradingAccountId, TradingProvenance,
        };
        let account = TradingAccountId::try_new("sim-a").expect("account");
        let other = TradingAccountId::try_new("sim-b").expect("account");
        let instrument =
            aeris_instruments::InstrumentId::try_new("instrument:fixture:A").expect("instrument");
        let point = |value| FixedPoint::try_new(value, 0).expect("point");
        let provenance = TradingProvenance {
            venue_id: "aeris-sim".into(),
            provider_id: "fixture".into(),
            session_generation: 1,
            source_sequence: 1,
            observed_unix_nanos: 1,
        };
        let make_order = |id: &str, account_id: TradingAccountId| Order {
            id: OrderId::try_new(id).expect("order"),
            client_order_id: ClientOrderId::try_new(id).expect("client"),
            account_id,
            instrument_id: instrument.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Day,
            quantity: point(1),
            filled_quantity: point(0),
            limit_price: Some(point(10)),
            stop_price: None,
            status: OrderStatus::Working,
            submitted_unix_nanos: 1,
            revision: 1,
            accepted_unix_nanos: 1,
            updated_unix_nanos: 1,
            eligible_after_unix_nanos: 1,
            triggered_unix_nanos: None,
            expires_at_unix_nanos: None,
            terminal_unix_nanos: None,
            provenance: provenance.clone(),
        };
        let mut publication = update(3, false);
        publication.event.working_set.orders = vec![
            make_order("first", account.clone()),
            make_order("elsewhere", other.clone()),
        ];
        publication.event.working_set.fills = ["first", "elsewhere"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| Fill {
                id: FillId::try_new(format!("fill-{index}")).expect("fill"),
                order_id: OrderId::try_new(id).expect("order"),
                account_id: if index == 0 {
                    account.clone()
                } else {
                    other.clone()
                },
                instrument_id: instrument.clone(),
                side: OrderSide::Buy,
                price: point(10),
                quantity: point(1),
                execution_unix_nanos: 1,
                provenance: provenance.clone(),
            })
            .collect();
        publication.event.working_set.order_events = ["first", "elsewhere"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| OrderEvent {
                id: OrderEventId::try_new(format!("event-{index}")).expect("event"),
                order_id: OrderId::try_new(id).expect("order"),
                sequence: index as u64 + 1,
                kind: OrderEventKind::Accepted,
                reason: None,
                event_unix_nanos: 1,
                detail: None,
                provenance: provenance.clone(),
            })
            .collect();
        (publication, account, other, instrument)
    }

    #[test]
    fn pane_view_indexes_orders_fills_and_events_by_owner_and_order() {
        let (publication, account, other, instrument) = indexed_fixture();
        let mut other_account_update = publication.clone();
        other_account_update.event.revision = 4;
        other_account_update.event.working_set.revision = 4;
        other_account_update.event.working_set.orders[1].filled_quantity =
            aeris_trading::FixedPoint::try_new(1, 0).expect("point");
        let mut store = TradingStore::default();
        assert!(store.slot.lock().expect("slot").offer(publication));
        assert_eq!(store.drain().expect("revision").revision, 3);
        let view = store.pane_view(&account, &instrument);
        assert_eq!(
            view.orders()
                .map(|order| order.id.as_str())
                .collect::<Vec<_>>(),
            ["first"]
        );
        assert_eq!(
            view.fills(&view.orders().next().expect("order").id).count(),
            1
        );
        assert_eq!(
            view.events(&view.orders().next().expect("order").id)
                .count(),
            1
        );
        assert_eq!(store.pane_view(&other, &instrument).orders().count(), 1);
        assert!(store.pane_changed(Some(&account), Some(&instrument)));
        assert_eq!(
            store
                .pane_view(
                    &account,
                    &aeris_instruments::InstrumentId::try_new("instrument:fixture:B")
                        .expect("second instrument")
                )
                .orders()
                .count(),
            0
        );
        assert!(store.slot.lock().expect("slot").offer(other_account_update));
        store.drain().expect("other account changed");
        assert!(!store.pane_changed(Some(&account), Some(&instrument)));
        assert!(store.pane_changed(Some(&other), Some(&instrument)));
        assert!(store.slot.lock().expect("slot").offer(update(5, false)));
        store.drain().expect("empty next revision");
        assert_eq!(store.pane_view(&account, &instrument).orders().count(), 0);
    }

    #[test]
    fn ticket_hotkey_and_quantity_read_selected_store_publication() {
        let (mut publication, account, other, instrument) = indexed_fixture();
        publication
            .event
            .working_set
            .accounts
            .push(aeris_trading::TradingAccount {
                id: account.clone(),
                display_name: "Fixture practice".into(),
                environment: aeris_trading::AccountEnvironment::Simulated,
                venue_id: "aeris-sim".into(),
                broker_ref: None,
                currency: "USD".into(),
                currency_scale: 2,
                starting_equity: None,
            });
        publication
            .event
            .working_set
            .risk_locks
            .push(aeris_trading_runtime::RiskLock {
                account_id: account.clone(),
                kind: aeris_trading_runtime::RiskLockKind::Manual,
                reason: "manual test lock".into(),
                locked_at_unix_nanos: 1,
                profile_id: None,
                profile_version: None,
            });
        let mut store = TradingStore::default();
        assert!(store.slot.lock().expect("slot").offer(publication));
        store.drain().expect("published");
        let set = store.current().expect("working set");
        let (selected_account, position, working_orders) =
            super::super::order_ticket::ticket_scope(&store, Some(&account), Some(&instrument));
        let selected_account = selected_account.expect("selected account");
        assert!(position.is_none());
        assert_eq!(selected_account.display_name, "Fixture practice");
        assert_eq!(
            super::super::order_ticket::TicketBroker::of(selected_account)
                .map(super::super::order_ticket::TicketBroker::label),
            Some("Practice")
        );
        assert_eq!(
            super::super::order_ticket::ticket_status(None, working_orders),
            "Flat · 1 working order"
        );
        assert_eq!(store.pane_view(&other, &instrument).orders().count(), 1);
        assert_eq!(
            super::super::order_ticket::ticket_scope(&store, Some(&other), Some(&instrument)).2,
            0,
            "an account without a registered ticket is not selected"
        );
        let reason = super::super::selected_account_lock_reason(super::super::PaneLockView {
            selected_account: Some(&account),
            locks: &set.risk_locks,
        });
        assert_eq!(
            super::super::trading_hotkeys::risk_increasing_trade_refusal(
                &selected_account.display_name,
                reason
            ),
            Some("Fixture practice is locked: manual test lock".into())
        );
        assert_eq!(
            super::super::selected_account_lock_reason(super::super::PaneLockView {
                selected_account: Some(&other),
                locks: &set.risk_locks,
            }),
            None
        );
        assert_eq!(
            super::super::order_quantity::parse_order_quantity("999"),
            Some(999)
        );
        assert_eq!(
            super::super::order_quantity::parse_order_quantity("1000"),
            None
        );
    }

    #[test]
    fn account_position_and_risk_updates_notify_only_changed_pane() {
        let (first, account, other, instrument) = indexed_fixture();
        let other_instrument =
            InstrumentId::try_new("instrument:fixture:B").expect("other instrument");
        let mut store = TradingStore::default();
        assert!(store.slot.lock().expect("slot").offer(first));
        store.drain().expect("initial");

        let mut risk = store.current().expect("current").clone();
        risk.revision += 1;
        risk.risk_locks.push(aeris_trading_runtime::RiskLock {
            account_id: account.clone(),
            kind: aeris_trading_runtime::RiskLockKind::Manual,
            reason: "account A locked".into(),
            locked_at_unix_nanos: 1,
            profile_id: None,
            profile_version: None,
        });
        let revision = risk.revision;
        assert!(store.slot.lock().expect("slot").offer(SubscriptionUpdate {
            lagged: false,
            event: TradingEvent {
                revision,
                cause: TradingEventCause::Command,
                working_set: Box::new(risk),
            },
        }));
        store.drain().expect("risk change");
        assert!(store.pane_changed(Some(&account), Some(&instrument)));
        assert!(!store.pane_changed(Some(&other), Some(&other_instrument)));

        let mut position = store.current().expect("current").clone();
        position.revision += 1;
        let point = |units| aeris_trading::FixedPoint::try_new(units, 0).expect("point");
        position
            .position_pnl
            .push(aeris_trading_runtime::PositionPnl {
                position: aeris_trading::Position {
                    account_id: other.clone(),
                    instrument_id: other_instrument.clone(),
                    net_quantity: point(1),
                    average_entry_price: Some(point(10)),
                    realized_pnl: point(0),
                    unrealized_pnl: point(2),
                    last_fill_unix_nanos: 1,
                },
                realized_ticks: None,
                unrealized_ticks: None,
                point_value: None,
                currency_scale: 0,
            });
        let revision = position.revision;
        assert!(store.slot.lock().expect("slot").offer(SubscriptionUpdate {
            lagged: false,
            event: TradingEvent {
                revision,
                cause: TradingEventCause::Command,
                working_set: Box::new(position),
            },
        }));
        store.drain().expect("position change");
        assert!(!store.pane_changed(Some(&account), Some(&instrument)));
        assert!(store.pane_changed(Some(&other), Some(&other_instrument)));

        let mut balance = store.current().expect("current").clone();
        balance.revision += 1;
        balance.account_pnl.push(aeris_trading::AccountPnl {
            account_id: account.clone(),
            currency: "USD".into(),
            realized: point(0),
            unrealized: Some(point(2)),
            balance: Some(point(100)),
            equity: Some(point(102)),
        });
        let revision = balance.revision;
        assert!(store.slot.lock().expect("slot").offer(SubscriptionUpdate {
            lagged: false,
            event: TradingEvent {
                revision,
                cause: TradingEventCause::Command,
                working_set: Box::new(balance),
            },
        }));
        store.drain().expect("account change");
        assert!(store.pane_changed(Some(&account), Some(&instrument)));
        assert!(!store.pane_changed(Some(&other), Some(&other_instrument)));
    }

    #[test]
    fn chart_order_lines_read_only_selected_pane_view() {
        let (publication, account, other, instrument) = indexed_fixture();
        let mut store = TradingStore::default();
        assert!(store.slot.lock().expect("slot").offer(publication));
        store.drain().expect("working orders");
        let first = store.pane_view(&account, &instrument);
        let second = store.pane_view(&other, &instrument);
        let lines = crate::desktop::chart_working_orders_from_pane(
            &first,
            &account,
            instrument.as_str(),
            None,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].id.as_str(), "first");
        let lines = crate::desktop::chart_working_orders_from_pane(
            &second,
            &other,
            instrument.as_str(),
            None,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].id.as_str(), "elsewhere");
        assert_eq!(
            crate::desktop::chart_working_orders_from_pane(
                &store.pane_view(
                    &account,
                    &InstrumentId::try_new("instrument:fixture:B").expect("instrument"),
                ),
                &account,
                "instrument:fixture:B",
                None,
            )
            .len(),
            0
        );
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
