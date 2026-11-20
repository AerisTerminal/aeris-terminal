//! The Rithmic broker venue relay. One order-plant and one P&L-plant connection under the
//! saved login translate the trading owner's venue requests and report the broker's orders,
//! fills, positions and balances back. Demo logins (Rithmic Test and paper trading) and live
//! logins are routed alike; every reported account carries the environment its login serves.
//!
//! Each connection is one venue session generation attached to the trading owner. A lost
//! connection drops its request receiver, so the owner refuses orders until the next
//! session attaches and reconciles every account.

mod reconcile;
mod reports;
mod requests;
mod session;
mod translate;

use crate::{
    RITHMIC_APPLICATION_NAME, RITHMIC_LOGIN_VAULT_KEY, RITHMIC_VAULT_SERVICE, RithmicApplication,
    RithmicCredentialBytes, RithmicLoginRevision,
};
use aeris_observability::diagnostic;
use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use aeris_trading::venue::BrokerVenueLink;
use session::SessionEnd;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

/// Separates this relay's reconnect jitter from the market-data session's.
const BACKOFF_SEED: u64 = 0x5645_4e55_4552_4c59;
/// A replaced session waits at least this long before the next login.
const MINIMUM_REPLACEMENT_DELAY: Duration = Duration::from_secs(1);
const WAIT_SLICE: Duration = Duration::from_millis(100);
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_RECONNECT_DELAY: Duration = Duration::from_secs(30);
const HEALTHY_RESET_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub(super) struct VenueClock {
    monotonic: fn() -> Instant,
    wall_nanos: fn() -> Option<i64>,
}

impl VenueClock {
    pub(super) const SYSTEM: Self = Self {
        monotonic: Instant::now,
        wall_nanos: system_wall_nanos,
    };

    pub(super) fn monotonic(self) -> Instant {
        (self.monotonic)()
    }

    pub(super) fn wall_nanos(self) -> Option<i64> {
        (self.wall_nanos)()
    }
}

fn system_wall_nanos() -> Option<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_nanos()).ok()
}

struct ReconnectBackoff {
    attempts: u32,
    random: u64,
}

impl ReconnectBackoff {
    fn new(clock: VenueClock, seed: u64) -> Self {
        let random = clock.wall_nanos().unwrap_or_default().cast_unsigned() ^ seed;
        Self {
            attempts: 0,
            random,
        }
    }

    const fn reset(&mut self) {
        self.attempts = 0;
    }

    fn healthy_for(&mut self, since: Instant, clock: VenueClock) {
        if clock.monotonic().saturating_duration_since(since) >= HEALTHY_RESET_AFTER {
            self.reset();
        }
    }

    fn transient_delay(&mut self) -> Duration {
        let factor = 2_u32.saturating_pow(self.attempts);
        self.attempts = self.attempts.saturating_add(1);
        let ceiling = INITIAL_RECONNECT_DELAY
            .saturating_mul(factor)
            .min(MAXIMUM_RECONNECT_DELAY);
        let millis = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
        if millis == 0 {
            return Duration::ZERO;
        }
        // Equal jitter: half deterministic, half pseudo-random.
        self.random = self
            .random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let half = millis / 2;
        let span = millis - half;
        let jitter = if span == 0 { 0 } else { self.random % span };
        Duration::from_millis(half.saturating_add(jitter))
    }
}

/// Runs the relay until shutdown: one session at a time, reconnecting with the bounded
/// Rithmic backoff after a loss and waiting for a login change when the saved login cannot
/// be routed.
pub fn run(mut link: BrokerVenueLink, shutdown: &Arc<AtomicBool>, login: &RithmicLoginRevision) {
    let clock = VenueClock::SYSTEM;
    let mut backoff = ReconnectBackoff::new(clock, BACKOFF_SEED);
    let mut reported_idle: Option<String> = None;
    while !shutdown.load(Ordering::Acquire) {
        let revision = login.current();
        let login_changed = || login.current() != revision;
        let started = clock.monotonic();
        match session::run(&mut link, shutdown, clock, &login_changed) {
            SessionEnd::Shutdown => return,
            SessionEnd::Idle(reason) => {
                if reported_idle.as_deref() != Some(reason.as_str()) {
                    diagnostic!("Aeris broker venue is idle: {reason}");
                    reported_idle = Some(reason);
                }
                backoff.reset();
                wait(shutdown, clock, None, &login_changed);
            }
            SessionEnd::LoginChanged => {
                reported_idle = None;
                backoff.reset();
                wait(shutdown, clock, Some(MINIMUM_REPLACEMENT_DELAY), &|| false);
            }
            SessionEnd::Lost(reason) => {
                reported_idle = None;
                diagnostic!("Aeris broker venue lost its Rithmic session: {reason}");
                backoff.healthy_for(started, clock);
                let delay = backoff.transient_delay().max(MINIMUM_REPLACEMENT_DELAY);
                wait(shutdown, clock, Some(delay), &login_changed);
            }
        }
    }
}

/// Sleeps until shutdown, `until_changed` or the optional limit, whichever comes first.
fn wait(
    shutdown: &AtomicBool,
    clock: VenueClock,
    limit: Option<Duration>,
    until_changed: &dyn Fn() -> bool,
) {
    let deadline = limit.map(|limit| clock.monotonic() + limit);
    while !shutdown.load(Ordering::Acquire) && !until_changed() {
        let now = clock.monotonic();
        let slice = match deadline {
            Some(deadline) if now >= deadline => return,
            Some(deadline) => WAIT_SLICE.min(deadline - now),
            None => WAIT_SLICE,
        };
        thread::sleep(slice);
    }
}

const fn application() -> RithmicApplication<'static> {
    RithmicApplication {
        name: RITHMIC_APPLICATION_NAME,
        version: env!("CARGO_PKG_VERSION"),
    }
}

/// The saved login, copied out of the native vault; `None` when none is saved.
fn saved_login() -> Result<Option<RithmicCredentialBytes>, String> {
    let vault = NativeCredentialVault::new(RITHMIC_VAULT_SERVICE)
        .map_err(|_| "the native credential vault is unavailable".to_string())?;
    let Some(stored) = vault
        .load(RITHMIC_LOGIN_VAULT_KEY)
        .map_err(|_| "the saved Rithmic login could not be loaded".to_string())?
    else {
        return Ok(None);
    };
    let stored = Zeroizing::new(stored);
    RithmicCredentialBytes::try_copy_from_vault(&stored)
        .map(Some)
        .map_err(|_| "the saved Rithmic login is unreadable".to_string())
}
