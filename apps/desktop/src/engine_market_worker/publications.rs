//! Publications.

use super::{
    ChartInterval, InstallProviderInstrument, LocalCommandOutcome, MarketDemandErrorScope,
    MarketRecoveryFailure, MarketRuntimeEvent, MarketService, MarketWorkerMessage,
    ReplayRecoveryCommand, WorkerEndpoint, chart_demand_streams, demand_error,
    replay_runtime_snapshot, series_key,
};
use aeris_observability::diagnostic;

pub(super) fn dispatch_native_price_alert(trigger: &super::MarketPriceAlertTrigger) {
    let (title, description) = crate::desktop::price_alert_trigger_text(trigger);
    let notification = aeris_platform_runtime::NativeUserNotification::try_new(
        "Aeris price alert",
        format!("{title}. {description}."),
    );
    match notification.and_then(aeris_platform_runtime::try_send_user_notification) {
        Ok(()) => {}
        Err(error) => diagnostic!("Aeris price alert notification was not delivered: {error}"),
    }
}

pub(super) fn send_recovery(
    market: &MarketService,
    client_id: u64,
    product: &InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
    command: ReplayRecoveryCommand,
) -> Result<(), String> {
    let series = series_key(product, interval)?;
    if let Some(previous) = endpoint.pending_recovery {
        endpoint
            .messages
            .send(MarketWorkerMessage::LocalCommandOutcome(
                LocalCommandOutcome::Recovery {
                    generation: endpoint.active_generation,
                    request_id: previous.request_id,
                    failure: MarketRecoveryFailure::Superseded,
                },
            ))
            .map_err(|error| error.to_string())?;
    }
    endpoint.pending_recovery = Some(command);
    let demand = chart_demand_streams(market, &product.provider, endpoint.depth_visible).and_then(
        |streams| {
            market.set_demand(
                client_id,
                endpoint.consumer_id,
                endpoint.active_generation,
                &series,
                streams,
            )
        },
    );
    if let Err(error) = demand {
        endpoint
            .messages
            .send(MarketWorkerMessage::LocalCommandOutcome(
                LocalCommandOutcome::Recovery {
                    generation: endpoint.active_generation,
                    request_id: command.request_id,
                    failure: MarketRecoveryFailure::Failed(error),
                },
            ))
            .map_err(|error| error.to_string())?;
        endpoint.pending_recovery = None;
    }
    Ok(())
}

/// Retires the pending recovery because a new market selection replaces the chart it served.
pub(super) fn cancel_pending_recovery(endpoint: &mut WorkerEndpoint) -> Result<(), String> {
    let Some(pending) = endpoint.pending_recovery else {
        return Ok(());
    };
    endpoint
        .messages
        .send(MarketWorkerMessage::LocalCommandOutcome(
            LocalCommandOutcome::Recovery {
                generation: endpoint.active_generation,
                request_id: pending.request_id,
                failure: MarketRecoveryFailure::Superseded,
            },
        ))
        .map_err(|error| error.to_string())?;
    endpoint.pending_recovery = None;
    Ok(())
}

pub(super) fn complete_pending_recovery(
    event: &aeris_market_runtime::MarketConsumerEvent,
    product: &InstallProviderInstrument,
    endpoint: &mut WorkerEndpoint,
    received_unix_nanos: i64,
) -> Result<bool, String> {
    let Some(command) = endpoint.pending_recovery else {
        return Ok(false);
    };
    match &event.value {
        MarketRuntimeEvent::SeriesSnapshot(snapshot)
            if snapshot.consumer_id.0.get() == endpoint.consumer_id
                && snapshot.generation.0.get() == endpoint.active_generation
                && snapshot.snapshot.provider_generation.0.get() >= product.session_generation =>
        {
            let replay = replay_runtime_snapshot(snapshot, Some(product), received_unix_nanos)?;
            // Recovery needs a covering bar. Previously the duplicate
            // MarketGeneration constructor rejected this empty case.
            if replay.bars().is_empty() {
                return Err("runtime snapshot is empty".to_string());
            }
            endpoint
                .messages
                .send(MarketWorkerMessage::RuntimeSeriesPublication {
                    event: event.clone(),
                    received_unix_nanos,
                    recovery_request_id: Some(command.request_id),
                })
                .map_err(|error| error.to_string())?;
            endpoint.pending_recovery = None;
            Ok(true)
        }
        MarketRuntimeEvent::DemandError(error)
            if error.scope == MarketDemandErrorScope::Series
                && error.consumer_id.0.get() == endpoint.consumer_id
                && error.generation.0.get() == endpoint.active_generation =>
        {
            endpoint
                .messages
                .send(MarketWorkerMessage::LocalCommandOutcome(
                    LocalCommandOutcome::Recovery {
                        generation: endpoint.active_generation,
                        request_id: command.request_id,
                        failure: MarketRecoveryFailure::Failed(demand_error(error)),
                    },
                ))
                .map_err(|error| error.to_string())?;
            endpoint.pending_recovery = None;
            Ok(true)
        }
        _ => Ok(false),
    }
}
