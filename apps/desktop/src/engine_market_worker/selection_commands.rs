//! Selection commands.

use super::{
    ChartState, ConsumerResourceClass, EndpointRecord, FeedConnectionState,
    InstallProviderInstrument, MarketRuntimeEvent, MarketService, MarketWorkerCommand,
    MarketWorkerMessage, ProviderInstrumentSummary, RITHMIC_CATALOG_READY_MESSAGE,
    STARTUP_CATALOG_COMMAND_GENERATION, SearchProviderInstruments, SelectProviderInstrument,
    StartupResolution, WorkerEndpoint, cancel_pending_recovery, chart_streams,
    provider_display_name, retire_endpoint, send_recovery, series_key,
};

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

    match market.install_provider_instrument(&record.product) {
        Ok(()) => begin_endpoint_demand(market, client_id, record),
        Err(error) if refreshable_hyperliquid_startup_error(&record.product, &error) => {
            let requested = record.product.clone();
            record.startup_resolution = Some(StartupResolution::Searching(requested.clone()));
            let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Loading,
                message: "Refreshing Hyperliquid instrument metadata".to_string(),
            });
            market.search_provider_instruments(
                client_id,
                SearchProviderInstruments {
                    consumer_id: record.endpoint.consumer_id,
                    search_generation: STARTUP_CATALOG_COMMAND_GENERATION,
                    provider: requested.provider,
                    query: requested.provider_symbol,
                    maximum_results: 32,
                },
            )
        }
        Err(error) => Err(error),
    }
}

fn refreshable_hyperliquid_startup_error(
    requested: &InstallProviderInstrument,
    error: &str,
) -> bool {
    requested.provider == "hyperliquid"
        && matches!(
            error,
            "provider instrument session is stale"
                | "provider instrument selection is stale"
                | "provider instrument selection conflicts"
        )
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
        chart_streams(record.endpoint.depth_visible),
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
                return Err("Hyperliquid startup catalog refresh was rejected".to_string());
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
                return Err("Hyperliquid startup selection identity mismatched".to_string());
            }
            record.product.clone_from(instrument);
            record.startup_resolution = None;
            begin_endpoint_demand(market, client_id, record)?;
            Ok(true)
        }
        MarketRuntimeEvent::ProviderCatalogRejected(rejection)
            if rejection.consumer_id == record.endpoint.consumer_id
                && rejection.provider == requested.provider
                && rejection.command_generation == STARTUP_CATALOG_COMMAND_GENERATION =>
        {
            record.startup_resolution = None;
            Err("Hyperliquid startup selection was rejected".to_string())
        }
        _ => Ok(false),
    }
}

fn startup_catalog_candidate(
    candidates: &[ProviderInstrumentSummary],
    requested: &InstallProviderInstrument,
) -> Result<ProviderInstrumentSummary, String> {
    candidates
        .iter()
        .find(|candidate| {
            candidate.symbol == requested.provider_symbol
                && candidate.exchange == requested.venue_id
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|candidate| candidate.symbol == requested.provider_symbol)
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "Hyperliquid catalog no longer contains {}",
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
            if request.provider != product.provider {
                return Err("unsupported provider catalog command".to_string());
            }
            request.consumer_id = endpoint.consumer_id;
            market.search_provider_instruments(client_id, request)
        }
        MarketWorkerCommand::ProviderSelect(mut request) => {
            if request.provider != product.provider {
                return Err("unsupported provider catalog command".to_string());
            }
            request.consumer_id = endpoint.consumer_id;
            market.select_provider_instrument(client_id, request)
        }
        MarketWorkerCommand::EngineSelect(request) => {
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
                chart_streams(endpoint.depth_visible),
            )
        }
        MarketWorkerCommand::Recovery(command) => {
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
        MarketWorkerCommand::DepthVisible(visible) => {
            endpoint.depth_visible = visible;
            if endpoint.active_generation == 0 {
                return Ok(());
            }
            market.set_streams(
                client_id,
                endpoint.consumer_id,
                endpoint.active_generation,
                chart_streams(visible),
            )
        }
        MarketWorkerCommand::ResourceClass(resource_class) => {
            set_resource_class(market, client_id, endpoint, resource_class)
        }
        MarketWorkerCommand::Shutdown => {
            retire_endpoint(market, client_id, endpoint);
            Ok(())
        }
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
