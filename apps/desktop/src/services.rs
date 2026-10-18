//! Per-desktop runtime handles supplied by the composition root.

use aeris_trading_runtime::TradingService;

/// Cloneable command handles for one desktop instance. Runtime owners remain in
/// their respective runtimes; this value does not own trading state.
#[derive(Clone)]
pub struct DesktopServices {
    pub trading: TradingService,
}

impl DesktopServices {
    #[must_use]
    pub fn new(trading: TradingService) -> Self {
        Self { trading }
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

    #[test]
    fn desktop_services_isolate_trading_owners() {
        let first = DesktopServices::new(trading());
        let second = DesktopServices::new(trading());
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
