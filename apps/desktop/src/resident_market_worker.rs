use axiusflow_application::{ReplayProvenance, ReplayStreamUpdate};
use std::{path::PathBuf, thread, time::Duration};

pub(super) use axiusflow_desktop_market_runtime::market_worker::{
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
    axiusflow_desktop_market_runtime::rithmic_market_worker::start(
        history_root("rithmic"),
        thread::current().id(),
        cfg!(feature = "diagnostics"),
        None,
    )
}

pub(super) fn run_coinbase_live_smoke(
    product_id: &str,
    history_root: PathBuf,
) -> Result<(), String> {
    let (startup, mut worker) = MarketDataWorker::start_coinbase(
        product_id.to_string(),
        history_root,
        thread::current().id(),
        false,
        true,
    )?;
    if !matches!(startup, MarketWorkerStartup::Loading(_)) {
        return Err("Coinbase desktop worker bypassed the loading state".to_string());
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let mut local_cache_observed = false;
    let mut last_state = None;
    let mut last_state_message = None;
    let mut last_snapshot_provenance = None;
    loop {
        let (messages, disconnected) = worker.drain_messages();
        for message in messages {
            match message {
                MarketWorkerMessage::Update(publication) => match publication.update {
                    ReplayStreamUpdate::Snapshot(snapshot)
                        if snapshot.provenance() == ReplayProvenance::LiveProvider =>
                    {
                        drop(worker);
                        println!(
                            "coinbase_shipping_live_smoke=passed product={product_id} loading=true local_cache_observed={local_cache_observed} covering_snapshot=true clean_shutdown=true"
                        );
                        return Ok(());
                    }
                    ReplayStreamUpdate::Snapshot(snapshot)
                        if snapshot.provenance() == ReplayProvenance::LocalCache =>
                    {
                        local_cache_observed = true;
                        last_snapshot_provenance = Some(snapshot.provenance());
                    }
                    ReplayStreamUpdate::Snapshot(snapshot) => {
                        last_snapshot_provenance = Some(snapshot.provenance());
                    }
                    ReplayStreamUpdate::Delta(_) => {}
                },
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message,
                } => {
                    return Err(format!(
                        "{message} (previous_state_message={last_state_message:?})"
                    ));
                }
                MarketWorkerMessage::State { state, message } => {
                    last_state = Some(state);
                    last_state_message = Some(message);
                }
                _ => {}
            }
        }
        if disconnected {
            return Err("Coinbase desktop worker disconnected before its snapshot".to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Coinbase desktop worker timed out before its snapshot (local_cache_observed={local_cache_observed}, last_snapshot_provenance={last_snapshot_provenance:?}, last_state={last_state:?}, last_state_message={last_state_message:?})"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
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
