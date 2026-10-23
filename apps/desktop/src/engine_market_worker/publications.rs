//! Publications.

use super::{
    ChartInterval, FeedConnectionState, InstallProviderInstrument, MarketDemandErrorScope,
    MarketPublicationGeneration, MarketRecoveryFailure, MarketRuntimeEvent, MarketService,
    MarketWorkerBootstrap, MarketWorkerMessage, MarketWorkerSender, ProviderConnectionState,
    ProviderState, PushedEventContext, ReplayRecoveryCommand, WorkerEndpoint, chart_demand_streams,
    demand_error, provider_display_name, replay_runtime_snapshot, series_key, worker_identity,
};
use aeris_observability::diagnostic;

pub(super) fn apply_pushed_event(
    event: MarketRuntimeEvent,
    context: &PushedEventContext<'_>,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let &PushedEventContext {
        consumer_id,
        active_generation,
        realtime,
        instrument,
    } = context;
    match event {
        MarketRuntimeEvent::ProviderState(state) => {
            apply_provider_state(
                &state,
                instrument.provider.as_str(),
                active_generation,
                realtime,
                messages,
            )?;
            Ok(())
        }
        MarketRuntimeEvent::TradeTapeSnapshot(snapshot) => {
            if snapshot.consumer_id.0.get() != consumer_id {
                return Err("engine trade-tape consumer mismatched".to_string());
            }
            if snapshot.generation.0.get() != active_generation {
                return Ok(());
            }
            messages
                .send(MarketWorkerMessage::TradeTape(snapshot))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::StudyOutputSnapshot(snapshot) => {
            if snapshot.consumer_id.0.get() != consumer_id {
                return Err("engine study-output consumer mismatched".to_string());
            }
            messages
                .send(MarketWorkerMessage::StudyOutput(snapshot))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::StudyOutputsInvalidated(invalidated) => {
            if invalidated.consumer_id.0.get() != consumer_id {
                return Err("engine study-invalidation consumer mismatched".to_string());
            }
            messages
                .send(MarketWorkerMessage::StudyOutputsInvalidated(invalidated))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::StudyRemoved(removed) => {
            if removed.consumer_id.0.get() != consumer_id {
                return Err("engine study-removal consumer mismatched".to_string());
            }
            messages
                .send(MarketWorkerMessage::StudyRemoved(removed))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::PriceAlertTriggered(trigger) => {
            if trigger.consumer_id.0.get() != consumer_id {
                return Err("engine price-alert identity mismatched".to_string());
            }
            dispatch_native_price_alert(&trigger);
            messages
                .send(MarketWorkerMessage::PriceAlertTriggered(trigger))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected pushed market event".to_string()),
    }
}

fn dispatch_native_price_alert(trigger: &super::MarketPriceAlertTrigger) {
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

pub(super) fn apply_provider_state(
    state: &ProviderState,
    expected_provider: &str,
    selection_generation: u64,
    realtime: bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if state.provider != expected_provider {
        return Err("engine provider state identity mismatched".to_string());
    }
    if !realtime {
        return Ok(());
    }
    let provider_name = provider_display_name(expected_provider);
    let provider_state = state.state;
    let (connection, detail) = match provider_state {
        ProviderConnectionState::Disconnected => (
            FeedConnectionState::Disconnected,
            format!("{provider_name} realtime is disconnected"),
        ),
        ProviderConnectionState::Connecting => (
            FeedConnectionState::Discovering,
            format!("{provider_name} realtime is connecting"),
        ),
        ProviderConnectionState::Online => (
            FeedConnectionState::Streaming,
            format!("{provider_name} history and realtime are current"),
        ),
        ProviderConnectionState::Recovering => (
            FeedConnectionState::Recovering,
            format!("{provider_name} realtime is recovering; retained history remains visible"),
        ),
        ProviderConnectionState::Failed => (
            FeedConnectionState::Stopped,
            format!("{provider_name} realtime stopped"),
        ),
    };
    messages
        .send(MarketWorkerMessage::Connection {
            generation: selection_generation,
            state: connection,
            message: state.detail.clone().unwrap_or(detail),
            transport_rtt_nanos: state.transport_rtt_nanos,
        })
        .map_err(|error| error.to_string())
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
            .send(MarketWorkerMessage::Recovery {
                generation: endpoint.active_generation,
                request_id: previous.request_id,
                result: Err(MarketRecoveryFailure::Superseded),
            })
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
            .send(MarketWorkerMessage::Recovery {
                generation: endpoint.active_generation,
                request_id: command.request_id,
                result: Err(MarketRecoveryFailure::Failed(error)),
            })
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
        .send(MarketWorkerMessage::Recovery {
            generation: endpoint.active_generation,
            request_id: pending.request_id,
            result: Err(MarketRecoveryFailure::Superseded),
        })
        .map_err(|error| error.to_string())?;
    endpoint.pending_recovery = None;
    Ok(())
}

pub(super) fn complete_pending_recovery(
    event: &MarketRuntimeEvent,
    product: &InstallProviderInstrument,
    endpoint: &mut WorkerEndpoint,
    received_unix_nanos: i64,
) -> Result<bool, String> {
    let Some(command) = endpoint.pending_recovery else {
        return Ok(false);
    };
    match event {
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
            let generation = MarketPublicationGeneration::from_snapshot(&replay)
                .with_selection_generation(endpoint.active_generation);
            let (subscription_id, worker_label) = worker_identity(product.provider.as_str());
            endpoint
                .messages
                .send(MarketWorkerMessage::Recovery {
                    generation: endpoint.active_generation,
                    request_id: command.request_id,
                    result: Ok(Box::new(MarketWorkerBootstrap {
                        snapshot: replay,
                        subscription_id: subscription_id.to_string(),
                        generation,
                        worker_label: worker_label.to_string(),
                    })),
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
                .send(MarketWorkerMessage::Recovery {
                    generation: endpoint.active_generation,
                    request_id: command.request_id,
                    result: Err(MarketRecoveryFailure::Failed(demand_error(error))),
                })
                .map_err(|error| error.to_string())?;
            endpoint.pending_recovery = None;
            Ok(true)
        }
        _ => Ok(false),
    }
}
