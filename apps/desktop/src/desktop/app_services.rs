//! The runtime handles the terminal holds beside its lifecycle: the trading view store, the
//! factory that opens market demand for new panes, the update client with its restart phase,
//! and the schedule that applies economic-event risk rules to the trading owner. The terminal
//! runs the work; these only say what is held and what is due.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui::Entity;

use super::engine_market_worker::WorkspaceMarketFactory;
use super::trading_store::TradingStore;
use super::update::DesktopUpdater;

/// An update restart waits for the workspace and chart preferences to be durable first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpdateRestart {
    Idle,
    AwaitingDurability,
}

pub(super) struct AppServices {
    trading_store: Entity<TradingStore>,
    /// Absent in a window that cannot open market demand for new panes.
    market_factory: Option<WorkspaceMarketFactory>,
    updater: Option<DesktopUpdater>,
    update_restart: UpdateRestart,
    event_risk: EconomicEventRiskSchedule,
}

impl AppServices {
    pub(super) fn new(
        trading_store: Entity<TradingStore>,
        market_factory: Option<WorkspaceMarketFactory>,
        updater: Option<DesktopUpdater>,
        now: Instant,
    ) -> Self {
        Self {
            trading_store,
            market_factory,
            updater,
            update_restart: UpdateRestart::Idle,
            event_risk: EconomicEventRiskSchedule::new(now),
        }
    }

    pub(super) const fn trading_store(&self) -> &Entity<TradingStore> {
        &self.trading_store
    }

    pub(super) const fn market_factory(&self) -> Option<&WorkspaceMarketFactory> {
        self.market_factory.as_ref()
    }

    pub(super) const fn updater(&self) -> Option<&DesktopUpdater> {
        self.updater.as_ref()
    }

    pub(super) const fn updater_mut(&mut self) -> Option<&mut DesktopUpdater> {
        self.updater.as_mut()
    }

    #[cfg(test)]
    pub(super) fn install_updater_for_test(&mut self, updater: DesktopUpdater) {
        self.updater = Some(updater);
    }

    pub(super) fn update_restart_pending(&self) -> bool {
        self.update_restart == UpdateRestart::AwaitingDurability
    }

    pub(super) fn await_update_restart_durability(&mut self) {
        self.update_restart = UpdateRestart::AwaitingDurability;
    }

    /// The restart committed or was cancelled.
    pub(super) fn end_update_restart(&mut self) {
        self.update_restart = UpdateRestart::Idle;
    }

    pub(super) const fn event_risk(&self) -> &EconomicEventRiskSchedule {
        &self.event_risk
    }

    pub(super) const fn event_risk_mut(&mut self) -> &mut EconomicEventRiskSchedule {
        &mut self.event_risk
    }
}

/// When the economic-event risk rules are next checked, and when each event was last applied,
/// so an event is reapplied only after the redispatch interval and forgotten after its grace.
pub(super) struct EconomicEventRiskSchedule {
    next_check: Instant,
    dispatched: BTreeMap<String, i64>,
}

impl EconomicEventRiskSchedule {
    const fn new(now: Instant) -> Self {
        Self {
            next_check: now,
            dispatched: BTreeMap::new(),
        }
    }

    /// Claims the check due at `now`, scheduling the next one `interval` later. Reports false
    /// before the scheduled check.
    pub(super) fn claim_check(&mut self, now: Instant, interval: Duration) -> bool {
        if now < self.next_check {
            return false;
        }
        self.next_check = now + interval;
        true
    }

    /// Forgets events last applied more than `grace_nanos` before `observed_unix_nanos`.
    pub(super) fn expire(&mut self, observed_unix_nanos: i64, grace_nanos: i64) {
        self.dispatched.retain(|_, scheduled| {
            scheduled
                .checked_add(grace_nanos)
                .is_some_and(|expires| expires >= observed_unix_nanos)
        });
    }

    pub(super) const fn dispatched(&self) -> &BTreeMap<String, i64> {
        &self.dispatched
    }

    pub(super) fn record(&mut self, event: String, observed_unix_nanos: i64) {
        self.dispatched.insert(event, observed_unix_nanos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_risk_checks_run_once_per_interval() {
        let start = Instant::now();
        let interval = Duration::from_secs(5);
        let mut schedule = EconomicEventRiskSchedule::new(start);
        assert!(schedule.claim_check(start, interval));
        assert!(!schedule.claim_check(start + Duration::from_secs(4), interval));
        assert!(schedule.claim_check(start + interval, interval));
        assert!(!schedule.claim_check(start + interval, interval));
    }

    #[test]
    fn applied_events_are_forgotten_after_their_grace() {
        let mut schedule = EconomicEventRiskSchedule::new(Instant::now());
        schedule.record("payrolls".to_string(), 100);
        schedule.record("rates".to_string(), 200);
        schedule.expire(250, 50);
        assert_eq!(
            schedule.dispatched().keys().collect::<Vec<_>>(),
            ["rates"],
            "an event applied at the grace boundary is kept"
        );
        schedule.record("unbounded".to_string(), i64::MAX);
        schedule.expire(i64::MAX, 1);
        assert!(
            schedule.dispatched().is_empty(),
            "an overflowing expiry is forgotten"
        );
    }
}
