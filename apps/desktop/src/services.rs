//! Per-desktop runtime handles supplied by the composition root.

use crate::account::DesktopAccount;
use aeris_account_runtime::AccountService;
use aeris_trading_runtime::TradingService;

/// Cloneable command handles for one desktop instance. Runtime owners remain in
/// their respective runtimes; this value does not own trading state.
#[derive(Clone)]
pub struct DesktopServices {
    pub trading: TradingService,
    pub account_service: AccountService,
    pub account: Option<DesktopAccount>,
}

impl DesktopServices {
    #[must_use]
    pub fn new(trading: TradingService, account_service: AccountService) -> Self {
        Self {
            trading,
            account_service,
            account: None,
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
            trading(),
            AccountService::new(aeris_account_runtime::AccountServiceConfig::from_environment()),
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
}
