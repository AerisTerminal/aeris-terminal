//! Change-driven, bounded publication of the trading working set (M5.11).
//!
//! `TradingService::subscribe` registers one of at most [`MAXIMUM_SUBSCRIPTIONS`] subscribers.
//! Every revision-changing owner step publishes a [`TradingEvent`] carrying the exact current
//! [`WorkingSet`]; quote-only mark changes are coalesced to at most one publication per
//! [`MARK_PUBLICATION_INTERVAL`], with a trailing flush so the last publication always carries
//! the latest marks. A subscriber that never drains holds at most
//! [`SUBSCRIPTION_QUEUE_CAPACITY`] events; the oldest are dropped and the next receive reports
//! `lagged = true` with a freshly resynchronized working set.

use crate::service::Command;
use crate::{OrderGroup, PositionPnl, RecoveryNotice, RiskLock};
use aeris_trading::market::{FeedReason, FeedState};
use aeris_trading::{AccountPnl, Fill, Order, OrderId, Position, TradingAccount};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// At most this many subscriptions are open at once; a further `subscribe()` is refused.
pub const MAXIMUM_SUBSCRIPTIONS: usize = 8;
/// A lagging subscriber's queue holds at most this many events; the oldest are dropped.
pub const SUBSCRIPTION_QUEUE_CAPACITY: usize = 1_024;
/// Quote-only mark publications are coalesced to at most one per this interval.
pub const MARK_PUBLICATION_INTERVAL: Duration = Duration::from_millis(100);
/// The working set carries at most this many of the newest fills, never deeper history.
pub const WORKING_SET_FILL_LIMIT: usize = 500;

/// The current trading working set: open orders (including inactive held legs), active
/// order groups, open positions and the newest fills, with the account and risk projections
/// a client needs to render. It carries no order/fill history beyond
/// [`WORKING_SET_FILL_LIMIT`] recent fills; use `TradingService::history` for the past.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkingSet {
    /// The owner revision this working set reflects; equal revisions are identical views.
    pub revision: u64,
    pub accounts: Vec<TradingAccount>,
    pub orders: Vec<Order>,
    /// Feed readiness for each open order, derived from the owner's verified routes and feed.
    pub order_feed_states: Vec<OrderFeedStatus>,
    pub order_groups: Vec<OrderGroup>,
    pub positions: Vec<Position>,
    /// The newest fills, oldest first within the window.
    pub fills: Vec<Fill>,
    pub position_pnl: Vec<PositionPnl>,
    pub account_pnl: Vec<AccountPnl>,
    pub risk_locks: Vec<RiskLock>,
    /// Startup repairs the running process performed, published until they are acknowledged.
    pub recovery_notices: Vec<RecoveryNotice>,
}

/// A per-order feed projection; never an independent feed or route authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderFeedStatus {
    pub order_id: OrderId,
    pub state: FeedState,
}

impl OrderFeedStatus {
    /// Human-readable diagnostic for a migrated order that cannot yet execute.
    #[must_use]
    pub fn message(&self) -> Option<&'static str> {
        match self.state {
            FeedState::Unavailable {
                reason: FeedReason::RouteUnknown,
            } => Some("feed unavailable (route unknown)"),
            _ => None,
        }
    }
}

/// What changed to produce a publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TradingEventCause {
    /// The first event of a subscription: the complete working set at subscribe time.
    Snapshot,
    /// A client command changed the working set.
    Command,
    /// A market observation changed orders, fills or positions durably.
    Market,
    /// Quote-only marks changed unrealized P&L; coalesced to [`MARK_PUBLICATION_INTERVAL`].
    Mark,
    /// The owner timer changed the working set (expiry, rollover, retention).
    Timer,
    /// A lagging subscriber was resynchronized; carries the exact current working set.
    Resync,
}

/// One typed publication. Every event carries the exact full working set at its revision, so
/// applying events in any order converges on the newest revision.
#[derive(Clone, Debug, PartialEq)]
pub struct TradingEvent {
    pub revision: u64,
    pub cause: TradingEventCause,
    pub working_set: Box<WorkingSet>,
}

/// The outcome of receiving from a [`TradingSubscription`].
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionUpdate {
    /// True when the queue overflowed since the last receive; `event` is a fresh resync with
    /// the exact current working set.
    pub lagged: bool,
    pub event: TradingEvent,
}

/// The owner-side queue of one subscriber: bounded, drop-oldest, with a lag flag.
#[derive(Debug, Default)]
pub(crate) struct SubscriberQueue {
    events: VecDeque<TradingEvent>,
    overflowed: bool,
}

/// The shared slot the owner pushes to and the subscriber receives from.
#[derive(Debug, Default)]
pub(crate) struct SubscriberSlot {
    queue: Mutex<SubscriberQueue>,
    available: Condvar,
}

impl SubscriberSlot {
    pub(crate) fn push(&self, event: TradingEvent) {
        let mut queue = self.queue.lock().expect("subscriber queue");
        while queue.events.len() >= SUBSCRIPTION_QUEUE_CAPACITY {
            queue.events.pop_front();
            queue.overflowed = true;
        }
        queue.events.push_back(event);
        drop(queue);
        self.available.notify_one();
    }

    /// True once only the owner holds this slot; the subscriber handle was dropped without
    /// its unsubscribe command reaching the owner.
    pub(crate) fn is_orphaned(slot: &Arc<Self>) -> bool {
        Arc::strong_count(slot) == 1
    }
}

/// One change-driven view of the trading working set. Drop to unsubscribe.
///
/// Receives are non-blocking or time-bounded; the owner thread never blocks on a subscriber.
pub struct TradingSubscription {
    pub(crate) id: u64,
    pub(crate) slot: Arc<SubscriberSlot>,
    pub(crate) commands: SyncSender<Command>,
    pub(crate) stopping: Arc<AtomicBool>,
}

impl TradingSubscription {
    /// Returns the next update without waiting, or `None` when no publication is pending.
    ///
    /// # Panics
    /// Panics when the subscriber queue mutex is poisoned by a panicking publisher.
    #[must_use]
    pub fn try_recv(&self) -> Option<SubscriptionUpdate> {
        let mut queue = self.slot.queue.lock().expect("subscriber queue");
        if queue.events.is_empty() && !queue.overflowed {
            return None;
        }
        if std::mem::take(&mut queue.overflowed) {
            drop(queue);
            return self.resync();
        }
        let event = queue.events.pop_front()?;
        drop(queue);
        Some(SubscriptionUpdate {
            lagged: false,
            event,
        })
    }

    /// Waits up to `timeout` for the next update.
    ///
    /// # Panics
    /// Panics when the subscriber queue mutex is poisoned by a panicking publisher.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<SubscriptionUpdate> {
        let mut queue = self.slot.queue.lock().expect("subscriber queue");
        if queue.events.is_empty() && !queue.overflowed {
            let (locked, _) = self
                .slot
                .available
                .wait_timeout(queue, timeout)
                .expect("subscriber queue");
            queue = locked;
        }
        if std::mem::take(&mut queue.overflowed) {
            drop(queue);
            return self.resync();
        }
        let event = queue.events.pop_front()?;
        drop(queue);
        Some(SubscriptionUpdate {
            lagged: false,
            event,
        })
    }

    /// Fetches a fresh working set from the owner after the queue overflowed. When the owner
    /// is gone, the newest queued event stands in so the subscriber still converges.
    fn resync(&self) -> Option<SubscriptionUpdate> {
        if !self.stopping.load(Ordering::Acquire) {
            let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
            match self.commands.try_send(Command::WorkingSet(reply_tx)) {
                Ok(()) => {
                    if let Ok(Ok(working_set)) = reply_rx.recv_timeout(Duration::from_secs(5)) {
                        let revision = working_set.revision;
                        // Queued events at or below the resync revision are stale working sets;
                        // only a step that landed after the resync is still worth receiving.
                        {
                            let mut queue = self.slot.queue.lock().expect("subscriber queue");
                            queue.events.retain(|event| event.revision > revision);
                            // The resync update itself carries the lag signal.
                            queue.overflowed = false;
                        }
                        return Some(SubscriptionUpdate {
                            lagged: true,
                            event: TradingEvent {
                                revision,
                                cause: TradingEventCause::Resync,
                                working_set: Box::new(working_set),
                            },
                        });
                    }
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
            }
        }
        let mut queue = self.slot.queue.lock().expect("subscriber queue");
        let event = queue.events.pop_back()?;
        queue.events.clear();
        drop(queue);
        Some(SubscriptionUpdate {
            lagged: true,
            event,
        })
    }
}

impl Drop for TradingSubscription {
    fn drop(&mut self) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        // Best effort: when the bounded command queue is full the owner prunes this slot as
        // orphaned on its next publication instead.
        let _ = self.commands.try_send(Command::Unsubscribe(self.id));
    }
}

impl std::fmt::Debug for TradingSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TradingSubscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Owner-side publication bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct PublicationState {
    /// The revision the last publication carried; the owner publishes when state moves past it.
    published_revision: u64,
    /// The mark stamp the last publication carried.
    published_mark_revision: u64,
    last_mark_publish: Option<Duration>,
    pending_marks: bool,
    mark_flush_at: Option<Duration>,
}

impl crate::TradingCore {
    /// Registers one subscriber, refusing beyond [`MAXIMUM_SUBSCRIPTIONS`]. The first queued
    /// event is the complete working set at subscribe time.
    pub(crate) fn subscribe(&mut self) -> Result<(u64, Arc<SubscriberSlot>), String> {
        self.prune_orphaned_subscribers();
        if self.subscribers.len() >= MAXIMUM_SUBSCRIPTIONS {
            return Err(format!(
                "trading subscription limit of {MAXIMUM_SUBSCRIPTIONS} reached"
            ));
        }
        let id = self.next_subscriber_id;
        self.next_subscriber_id += 1;
        let slot = Arc::new(SubscriberSlot::default());
        slot.push(self.event(TradingEventCause::Snapshot)?);
        self.subscribers.insert(id, Arc::clone(&slot));
        // The join is current: later changes compare against the published stamps.
        self.publication.published_revision = self.state.revision;
        self.publication.published_mark_revision = self.mark_revision;
        Ok((id, slot))
    }

    pub(crate) fn unsubscribe(&mut self, id: u64) {
        self.subscribers.remove(&id);
    }

    /// Publishes after one owner step when it changed the working set. Revision-changing steps
    /// publish immediately; quote-only mark changes coalesce to [`MARK_PUBLICATION_INTERVAL`]
    /// with a trailing flush, so an idle runtime publishes nothing.
    pub(crate) fn publish_changes(&mut self, cause: TradingEventCause) {
        self.prune_orphaned_subscribers();
        if self.subscribers.is_empty() {
            return;
        }
        let now = self.clock.monotonic();
        if self.state.revision != self.publication.published_revision {
            self.publish(cause);
            return;
        }
        if self.publication.pending_marks
            && self
                .publication
                .mark_flush_at
                .is_some_and(|deadline| now >= deadline)
        {
            self.publish(TradingEventCause::Mark);
            return;
        }
        if self.mark_revision == self.publication.published_mark_revision {
            return;
        }
        match self.publication.last_mark_publish {
            Some(last) if now < last + MARK_PUBLICATION_INTERVAL => {
                self.publication.pending_marks = true;
                self.publication.mark_flush_at = Some(last + MARK_PUBLICATION_INTERVAL);
            }
            _ => self.publish(TradingEventCause::Mark),
        }
    }

    /// The monotonic deadline at which coalesced marks must flush, when one is pending. The
    /// owner wait includes it so the trailing publication is not delayed by an idle queue.
    pub(crate) fn next_publication_flush(&self) -> Option<Duration> {
        if self.publication.pending_marks {
            self.publication.mark_flush_at
        } else {
            None
        }
    }

    /// The exact current working set for a lagged subscriber's resync.
    pub(crate) fn current_working_set(&self) -> Result<WorkingSet, String> {
        self.working_set()
    }

    fn publish(&mut self, cause: TradingEventCause) {
        let event = match self.event(cause) {
            Ok(event) => event,
            Err(error) => {
                aeris_observability::diagnostic!("trading publication failed: {error}");
                return;
            }
        };
        for slot in self.subscribers.values() {
            slot.push(event.clone());
        }
        let now = self.clock.monotonic();
        self.publication.published_revision = self.state.revision;
        self.publication.published_mark_revision = self.mark_revision;
        // Every publication carries the latest marks, so any of them restarts the window.
        self.publication.last_mark_publish = Some(now);
        self.publication.pending_marks = false;
        self.publication.mark_flush_at = None;
    }

    fn event(&self, cause: TradingEventCause) -> Result<TradingEvent, String> {
        Ok(TradingEvent {
            revision: self.state.revision,
            cause,
            working_set: Box::new(self.working_set()?),
        })
    }

    /// Builds the working set from the open-order index, active groups, open positions and the
    /// newest fills window only; its cost is independent of the durable history depth.
    fn working_set(&self) -> Result<WorkingSet, String> {
        let mut fills = self
            .state
            .fills
            .iter()
            .rev()
            .take(WORKING_SET_FILL_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        fills.reverse();
        let orders = self
            .state
            .orders
            .values()
            .filter(|order| order.status.is_open())
            .cloned()
            .collect::<Vec<_>>();
        let order_feed_states = orders
            .iter()
            .map(|order| OrderFeedStatus {
                order_id: order.id.clone(),
                state: self
                    .quote_book
                    .feed_state(&order.instrument_id, &self.state.market_routes),
            })
            .collect();
        Ok(WorkingSet {
            revision: self.state.revision,
            accounts: self.state.accounts.values().cloned().collect(),
            orders,
            order_feed_states,
            order_groups: self
                .state
                .order_groups
                .values()
                .filter(|group| {
                    matches!(
                        group.status,
                        crate::GroupStatus::Pending | crate::GroupStatus::Active
                    )
                })
                .cloned()
                .collect(),
            positions: self
                .state
                .positions
                .values()
                .filter(|position| position.net_quantity.units() != 0)
                .cloned()
                .collect(),
            fills,
            position_pnl: self.position_pnl()?,
            account_pnl: self.account_pnl()?,
            risk_locks: self.projected_risk_locks(),
            recovery_notices: self.recovery_notices.clone(),
        })
    }

    fn prune_orphaned_subscribers(&mut self) {
        self.subscribers
            .retain(|_, slot| !SubscriberSlot::is_orphaned(slot));
    }
}
