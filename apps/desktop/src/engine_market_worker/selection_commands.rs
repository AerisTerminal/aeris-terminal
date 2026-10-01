//! Selection commands.

use super::{
    ChartState, ConsumerResourceClass, EndpointRecord, FeedConnectionState,
    InstallProviderInstrument, MarketPriceAlert, MarketRuntimeEvent, MarketService,
    MarketWorkerCommand, MarketWorkerMessage, ProviderInstrumentSummary,
    RITHMIC_CATALOG_READY_MESSAGE, ReplayRecoveryCommand, STARTUP_CATALOG_COMMAND_GENERATION,
    SearchProviderInstruments, SelectProviderInstrument, StartupResolution, StreamRequirements,
    WorkerEndpoint, cancel_pending_recovery, chart_streams, provider_display_name, retire_endpoint,
    send_recovery, series_key,
};

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
            request_id: command.request_id,
            result: Err("market recovery was superseded by a newer market selection".to_string()),
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
    endpoint
        .messages
        .send(MarketWorkerMessage::Connection {
            state: FeedConnectionState::Authenticating,
            message: RITHMIC_CATALOG_READY_MESSAGE.to_string(),
            transport_rtt_nanos: None,
        })
        .map_err(|error| error.to_string())
}

pub(super) fn initialize_endpoint(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) -> Result<(), String> {
    let _ = record
        .endpoint
        .messages
        .send(MarketWorkerMessage::Connection {
            state: FeedConnectionState::Discovering,
            message: "Connecting to market data".to_string(),
            transport_rtt_nanos: None,
        });
    market.register_consumer(client_id, record.workspace_id, record.endpoint.consumer_id)?;
    if record.endpoint.resource_class != ConsumerResourceClass::Foreground {
        market.set_resource_class(
            client_id,
            record.endpoint.consumer_id,
            record.endpoint.resource_class,
        )?;
    }

    // Restored provider metadata (price increment, session) is never trusted:
    // the live catalog re-resolves it before any market data reaches the chart.
    if matches!(
        record.product.provider.as_str(),
        "hyperliquid" | "tastytrade"
    ) {
        let requested = record.product.clone();
        record.startup_resolution = Some(StartupResolution::Searching(requested.clone()));
        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Loading,
            message: "Refreshing provider instrument metadata".to_string(),
        });
        return market.search_provider_instruments(
            client_id,
            SearchProviderInstruments {
                consumer_id: record.endpoint.consumer_id,
                search_generation: STARTUP_CATALOG_COMMAND_GENERATION,
                provider: requested.provider.clone(),
                query: if requested.provider == "tastytrade" {
                    requested.display_symbol
                } else {
                    requested.provider_symbol
                },
                maximum_results: 32,
            },
        );
    }
    market.install_provider_instrument(&record.product)?;
    begin_endpoint_demand(market, client_id, record)
}

fn begin_endpoint_demand(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) -> Result<(), String> {
    let provider_name = provider_display_name(record.product.provider.as_str());
    let series = series_key(&record.product, record.interval)?;
    let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
        state: ChartState::Loading,
        message: format!("Loading {provider_name} history"),
    });
    if let Err(error) = market.set_demand(
        client_id,
        record.endpoint.consumer_id,
        record.endpoint.active_generation,
        &series,
        market.available_streams(
            &record.product.provider,
            chart_streams(record.endpoint.depth_visible),
        )?,
    ) {
        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
        return Ok(());
    }

    // Catalog population is ancillary to an already resolved market demand.
    // A picker refresh failure must not block or tear down the price stream.
    let _ = market.search_provider_instruments(
        client_id,
        SearchProviderInstruments {
            consumer_id: record.endpoint.consumer_id,
            search_generation: 1,
            provider: record.product.provider.clone(),
            query: String::new(),
            maximum_results: u32::try_from(
                crate::desktop::rithmic_shell::MAXIMUM_RITHMIC_SYMBOL_RESULTS,
            )
            .unwrap_or(u32::MAX),
        },
    );
    Ok(())
}

pub(super) fn handle_startup_catalog_event(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
    event: &MarketRuntimeEvent,
) -> Result<bool, String> {
    let resolution = match &record.startup_resolution {
        Some(StartupResolution::Searching(requested)) => Some((true, requested.clone())),
        Some(StartupResolution::Selecting(requested)) => Some((false, requested.clone())),
        None => None,
    };

    let Some((searching, requested)) = resolution else {
        return Ok(is_retired_startup_catalog_event(event));
    };

    if searching {
        match event {
            MarketRuntimeEvent::ProviderInstrumentSearchResult(result)
                if result.consumer_id == record.endpoint.consumer_id
                    && result.provider == requested.provider
                    && result.search_generation == STARTUP_CATALOG_COMMAND_GENERATION =>
            {
                let candidate =
                    startup_catalog_candidate(result.instruments.as_slice(), &requested)?;
                market.select_provider_instrument(
                    client_id,
                    SelectProviderInstrument {
                        consumer_id: record.endpoint.consumer_id,
                        selection_generation: STARTUP_CATALOG_COMMAND_GENERATION,
                        search_generation: STARTUP_CATALOG_COMMAND_GENERATION,
                        provider: requested.provider.clone(),
                        symbol: candidate.symbol,
                        exchange: candidate.exchange,
                        entitlement_id: requested.entitlement_id.clone(),
                    },
                )?;
                record.startup_resolution = Some(StartupResolution::Selecting(requested));
                return Ok(true);
            }
            MarketRuntimeEvent::ProviderCatalogRejected(rejection)
                if rejection.consumer_id == record.endpoint.consumer_id
                    && rejection.provider == requested.provider
                    && rejection.command_generation == STARTUP_CATALOG_COMMAND_GENERATION =>
            {
                record.startup_resolution = None;
                return Err("Provider startup catalog refresh was rejected".to_string());
            }
            _ => return Ok(false),
        }
    }

    match event {
        MarketRuntimeEvent::ProviderInstrumentSelection(selection)
            if selection.consumer_id.0.get() == record.endpoint.consumer_id
                && selection.command_generation == STARTUP_CATALOG_COMMAND_GENERATION =>
        {
            let instrument = &selection.instrument;
            if instrument.provider != requested.provider
                || instrument.provider_symbol != requested.provider_symbol
                || instrument.entitlement_id != requested.entitlement_id
            {
                record.startup_resolution = None;
                return Err("Provider startup selection identity mismatched".to_string());
            }
            aeris_desktop::trading::register_provider_instrument_if_running(instrument)?;
            record.product.clone_from(instrument);
            record.startup_resolution = None;
            let _ = record
                .endpoint
                .messages
                .send(MarketWorkerMessage::ProviderCatalog(
                    aeris_desktop::market_worker::ProviderCatalogEvent::StartupInstrumentResolved(
                        instrument.clone(),
                    ),
                ));
            begin_endpoint_demand(market, client_id, record)?;
            Ok(true)
        }
        MarketRuntimeEvent::ProviderCatalogRejected(rejection)
            if rejection.consumer_id == record.endpoint.consumer_id
                && rejection.provider == requested.provider
                && rejection.command_generation == STARTUP_CATALOG_COMMAND_GENERATION =>
        {
            record.startup_resolution = None;
            Err("Provider startup selection was rejected".to_string())
        }
        _ => Ok(false),
    }
}

fn startup_catalog_candidate(
    candidates: &[ProviderInstrumentSummary],
    requested: &InstallProviderInstrument,
) -> Result<ProviderInstrumentSummary, String> {
    let symbol = if requested.provider == "tastytrade" {
        &requested.display_symbol
    } else {
        &requested.provider_symbol
    };
    candidates
        .iter()
        .find(|candidate| candidate.symbol == *symbol && candidate.exchange == requested.venue_id)
        .or_else(|| {
            candidates
                .iter()
                .find(|candidate| candidate.symbol == *symbol)
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "Provider catalog no longer contains {}",
                requested.display_symbol
            )
        })
}

fn is_retired_startup_catalog_event(event: &MarketRuntimeEvent) -> bool {
    match event {
        MarketRuntimeEvent::ProviderInstrumentSearchResult(result) => {
            result.search_generation == STARTUP_CATALOG_COMMAND_GENERATION
        }
        MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
            selection.command_generation == STARTUP_CATALOG_COMMAND_GENERATION
        }
        MarketRuntimeEvent::ProviderCatalogRejected(rejection) => {
            rejection.command_generation == STARTUP_CATALOG_COMMAND_GENERATION
        }
        _ => false,
    }
}
/// Records the desired order-book depth visibility and applies it to installed demand.
///
/// Visibility is endpoint intent, so it is retained even when no series demand
/// exists yet: while the startup instrument is still resolving against the live
/// catalog, `begin_endpoint_demand` installs demand with the retained streams.
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
    let EndpointRecord {
        product,
        interval,
        endpoint,
        startup_resolution,
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
        MarketWorkerCommand::EngineSelect(request) => process_engine_select(
            market,
            client_id,
            product,
            interval,
            endpoint,
            startup_resolution,
            &request,
        ),
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
            startup_resolution.is_some(),
            visible,
        ),
        MarketWorkerCommand::ResourceClass(resource_class) => {
            set_resource_class(market, client_id, endpoint, resource_class)
        }
        MarketWorkerCommand::ReplacePriceAlerts(alerts) => {
            replace_price_alerts(market, client_id, endpoint, &alerts)
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

fn process_engine_select(
    market: &MarketService,
    client_id: u64,
    product: &mut InstallProviderInstrument,
    interval: &mut aeris_market_data::ChartInterval,
    endpoint: &mut WorkerEndpoint,
    startup_resolution: &mut Option<StartupResolution>,
    request: &aeris_desktop::market_worker::EngineSelectionRequest,
) -> Result<(), String> {
    *startup_resolution = None;
    let series = match series_key(&request.product, request.interval) {
        Ok(series) => series,
        Err(error) => {
            let _ = endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: error,
            });
            return Ok(());
        }
    };
    cancel_pending_recovery(
        endpoint,
        "market recovery was superseded by a new market selection",
    )?;
    let _ = endpoint
        .messages
        .send(MarketWorkerMessage::EngineSwitchMarker {
            sequence: request.sequence,
        });
    let _ = endpoint.messages.send(MarketWorkerMessage::State {
        state: ChartState::Loading,
        message: format!(
            "Loading {} history",
            provider_display_name(&request.product.provider)
        ),
    });
    endpoint.live = false;
    endpoint.active_generation = request.sequence;
    product.clone_from(&request.product);
    *interval = request.interval;
    market.set_demand(
        client_id,
        endpoint.consumer_id,
        request.sequence,
        &series,
        market.available_streams(
            &request.product.provider,
            chart_streams(endpoint.depth_visible),
        )?,
    )
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
    match market.register_study(client_id, endpoint.consumer_id, request.registration) {
        Ok(study_id) => endpoint
            .messages
            .send(MarketWorkerMessage::StudyRegistered {
                request_sequence: request.sequence,
                study_id,
            })
            .map_err(|error| error.to_string()),
        Err(message) => endpoint
            .messages
            .send(MarketWorkerMessage::StudyRegistrationFailed {
                request_sequence: request.sequence,
                message,
            })
            .map_err(|error| error.to_string()),
    }
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
    alerts: &[MarketPriceAlert],
) -> Result<(), String> {
    match market.replace_price_alerts(client_id, endpoint.consumer_id, alerts) {
        Ok(()) => Ok(()),
        Err(error) => endpoint
            .messages
            .send(MarketWorkerMessage::PriceAlertSyncFailed(error))
            .map_err(|error| error.to_string()),
    }
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
