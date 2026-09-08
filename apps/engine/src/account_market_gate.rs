use std::{
    thread,
    time::{Duration, Instant},
};

use axiusflow_engine_protocol::{EngineLifetimeMode, HotSeries, ResourceMode};

use crate::{EngineShutdown, EngineState, MarketService};

const ACCOUNT_MARKET_GATE_POLL: Duration = Duration::from_millis(100);
const ACCOUNT_MARKET_GATE_MAX_RETRY: Duration = Duration::from_secs(2);

struct AccountMarketGateState {
    applied_mode: ResourceMode,
    pending_mode: ResourceMode,
    retry_at: Instant,
    retry_delay: Duration,
    startup_hot_set_restored: bool,
}

impl AccountMarketGateState {
    fn new(applied_mode: ResourceMode, now: Instant) -> Self {
        Self {
            applied_mode,
            pending_mode: applied_mode,
            retry_at: now,
            retry_delay: ACCOUNT_MARKET_GATE_POLL,
            startup_hot_set_restored: false,
        }
    }

    fn should_restore_startup_hot_set(&self, desired: ResourceMode) -> bool {
        desired != ResourceMode::OfflineSuspended && !self.startup_hot_set_restored
    }

    fn startup_hot_set_restored(&mut self) {
        self.startup_hot_set_restored = true;
    }

    fn should_apply(&mut self, desired: ResourceMode, now: Instant) -> bool {
        if desired != self.pending_mode {
            self.pending_mode = desired;
            self.retry_at = now;
            self.retry_delay = ACCOUNT_MARKET_GATE_POLL;
            return true;
        }
        if desired == self.applied_mode {
            return false;
        }
        now >= self.retry_at
    }

    fn acknowledge(&mut self, applied: ResourceMode, now: Instant) {
        self.record_applied(applied, now);
        self.pending_mode = applied;
    }

    fn record_applied(&mut self, applied: ResourceMode, now: Instant) {
        self.applied_mode = applied;
        self.retry_at = now;
        self.retry_delay = ACCOUNT_MARKET_GATE_POLL;
    }

    fn retry(&mut self, now: Instant) {
        self.retry_at = now + self.retry_delay;
        self.retry_delay = self
            .retry_delay
            .saturating_mul(2)
            .min(ACCOUNT_MARKET_GATE_MAX_RETRY);
    }
}

/// Starts the process-wide worker that keeps market resources aligned with
/// the authenticated account and persisted engine lifetime policy.
///
/// # Errors
/// Returns an error when the bounded background worker cannot be spawned.
pub fn start_account_market_gate(
    state: &EngineState,
    market: &MarketService,
    shutdown: &EngineShutdown,
    startup_hot_series: Vec<HotSeries>,
) -> Result<thread::JoinHandle<()>, String> {
    let state = state.clone();
    let market = market.clone();
    let shutdown = shutdown.clone();
    thread::Builder::new()
        .name("axiusflow-account-market-gate".to_string())
        .spawn(move || {
            let mut gate =
                AccountMarketGateState::new(ResourceMode::OfflineSuspended, Instant::now());
            while !shutdown.is_requested() {
                let desired = account_market_resource_mode(&state);
                let now = Instant::now();
                if gate.should_apply(desired, now) {
                    if gate.should_restore_startup_hot_set(desired) {
                        if let Err(error) = market.restore_hot_set(&startup_hot_series) {
                            eprintln!("Axiusflow account market hot-set restore degraded: {error}");
                            gate.retry(now);
                            thread::sleep(ACCOUNT_MARKET_GATE_POLL);
                            continue;
                        }
                        gate.startup_hot_set_restored();
                    }
                    if let Err(error) = market.set_resource_mode(desired) {
                        eprintln!("Axiusflow account market gate degraded: {error}");
                        gate.retry(now);
                    } else {
                        // The coordinator accepted this exact target, so keep
                        // the gate's applied-mode truth synchronized before
                        // re-reading account/lifecycle state. If the target
                        // changed while the command was in flight, the next
                        // loop must actively undo this now-stale mode.
                        gate.record_applied(desired, now);
                        let current = account_market_resource_mode(&state);
                        if current == desired {
                            state.set_resource_mode(desired);
                            gate.acknowledge(desired, now);
                        } else {
                            // Account/lifecycle state changed while the coordinator
                            // handled the old target. Retire it without publishing
                            // the stale mode; the newer target applies immediately.
                            gate.pending_mode = current;
                            gate.retry_at = now;
                            continue;
                        }
                    }
                }
                thread::sleep(ACCOUNT_MARKET_GATE_POLL);
            }
        })
        .map_err(|error| error.to_string())
}

fn account_market_resource_mode(state: &EngineState) -> ResourceMode {
    if state.account().is_authenticated() {
        lifetime_resource_mode(&state.workspace()).unwrap_or(ResourceMode::Interactive)
    } else {
        ResourceMode::OfflineSuspended
    }
}

fn lifetime_resource_mode(
    workspace: &axiusflow_engine_protocol::WorkspaceState,
) -> Result<ResourceMode, String> {
    match EngineLifetimeMode::try_from(workspace.lifetime_mode)
        .map_err(|_| "persisted engine lifetime mode is invalid".to_string())?
    {
        EngineLifetimeMode::KeepMarketsLive if workspace.markets_live_permitted => {
            Ok(ResourceMode::MarketsLive)
        }
        EngineLifetimeMode::ExitCompletely | EngineLifetimeMode::KeepEngineWarm => {
            Ok(ResourceMode::Warm)
        }
        EngineLifetimeMode::KeepMarketsLive => {
            Err("persisted markets-live mode lacks explicit permission".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_failed_targets_without_acknowledging_them() {
        let started = Instant::now();
        let mut gate = AccountMarketGateState::new(ResourceMode::OfflineSuspended, started);

        assert!(gate.should_apply(ResourceMode::Warm, started));
        gate.retry(started);
        assert_eq!(gate.applied_mode, ResourceMode::OfflineSuspended);
        assert!(!gate.should_apply(ResourceMode::Warm, started));
        assert!(gate.should_apply(ResourceMode::Warm, started + ACCOUNT_MARKET_GATE_POLL));

        gate.acknowledge(ResourceMode::Warm, started + ACCOUNT_MARKET_GATE_POLL);
        assert_eq!(gate.applied_mode, ResourceMode::Warm);
        assert!(!gate.should_apply(ResourceMode::Warm, started + Duration::from_secs(10)));
    }

    #[test]
    fn newer_target_retires_an_older_retry() {
        let started = Instant::now();
        let mut gate = AccountMarketGateState::new(ResourceMode::OfflineSuspended, started);

        assert!(gate.should_apply(ResourceMode::Warm, started));
        gate.retry(started);
        assert!(
            gate.should_apply(ResourceMode::OfflineSuspended, started),
            "a sign-out must undo a partially applied resume even though suspension was last acknowledged"
        );
        gate.acknowledge(ResourceMode::OfflineSuspended, started);

        assert_eq!(gate.applied_mode, ResourceMode::OfflineSuspended);
        assert!(!gate.should_apply(
            ResourceMode::OfflineSuspended,
            started + Duration::from_secs(10)
        ));
    }

    #[test]
    fn successful_stale_resume_is_followed_by_explicit_resuspension() {
        let started = Instant::now();
        let mut gate = AccountMarketGateState::new(ResourceMode::OfflineSuspended, started);

        assert!(gate.should_apply(ResourceMode::Warm, started));
        gate.record_applied(ResourceMode::Warm, started);
        gate.pending_mode = ResourceMode::OfflineSuspended;
        gate.retry_at = started;

        assert!(
            gate.should_apply(ResourceMode::OfflineSuspended, started),
            "a sign-out racing a successful resume must send an explicit suspension"
        );
    }

    #[test]
    fn startup_hot_set_restores_once_and_only_after_market_access_is_authorized() {
        let started = Instant::now();
        let mut gate = AccountMarketGateState::new(ResourceMode::OfflineSuspended, started);

        assert!(!gate.should_restore_startup_hot_set(ResourceMode::OfflineSuspended));
        assert!(gate.should_restore_startup_hot_set(ResourceMode::Warm));
        assert!(gate.should_restore_startup_hot_set(ResourceMode::MarketsLive));

        gate.startup_hot_set_restored();
        assert!(!gate.should_restore_startup_hot_set(ResourceMode::Warm));
        assert!(!gate.should_restore_startup_hot_set(ResourceMode::MarketsLive));
    }
}
