//! Selection commands.

use super::{
    ConsumerResourceClass, EndpointRecord, FeedConnectionState, InstallProviderInstrument,
    MarketPriceAlert, MarketRecoveryFailure, MarketService, MarketWorkerCommand,
    MarketWorkerMessage, ReplayRecoveryCommand, StreamRequirements, WorkerEndpoint,
    cancel_pending_recovery, chart_demand_streams, chart_streams, retire_endpoint, send_recovery,
};
use aeris_desktop::market_worker::MarketWorkerSendError;
use aeris_market_runtime::{InstrumentRef, InstrumentRequest, SelectionRequest};
use aeris_observability::diagnostic;

pub(super) fn fence_recovery_command(
    endpoint: &WorkerEndpoint,
    selection_generation: Option<u64>,
    command: ReplayRecoveryCommand,
) -> Result<Option<ReplayRecoveryCommand>, String> {
    if selection_generation.is_none_or(|generation| generation == endpoint.active_generation) {
        return Ok(Some(command));
    }
    endpoint
        .messages
        .send(MarketWorkerMessage::Recovery {
            generation: selection_generation.unwrap_or(endpoint.active_generation),
            request_id: command.request_id,
            failure: MarketRecoveryFailure::Superseded,
        })
        .map_err(|error| error.to_string())?;
    Ok(None)
}

pub(super) fn initialize_catalog_endpoint(
    market: &MarketService,
    client_id: u64,
    workspace_id: u64,
    endpoint: &mut WorkerEndpoint,
) -> Result<(), String> {
    market.register_consumer(client_id, workspace_id, endpoint.consumer_id)?;
    Ok(())
}

pub(super) fn initialize_market_screen_endpoint(
    market: &MarketService,
    client_id: u64,
    workspace_id: u64,
    endpoint: &WorkerEndpoint,
) -> Result<(), String> {
    market.register_consumer(client_id, workspace_id, endpoint.consumer_id)
}

pub(super) fn initialize_endpoint(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) -> Result<(), String> {
    match record
        .endpoint
        .messages
        .send(MarketWorkerMessage::Connection {
            generation: record.endpoint.active_generation,
            state: FeedConnectionState::Discovering,
            message: "Connecting to market data".to_string(),
            transport_rtt_nanos: None,
        }) {
        Ok(()) | Err(MarketWorkerSendError::Disconnected) => {
            // A retired pane may release the receiver before registration.
        }
        Err(MarketWorkerSendError::Full) => {
            diagnostic!("Aeris startup market connection state could not enter the pane mailbox");
        }
    }
    market.register_consumer(client_id, record.workspace_id, record.endpoint.consumer_id)?;
    if record.endpoint.resource_class != ConsumerResourceClass::Foreground {
        market.set_resource_class(
            client_id,
            record.endpoint.consumer_id,
            record.endpoint.resource_class,
        )?;
    }

    begin_endpoint_demand(market, client_id, record)
}

fn begin_endpoint_demand(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) -> Result<(), String> {
    let streams = chart_demand_streams(
        market,
        &record.product.provider,
        record.endpoint.depth_visible,
    )?;
    if let Err(error) = dispatch_initial_selection(record, streams, |request| {
        market.select_and_demand(client_id, request)
    }) {
        super::runtime::report_failure(
            &record.endpoint.messages,
            Some(record.endpoint.active_generation),
            error,
        );
        return Ok(());
    }

    Ok(())
}

pub(super) fn dispatch_initial_selection(
    record: &EndpointRecord,
    streams: StreamRequirements,
    dispatch: impl FnOnce(SelectionRequest) -> Result<(), String>,
) -> Result<(), String> {
    dispatch(SelectionRequest {
        consumer_id: record.endpoint.consumer_id,
        generation: record.endpoint.active_generation,
        instrument: record.initial_instrument.clone(),
        interval: record.interval,
        streams,
        viewport: None,
    })
}

fn instrument_ref(product: &InstallProviderInstrument) -> InstrumentRequest {
    InstrumentRequest::Ref(InstrumentRef {
        provider_id: product.provider.clone(),
        instrument_id: product.instrument_id.clone(),
        entitlement_id: product.entitlement_id.clone(),
        query: Some(product.provider_symbol.clone()),
    })
}

/// Records the desired order-book depth visibility and applies it to installed demand.
///
/// Visibility is endpoint intent, so it is retained even when no series demand
/// exists yet: while the startup instrument is still resolving against the live
/// catalog, the adopted selection receives the retained streams through
/// `apply_depth_visibility`.
pub(super) fn set_depth_visible(
    market: &MarketService,
    client_id: u64,
    endpoint: &mut WorkerEndpoint,
    provider: &str,
    startup_resolving: bool,
    visible: bool,
) -> Result<(), String> {
    let Some(streams) = retain_depth_visibility(endpoint, startup_resolving, visible) else {
        return Ok(());
    };
    market.set_streams(
        client_id,
        endpoint.consumer_id,
        endpoint.active_generation,
        market.available_streams(provider, streams)?,
    )
}

/// Applies the retained depth visibility to an adopted selection's demand. A selection
/// dispatched before the order book opened or closed carried the older streams; when they
/// already match, the runtime leaves the demand unchanged.
pub(super) fn apply_depth_visibility(
    market: &MarketService,
    client_id: u64,
    record: &EndpointRecord,
) -> Result<(), String> {
    let endpoint = &record.endpoint;
    market.set_streams(
        client_id,
        endpoint.consumer_id,
        endpoint.active_generation,
        chart_demand_streams(market, &record.product.provider, endpoint.depth_visible)?,
    )
}

/// Retains the visibility and returns the streams to apply when demand is installed.
pub(super) fn retain_depth_visibility(
    endpoint: &mut WorkerEndpoint,
    startup_resolving: bool,
    visible: bool,
) -> Option<StreamRequirements> {
    endpoint.depth_visible = visible;
    (endpoint.active_generation != 0 && !startup_resolving).then(|| chart_streams(visible))
}

pub(super) fn process_command(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
    command: MarketWorkerCommand,
) -> Result<(), String> {
    if let MarketWorkerCommand::SelectCatalog(request) = &command {
        return process_catalog_select(market, client_id, record, request);
    }
    let EndpointRecord {
        product,
        interval,
        endpoint,
        pending_selection,
        initial_selection_pending,
        ..
    } = record;
    match command {
        MarketWorkerCommand::ProviderSearch(mut request) => {
            request.consumer_id = endpoint.consumer_id;
            market.search_provider_instruments(client_id, request)
        }
        MarketWorkerCommand::ProviderSelect(mut request) => {
            request.consumer_id = endpoint.consumer_id;
            market.select_provider_instrument(client_id, request)
        }
        MarketWorkerCommand::ProviderScreen(mut request) => {
            request.consumer_id = endpoint.consumer_id;
            market.screen_provider_markets(client_id, request)
        }
        MarketWorkerCommand::EngineSelect(request) => process_engine_select(
            market,
            client_id,
            product,
            interval,
            endpoint,
            pending_selection,
            &request,
        ),
        MarketWorkerCommand::SelectCatalog(_) => unreachable!("handled before other commands"),
        MarketWorkerCommand::Recovery {
            selection_generation,
            command,
        } => {
            let Some(command) = fence_recovery_command(endpoint, selection_generation, command)?
            else {
                return Ok(());
            };
            send_recovery(market, client_id, product, *interval, endpoint, command)
        }
        MarketWorkerCommand::ChartViewport(viewport) => {
            if viewport.selection_generation > 0 {
                market.set_viewport(
                    client_id,
                    endpoint.consumer_id,
                    viewport.selection_generation,
                    viewport.start_unix_nanos,
                    viewport.end_unix_nanos,
                )?;
            }
            Ok(())
        }
        MarketWorkerCommand::DepthVisible(visible) => set_depth_visible(
            market,
            client_id,
            endpoint,
            &product.provider,
            endpoint.active_generation == 0 || *initial_selection_pending,
            visible,
        ),
        MarketWorkerCommand::ResourceClass(resource_class) => {
            set_resource_class(market, client_id, endpoint, resource_class)
        }
        MarketWorkerCommand::ReplacePriceAlerts { sequence, alerts } => {
            replace_price_alerts(market, client_id, endpoint, sequence, &alerts)
        }
        command @ (MarketWorkerCommand::RegisterStudy(_)
        | MarketWorkerCommand::ReinitializeStudy(_)
        | MarketWorkerCommand::RemoveStudy(_)) => {
            process_study_command(market, client_id, endpoint, command)
        }
        MarketWorkerCommand::Shutdown => {
            retire_endpoint(market, client_id, endpoint);
            Ok(())
        }
    }
}

fn process_catalog_select(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
    request: &aeris_desktop::market_worker::CatalogSelectionRequest,
) -> Result<(), String> {
    let endpoint = &mut record.endpoint;
    cancel_pending_recovery(endpoint)?;
    market.select_and_demand(
        client_id,
        SelectionRequest {
            consumer_id: endpoint.consumer_id,
            generation: request.sequence,
            instrument: aeris_market_runtime::InstrumentRequest::CatalogSymbol {
                provider_id: request.provider.clone(),
                symbol: request.symbol.clone(),
                exchange: request.exchange.clone(),
                entitlement_id: request.entitlement_id.clone(),
                search_generation: request.search_generation,
            },
            interval: request.interval,
            streams: chart_demand_streams(market, &request.provider, endpoint.depth_visible)?,
            viewport: None,
        },
    )?;
    record.pending_selection = Some((request.sequence, record.product.clone(), request.interval));
    record.pending_catalog_selection = Some((request.sequence, request.browser_generation));
    Ok(())
}

fn process_engine_select(
    market: &MarketService,
    client_id: u64,
    _product: &mut InstallProviderInstrument,
    _interval: &mut aeris_market_data::ChartInterval,
    endpoint: &mut WorkerEndpoint,
    pending_selection: &mut Option<(
        u64,
        InstallProviderInstrument,
        aeris_market_data::ChartInterval,
    )>,
    request: &aeris_desktop::market_worker::EngineSelectionRequest,
) -> Result<(), String> {
    cancel_pending_recovery(endpoint)?;
    market.select_and_demand(
        client_id,
        SelectionRequest {
            consumer_id: endpoint.consumer_id,
            generation: request.sequence,
            instrument: instrument_ref(&request.product),
            interval: request.interval,
            streams: chart_demand_streams(
                market,
                &request.product.provider,
                endpoint.depth_visible,
            )?,
            viewport: None,
        },
    )?;
    // The endpoint adopts the new binding only after Selected arrives ahead
    // of its first snapshot. A failure leaves the old series live.
    *pending_selection = Some((request.sequence, request.product.clone(), request.interval));
    Ok(())
}

fn process_study_command(
    market: &MarketService,
    client_id: u64,
    endpoint: &WorkerEndpoint,
    command: MarketWorkerCommand,
) -> Result<(), String> {
    match command {
        MarketWorkerCommand::RegisterStudy(request) => {
            register_study(market, client_id, endpoint, *request)
        }
        MarketWorkerCommand::ReinitializeStudy(request) => {
            reinitialize_study(market, client_id, endpoint, *request)
        }
        MarketWorkerCommand::RemoveStudy(study_id) => {
            remove_study(market, client_id, endpoint, study_id)
        }
        _ => unreachable!("only study commands reach study dispatch"),
    }
}

fn reinitialize_study(
    market: &MarketService,
    client_id: u64,
    endpoint: &WorkerEndpoint,
    request: aeris_desktop::market_worker::StudyReinitializationRequest,
) -> Result<(), String> {
    match market.reinitialize_study(client_id, request.study_id, request.registration) {
        Ok(_) => endpoint
            .messages
            .send(MarketWorkerMessage::StudyReinitialized {
                study_id: request.study_id,
            })
            .map_err(|error| error.to_string()),
        Err(message) => endpoint
            .messages
            .send(MarketWorkerMessage::StudyReinitializationFailed {
                study_id: request.study_id,
                message,
            })
            .map_err(|error| error.to_string()),
    }
}

fn register_study(
    market: &MarketService,
    client_id: u64,
    endpoint: &WorkerEndpoint,
    request: aeris_desktop::market_worker::StudyRegistrationRequest,
) -> Result<(), String> {
    let result = market.register_study(client_id, endpoint.consumer_id, request.registration);
    endpoint
        .messages
        .send(MarketWorkerMessage::StudyRegistrationOutcome {
            request_sequence: request.sequence,
            result,
        })
        .map_err(|error| error.to_string())
}

fn remove_study(
    market: &MarketService,
    client_id: u64,
    endpoint: &WorkerEndpoint,
    study_id: aeris_market_runtime::study::StudyInstanceId,
) -> Result<(), String> {
    match market.remove_study(client_id, study_id) {
        Ok(_) => Ok(()),
        Err(message) => endpoint
            .messages
            .send(MarketWorkerMessage::StudyRemovalFailed { study_id, message })
            .map_err(|error| error.to_string()),
    }
}

fn replace_price_alerts(
    market: &MarketService,
    client_id: u64,
    endpoint: &WorkerEndpoint,
    sequence: u64,
    alerts: &[MarketPriceAlert],
) -> Result<(), String> {
    let succeeded = market
        .replace_price_alerts(client_id, endpoint.consumer_id, alerts)
        .is_ok();
    endpoint
        .messages
        .send(MarketWorkerMessage::PriceAlertSync {
            sequence,
            succeeded,
        })
        .map_err(|error| error.to_string())
}

pub(super) fn set_resource_class(
    market: &MarketService,
    client_id: u64,
    endpoint: &mut WorkerEndpoint,
    resource_class: ConsumerResourceClass,
) -> Result<(), String> {
    market.set_resource_class(client_id, endpoint.consumer_id, resource_class)?;
    endpoint.resource_class = resource_class;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_selection_preserves_catalog_identity() {
        let product = InstallProviderInstrument {
            provider: "rithmic".into(),
            provider_symbol: "ESZ6".into(),
            instrument_id: "rithmic:future:ESZ6:CME".into(),
            display_symbol: "ESZ6".into(),
            entitlement_id: "futures".into(),
            venue_id: "CME".into(),
            ..Default::default()
        };
        assert!(
            matches!(instrument_ref(&product), InstrumentRequest::Ref(reference)
            if reference.instrument_id == product.instrument_id
                && reference.entitlement_id == product.entitlement_id)
        );
    }
}
