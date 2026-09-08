//! Selection commands.

use super::{
    ChartInterval, ChartState, ConsumerResourceClass, EVENT_WAIT, EndpointRecord,
    FeedConnectionState, InstallProviderInstrument, Instant, MarketRuntimeEvent, MarketService,
    MarketWorkerCommand, MarketWorkerMessage, ProviderInstrumentSummary,
    RITHMIC_CATALOG_READY_MESSAGE, STARTUP_CATALOG_COMMAND_GENERATION,
    STARTUP_CATALOG_RESOLUTION_TIMEOUT, SearchProviderInstruments, SelectProviderInstrument,
    WorkerEndpoint, cancel_pending_recovery, chart_streams, provider_display_name, retire_endpoint,
    send_recovery, series_key,
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
    workspace_id: u64,
    product: &mut InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
) -> Result<(), String> {
    let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
        state: FeedConnectionState::Discovering,
        message: "Connecting to market data".to_string(),
        transport_rtt_nanos: None,
    });
    market.register_consumer(client_id, workspace_id, endpoint.consumer_id)?;
    let canonical =
        install_or_resolve_startup_instrument(market, client_id, endpoint.consumer_id, product)?;
    product.clone_from(&canonical);
    let provider_name = provider_display_name(product.provider.as_str());
    let series = series_key(product, interval)?;
    let _ = endpoint.messages.send(MarketWorkerMessage::State {
        state: ChartState::Loading,
        message: format!("Loading {provider_name} history"),
    });
    if let Err(error) = market.set_demand(
        client_id,
        endpoint.consumer_id,
        endpoint.active_generation,
        &series,
        chart_streams(endpoint.depth_visible),
    ) {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
    market.search_provider_instruments(
        client_id,
        SearchProviderInstruments {
            consumer_id: endpoint.consumer_id,
            search_generation: 1,
            provider: product.provider.clone(),
            query: String::new(),
            maximum_results: u32::try_from(
                crate::desktop::rithmic_shell::MAXIMUM_RITHMIC_SYMBOL_RESULTS,
            )
            .unwrap_or(u32::MAX),
        },
    )?;
    Ok(())
}

fn install_or_resolve_startup_instrument(
    market: &MarketService,
    client_id: u64,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
) -> Result<InstallProviderInstrument, String> {
    match market.install_provider_instrument(requested) {
        Ok(()) => return Ok(requested.clone()),
        Err(error)
            if requested.provider == "hyperliquid"
                && matches!(
                    error.as_str(),
                    "provider instrument session is stale"
                        | "provider instrument selection is stale"
                        | "provider instrument selection conflicts"
                ) => {}
        Err(error) => return Err(error),
    }

    resolve_hyperliquid_startup_instrument(market, client_id, consumer_id, requested)
}

fn resolve_hyperliquid_startup_instrument(
    market: &MarketService,
    client_id: u64,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
) -> Result<InstallProviderInstrument, String> {
    let command_generation = STARTUP_CATALOG_COMMAND_GENERATION;
    let deadline = Instant::now()
        .checked_add(STARTUP_CATALOG_RESOLUTION_TIMEOUT)
        .ok_or_else(|| "Hyperliquid startup catalog deadline overflowed".to_string())?;
    let summary = search_hyperliquid_startup_instrument(
        market,
        client_id,
        consumer_id,
        requested,
        command_generation,
        deadline,
    )?;
    select_hyperliquid_startup_instrument(
        market,
        client_id,
        consumer_id,
        requested,
        summary,
        command_generation,
        deadline,
    )
}

fn search_hyperliquid_startup_instrument(
    market: &MarketService,
    client_id: u64,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
    command_generation: u64,
    deadline: Instant,
) -> Result<ProviderInstrumentSummary, String> {
    market.search_provider_instruments(
        client_id,
        SearchProviderInstruments {
            consumer_id,
            search_generation: command_generation,
            provider: requested.provider.clone(),
            query: requested.provider_symbol.clone(),
            maximum_results: 32,
        },
    )?;
    loop {
        let Some(event) = poll_market_event_until(market, client_id, consumer_id, deadline)? else {
            if Instant::now() >= deadline {
                return Err("Hyperliquid startup catalog search timed out".to_string());
            }
            continue;
        };
        match event {
            MarketRuntimeEvent::ProviderInstrumentSearchResult(result)
                if result.consumer_id == consumer_id
                    && result.search_generation == command_generation =>
            {
                let index = result
                    .instruments
                    .iter()
                    .position(|candidate| {
                        candidate.symbol == requested.provider_symbol
                            && candidate.exchange == requested.venue_id
                    })
                    .or_else(|| {
                        result
                            .instruments
                            .iter()
                            .position(|candidate| candidate.symbol == requested.provider_symbol)
                    })
                    .ok_or_else(|| {
                        format!(
                            "Hyperliquid catalog no longer contains {}",
                            requested.display_symbol
                        )
                    })?;
                return result
                    .instruments
                    .into_iter()
                    .nth(index)
                    .ok_or_else(|| "Hyperliquid startup catalog result vanished".to_string());
            }
            MarketRuntimeEvent::ProviderCatalogRejected(rejection)
                if rejection.consumer_id == consumer_id
                    && rejection.command_generation == command_generation =>
            {
                return Err("Hyperliquid startup catalog search was rejected".to_string());
            }
            MarketRuntimeEvent::Fault(fault) => return Err(fault.redacted_detail),
            _ => {}
        }
    }
}

fn select_hyperliquid_startup_instrument(
    market: &MarketService,
    client_id: u64,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
    summary: ProviderInstrumentSummary,
    command_generation: u64,
    deadline: Instant,
) -> Result<InstallProviderInstrument, String> {
    market.select_provider_instrument(
        client_id,
        SelectProviderInstrument {
            consumer_id,
            selection_generation: command_generation,
            search_generation: command_generation,
            provider: requested.provider.clone(),
            symbol: summary.symbol,
            exchange: summary.exchange,
            entitlement_id: requested.entitlement_id.clone(),
        },
    )?;

    loop {
        let Some(event) = poll_market_event_until(market, client_id, consumer_id, deadline)? else {
            if Instant::now() >= deadline {
                return Err("Hyperliquid startup selection timed out".to_string());
            }
            continue;
        };
        match event {
            MarketRuntimeEvent::ProviderInstrumentSelection(selection)
                if selection.consumer_id.0.get() == consumer_id
                    && selection.command_generation == command_generation =>
            {
                let instrument = selection.instrument;
                if instrument.provider != requested.provider
                    || instrument.provider_symbol != requested.provider_symbol
                    || instrument.entitlement_id != requested.entitlement_id
                {
                    return Err("Hyperliquid startup selection identity mismatched".to_string());
                }
                return Ok(instrument);
            }
            MarketRuntimeEvent::ProviderCatalogRejected(rejection)
                if rejection.consumer_id == consumer_id
                    && rejection.command_generation == command_generation =>
            {
                return Err("Hyperliquid startup selection was rejected".to_string());
            }
            MarketRuntimeEvent::Fault(fault) => return Err(fault.redacted_detail),
            _ => {}
        }
    }
}

fn poll_market_event_until(
    market: &MarketService,
    client_id: u64,
    consumer_id: u64,
    deadline: Instant,
) -> Result<Option<MarketRuntimeEvent>, String> {
    loop {
        if let Some(event) = market.poll_event(client_id, consumer_id)? {
            return Ok(Some(event));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        std::thread::sleep(EVENT_WAIT.min(remaining));
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
