use std::{path::PathBuf, thread};

pub(super) use axiusflow_desktop_market_runtime::market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerCommand, MarketWorkerMessage, MarketWorkerPublication, MarketWorkerSender,
    MarketWorkerStartup, PendingUiDiagnostics, UiDiagnosticsFeedback, market_worker_channel,
};

pub(super) fn start() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    crate::engine_market_worker::start()
}

pub(super) fn start_rithmic() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    axiusflow_desktop_market_runtime::rithmic_market_worker::start(
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
                history_root("rithmic"),
                std::path::PathBuf::from("local-data").join("rithmic-history")
            );
        }
    }
}
