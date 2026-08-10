use std::{path::PathBuf, thread};

pub(super) use axiusflow_coinbase_coordinator::market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup, PendingUiDiagnostics,
    UiDiagnosticsFeedback,
};

pub(super) fn start() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    MarketDataWorker::start_coinbase(
        "BTC-USD".to_string(),
        history_root("coinbase"),
        thread::current().id(),
        cfg!(feature = "diagnostics"),
        true,
    )
}

pub(super) fn start_rithmic() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    axiusflow_coinbase_coordinator::rithmic_market_worker::start(
        history_root("rithmic"),
        thread::current().id(),
        cfg!(feature = "diagnostics"),
        None,
    )
}

fn history_root(provider: &str) -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("local-data").join(format!("{provider}-history")),
        |root| {
            PathBuf::from(root)
                .join("Axiusflow")
                .join("market-history")
                .join(provider)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::history_root;

    #[test]
    fn fallback_history_root_is_provider_scoped() {
        if std::env::var_os("LOCALAPPDATA").is_none() {
            assert_eq!(
                history_root("coinbase"),
                std::path::PathBuf::from("local-data").join("coinbase-history")
            );
        }
    }
}
