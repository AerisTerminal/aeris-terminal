//! Per-desktop runtime handles supplied by the composition root.

use crate::account::DesktopAccount;
use crate::chart_chrome::DesktopPersistence;
use crate::trading::TradingFeedback;
use aeris_account_runtime::AccountService;
use aeris_market_runtime::MarketService;
use aeris_trading_runtime::TradingService;
use aeris_trading_runtime::clock::Clock;
use std::sync::Arc;

/// Cloneable command handles for one desktop instance. Runtime owners remain in
/// their respective runtimes; this value does not own trading state.
#[derive(Clone)]
pub struct DesktopServices {
    pub market: MarketService,
    pub trading: TradingService,
    pub trading_feedback: TradingFeedback,
    pub account_service: AccountService,
    pub account: Option<DesktopAccount>,
    pub persistence: DesktopPersistence,
    pub clock: Arc<dyn Clock>,
}

impl DesktopServices {
    #[must_use]
    pub fn new(
        market: MarketService,
        trading: TradingService,
        account_service: AccountService,
        persistence: DesktopPersistence,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            market,
            trading,
            trading_feedback: TradingFeedback::with_clock(Arc::clone(&clock)),
            account_service,
            account: None,
            persistence,
            clock,
        }
    }

    /// Attaches the sign-in session installed before these services were
    /// composed; a validation run passes none.
    #[must_use]
    pub fn with_account(mut self, account: Option<DesktopAccount>) -> Self {
        self.account = account;
        self
    }
}

/// Installs the sign-in session when this process requires one. A validation
/// run gets none and reaches the workspace without sign-in.
///
/// # Errors
/// Returns an error if the account client thread cannot start.
pub fn install_account(account_service: &AccountService) -> Result<Option<DesktopAccount>, String> {
    if crate::account::sign_in_required() {
        return DesktopAccount::install(account_service.clone()).map(Some);
    }
    Ok(None)
}

/// Holds the signed-in startup (provider sessions, the broker venue and the
/// workspace that demands market data) until the account owner first reports
/// a usable session, signed in or the cached offline one, then releases it
/// exactly once. Later offline, refresh and re-sign-in transitions find it
/// spent, so a session flip never starts a second provider session or venue.
pub struct SignedInStart<T> {
    pending: Option<T>,
}

impl<T> SignedInStart<T> {
    #[must_use]
    pub const fn new(start: T) -> Self {
        Self {
            pending: Some(start),
        }
    }

    /// Returns the held startup on the first call that observes an
    /// authenticated session, and `None` before it and ever after.
    pub fn release(&mut self, authenticated: bool) -> Option<T> {
        if authenticated {
            self.pending.take()
        } else {
            None
        }
    }

    /// Whether the startup has been released.
    #[must_use]
    pub const fn released(&self) -> bool {
        self.pending.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_trading_runtime::clock::ManualClock;

    fn trading() -> TradingService {
        TradingService::start(aeris_trading_runtime::TradingServiceConfig {
            database_path: ":memory:".into(),
            retention: aeris_trading_runtime::TradingRetention::default(),
            clock: std::sync::Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
            execution: aeris_trading_runtime::ExecutionPolicy::default(),
        })
        .expect("independent trading owner")
    }

    fn services() -> DesktopServices {
        DesktopServices::new(
            MarketService::start().expect("independent market owner"),
            trading(),
            AccountService::new(aeris_account_runtime::AccountServiceConfig::production()),
            DesktopPersistence::new(
                std::env::temp_dir().join(format!("aeris-desktop-services-{}", std::process::id())),
            ),
            std::sync::Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
        )
    }

    #[test]
    fn desktop_services_isolate_trading_owners() {
        let first = services();
        let second = services();
        let created = first
            .trading
            .create_practice_account(aeris_trading_runtime::CreatePracticeAccount {
                display_name: "Isolated fixture".to_string(),
                starting_equity: aeris_trading::FixedPoint::try_new(10_000, 2).expect("equity"),
            })
            .expect("create in first desktop");
        assert!(
            first
                .trading
                .snapshot()
                .expect("first snapshot")
                .accounts
                .iter()
                .any(|account| account.id == created.id)
        );
        assert!(
            !second
                .trading
                .snapshot()
                .expect("second snapshot")
                .accounts
                .iter()
                .any(|account| account.id == created.id)
        );
    }

    #[test]
    fn desktop_services_isolate_command_feedback_and_share_it_with_their_panes() {
        let first = services();
        let second = services();
        let pane = first.trading_feedback.clone();
        assert!(first.trading_feedback.latest().is_none());
        assert!(second.trading_feedback.latest().is_none());

        assert!(crate::trading::record_result(&pane, Ok("Accepted".into())));
        assert_eq!(
            first
                .trading_feedback
                .latest()
                .expect("first status")
                .revision,
            1
        );
        assert_eq!(
            first
                .trading_feedback
                .latest()
                .expect("pane status")
                .message,
            "Accepted"
        );
        assert!(second.trading_feedback.latest().is_none());
        assert!(!crate::trading::record_result(
            &second.trading_feedback,
            Err("Refused".into())
        ));
        assert_eq!(
            second
                .trading_feedback
                .latest()
                .expect("second status")
                .revision,
            1
        );
        assert_eq!(
            first
                .trading_feedback
                .latest()
                .expect("first unchanged")
                .revision,
            1
        );
    }

    #[test]
    fn desktop_services_clock_drives_shared_trading_feedback_without_cross_instance_time() {
        let first_clock = Arc::new(ManualClock::new(1_000_000_000));
        let second_clock = Arc::new(ManualClock::new(2_000_000_000));
        let first = DesktopServices::new(
            MarketService::start().expect("first market"),
            trading(),
            AccountService::new(aeris_account_runtime::AccountServiceConfig::production()),
            DesktopPersistence::new(std::env::temp_dir().join("aeris-clock-first")),
            first_clock.clone(),
        );
        let second = DesktopServices::new(
            MarketService::start().expect("second market"),
            trading(),
            AccountService::new(aeris_account_runtime::AccountServiceConfig::production()),
            DesktopPersistence::new(std::env::temp_dir().join("aeris-clock-second")),
            second_clock,
        );
        let pane_feedback = first.trading_feedback.clone();
        assert_eq!(pane_feedback.now(), first.clock.wall_unix_nanos());
        assert_eq!(second.trading_feedback.now(), 2_000_000_000);
        first_clock.set_wall_unix_nanos(3_000_000_000);
        assert_eq!(pane_feedback.now(), 3_000_000_000);
        assert_eq!(second.trading_feedback.now(), 2_000_000_000);
    }

    #[test]
    fn signed_in_start_releases_once_only_after_sign_in() {
        let started = std::cell::Cell::new(0_u32);
        let mut gate = SignedInStart::new(());
        // Restore pending, signed out, then browser sign-in in progress.
        for authenticated in [false, false, false] {
            if gate.release(authenticated).is_some() {
                started.set(started.get() + 1);
            }
        }
        assert_eq!(started.get(), 0, "nothing may start before sign-in");
        assert!(!gate.released());
        // Signed in, then offline, refresh, sign-out and a second sign-in.
        for authenticated in [true, true, true, false, true, true] {
            if gate.release(authenticated).is_some() {
                started.set(started.get() + 1);
            }
        }
        assert_eq!(started.get(), 1, "the signed-in startup runs exactly once");
        assert!(gate.released());
    }

    #[test]
    fn desktop_services_isolate_market_consumers() {
        let first = services();
        let second = services();
        let client_id = 7123;
        first.market.attach(client_id).expect("first attach");
        second
            .market
            .attach(client_id)
            .expect("second independent attach");
        first.market.detach(client_id).expect("first detach");
        second.market.detach(client_id).expect("second detach");
        first
            .market
            .shutdown(std::time::Duration::from_secs(2))
            .expect("first shutdown");
        second
            .market
            .shutdown(std::time::Duration::from_secs(2))
            .expect("second shutdown");
    }
}
