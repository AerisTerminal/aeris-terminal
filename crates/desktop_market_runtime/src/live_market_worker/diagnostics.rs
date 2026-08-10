use super::composition::CoinbaseDesktopWorker;
use crate::market_worker::{
    MarketWorkerMessage, MarketWorkerSender, UiDiagnosticsFeedback, UiDiagnosticsReceiver,
};
use axiusflow_desktop_provider_runtime::{DesktopMarketWorkerError, DesktopProviderError};
use axiusflow_observability::{
    DiagnosticsQueue, LatencyBoundary, LatencyTimestampChain, LocalLatencyMetric,
};
use std::{mem::size_of, time::Duration};

const DIAGNOSTICS_CADENCE: Duration = Duration::from_millis(250);

pub(super) fn diagnostics_wait_duration(reconnect: Option<Duration>) -> Duration {
    reconnect.map_or(DIAGNOSTICS_CADENCE, |wait| wait.min(DIAGNOSTICS_CADENCE))
}

pub(super) fn flush_diagnostics<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    feedback_rx: &UiDiagnosticsReceiver,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let (feedback_items, _) = feedback_rx.occupancy();
    for _ in 0..feedback_items {
        let Some(feedback) = feedback_rx.try_recv() else {
            break;
        };
        match feedback {
            UiDiagnosticsFeedback::Presented {
                generation,
                ui_enqueue_nanos,
                frame_submit_nanos,
                present_nanos,
            } => {
                let mut chain = LatencyTimestampChain::new();
                chain.set(LatencyBoundary::ModelPublish, 0);
                chain.set(LatencyBoundary::UiEnqueue, ui_enqueue_nanos);
                chain.set(LatencyBoundary::FrameSubmit, frame_submit_nanos);
                chain.set(LatencyBoundary::Present, present_nanos);
                for metric in [
                    LocalLatencyMetric::ModelPublishToUiEnqueue,
                    LocalLatencyMetric::UiEnqueueToFrameSubmit,
                    LocalLatencyMetric::FrameSubmitToPresent,
                ] {
                    if let Err(error) =
                        worker.record_latency_diagnostics(generation, metric, &chain)
                    {
                        if is_stale_feedback(&error) {
                            break;
                        }
                        return Err(error.to_string());
                    }
                }
            }
            UiDiagnosticsFeedback::Coalesced { generation } => {
                if let Err(error) = worker.record_coalesced_ui_update_diagnostics(generation, 1)
                    && !is_stale_feedback(&error)
                {
                    return Err(error.to_string());
                }
            }
        }
    }

    while let Some(coalesced) = feedback_rx.try_take_coalesced_feedback() {
        if let Err(error) =
            worker.record_coalesced_ui_update_diagnostics(coalesced.generation, coalesced.count)
            && !is_stale_feedback(&error)
        {
            return Err(error.to_string());
        }
    }
    while let Some(coalesced) = message_tx.try_take_coalesced_update() {
        if let Err(error) =
            worker.record_coalesced_ui_update_diagnostics(coalesced.generation, coalesced.count)
            && !is_stale_feedback(&error)
        {
            return Err(error.to_string());
        }
    }

    let (current_items, item_capacity) = message_tx.occupancy();
    let item_bytes = size_of::<MarketWorkerMessage>();
    worker
        .observe_diagnostics_queue(
            DiagnosticsQueue::UiUpdate,
            current_items,
            item_capacity,
            current_items.saturating_mul(item_bytes),
            item_capacity.saturating_mul(item_bytes),
        )
        .map_err(|error| error.to_string())?;

    if let Some(snapshot) = worker
        .try_diagnostics_snapshot()
        .map_err(|error| error.to_string())?
    {
        message_tx
            .send(MarketWorkerMessage::Diagnostics(Box::new(snapshot)))
            .map_err(|_| "desktop market UI channel disconnected".to_string())?;
    }
    Ok(())
}

fn is_stale_feedback(error: &DesktopMarketWorkerError) -> bool {
    matches!(
        error,
        DesktopMarketWorkerError::Provider(DesktopProviderError::StaleGeneration)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_cadence_never_delays_an_earlier_reconnect() {
        assert_eq!(diagnostics_wait_duration(None), DIAGNOSTICS_CADENCE);
        assert_eq!(
            diagnostics_wait_duration(Some(Duration::from_millis(25))),
            Duration::from_millis(25)
        );
        assert_eq!(
            diagnostics_wait_duration(Some(Duration::from_secs(1))),
            DIAGNOSTICS_CADENCE
        );
    }
}
