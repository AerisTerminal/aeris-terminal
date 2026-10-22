//! The Rithmic broker venue relay. One order-plant and one P&L-plant connection under the
//! saved login translate the trading owner's venue requests and report the broker's orders,
//! fills, positions and balances back. Only demo logins (Rithmic Test and paper trading)
//! are routed: a live login is refused before any account is reported.
//!
//! Each connection is one venue session generation attached to the trading owner. A lost
//! connection drops its request receiver, so the owner refuses orders until the next
//! session attaches and reconciles every account.

mod reconcile;
mod reports;
mod requests;
mod session;
mod translate;

use crate::market_service::{
    BrokerVenueLink,
    clock::{Clock, SystemClock},
    reconnect::{Backoff, ReconnectPolicy},
};
use crate::rithmic_realtime::RithmicLoginRevision;
use aeris_observability::diagnostic;
use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use aeris_rithmic_protocol_adapter::{
    RITHMIC_APPLICATION_NAME, RITHMIC_LOGIN_VAULT_KEY, RITHMIC_VAULT_SERVICE, RithmicApplication,
    RithmicCredentialBytes,
};
use session::SessionEnd;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

/// Separates this relay's reconnect jitter from the market-data session's.
const BACKOFF_SEED: u64 = 0x5645_4e55_4552_4c59;
/// A replaced session waits at least this long before the next login.
const MINIMUM_REPLACEMENT_DELAY: Duration = Duration::from_secs(1);
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// Runs the relay until shutdown: one session at a time, reconnecting with the bounded
/// Rithmic backoff after a loss and waiting for a login change when the saved login cannot
/// be routed.
pub(crate) fn run(
    mut link: BrokerVenueLink,
    shutdown: &Arc<AtomicBool>,
    login: &RithmicLoginRevision,
) {
    let clock = SystemClock;
    let mut backoff = Backoff::new(ReconnectPolicy::rithmic_recovery(), &clock, BACKOFF_SEED);
    let mut reported_idle: Option<String> = None;
    while !shutdown.load(Ordering::Acquire) {
        let revision = login.current();
        let login_changed = || login.current() != revision;
        let started = clock.monotonic();
        match session::run(&mut link, shutdown, &clock, &login_changed) {
            SessionEnd::Shutdown => return,
            SessionEnd::Idle(reason) => {
                if reported_idle.as_deref() != Some(reason.as_str()) {
                    diagnostic!("Aeris broker venue is idle: {reason}");
                    reported_idle = Some(reason);
                }
                backoff.reset();
                wait(shutdown, &clock, None, &login_changed);
            }
            SessionEnd::LoginChanged => {
                reported_idle = None;
                backoff.reset();
                wait(shutdown, &clock, Some(MINIMUM_REPLACEMENT_DELAY), &|| false);
            }
            SessionEnd::Lost(reason) => {
                reported_idle = None;
                diagnostic!("Aeris broker venue lost its Rithmic session: {reason}");
                backoff.healthy_for(started, &clock);
                let delay = backoff.transient_delay().max(MINIMUM_REPLACEMENT_DELAY);
                wait(shutdown, &clock, Some(delay), &login_changed);
            }
        }
    }
}

/// Sleeps until shutdown, `until_changed` or the optional limit, whichever comes first.
fn wait(
    shutdown: &AtomicBool,
    clock: &dyn Clock,
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
