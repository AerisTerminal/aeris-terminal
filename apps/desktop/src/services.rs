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

    /// Starts the optional session on this instance's account owner.
    ///
    /// # Errors
    /// Returns an error if the account client thread cannot start.
    pub fn start_account(&mut self) -> Result<(), String> {
        if crate::account::AUTH_BACKEND_CONFIGURED {
            self.account = Some(DesktopAccount::install(self.account_service.clone())?);
        }
        Ok(())
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
            AccountService::new(aeris_account_runtime::AccountServiceConfig::from_environment()),
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
            AccountService::new(aeris_account_runtime::AccountServiceConfig::from_environment()),
            DesktopPersistence::new(std::env::temp_dir().join("aeris-clock-first")),
            first_clock.clone(),
        );
        let second = DesktopServices::new(
            MarketService::start().expect("second market"),
            trading(),
            AccountService::new(aeris_account_runtime::AccountServiceConfig::from_environment()),
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
