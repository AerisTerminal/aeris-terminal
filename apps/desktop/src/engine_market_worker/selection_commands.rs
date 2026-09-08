//! Selection commands.

use super::{
    ChartInterval, ChartState, ConsumerResourceClass, EndpointRecord, EngineSupervisor,
    FeedConnectionState, InstallProviderInstrument, Instant, MarketWorkerCommand,
    MarketWorkerMessage, ProviderInstrumentSummary, STARTUP_CATALOG_COMMAND_GENERATION,
    STARTUP_CATALOG_RESOLUTION_TIMEOUT, SearchProviderInstruments, SelectProviderInstrument,
    WorkerEndpoint, cancel_pending_recovery, envelope, provider_display_name, retire_endpoint,
    send_recovery, series_key,
};

pub(super) fn initialize_endpoint(
    client: &mut EngineSupervisor,
    workspace_id: u64,
    product: &mut InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
) -> Result<(), String> {
    let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
        state: FeedConnectionState::Discovering,
        message: "Connecting to the resident market engine".to_string(),
        transport_rtt_nanos: None,
    });
    client.register_consumer(workspace_id, endpoint.consumer_id)?;
    let canonical = install_or_resolve_startup_instrument(client, endpoint.consumer_id, product)?;
    product.clone_from(&canonical);
    let provider_name = provider_display_name(product.provider.as_str());
    let series = series_key(product, interval)?;
    let _ = endpoint.messages.send(MarketWorkerMessage::State {
        state: ChartState::Loading,
        message: format!("Loading {provider_name} history through the resident engine"),
    });
    if let Err(error) =
        client.set_series_demand(endpoint.consumer_id, endpoint.active_generation, series)
    {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
    client.search_provider_instruments(SearchProviderInstruments {
        consumer_id: endpoint.consumer_id,
        search_generation: 1,
        provider: product.provider.clone(),
        query: String::new(),
        maximum_results: u32::try_from(
            crate::desktop::rithmic_shell::MAXIMUM_RITHMIC_SYMBOL_RESULTS,
        )
        .unwrap_or(u32::MAX),
    })?;
    Ok(())
}

fn install_or_resolve_startup_instrument(
    client: &mut EngineSupervisor,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
) -> Result<InstallProviderInstrument, String> {
    match client.install_provider_instrument(requested.clone()) {
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

    resolve_hyperliquid_startup_instrument(client, consumer_id, requested)
}

fn resolve_hyperliquid_startup_instrument(
    client: &mut EngineSupervisor,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
) -> Result<InstallProviderInstrument, String> {
    let command_generation = STARTUP_CATALOG_COMMAND_GENERATION;
    let deadline = Instant::now()
        .checked_add(STARTUP_CATALOG_RESOLUTION_TIMEOUT)
        .ok_or_else(|| "Hyperliquid startup catalog deadline overflowed".to_string())?;
    let summary = search_hyperliquid_startup_instrument(
        client,
        consumer_id,
        requested,
        command_generation,
        deadline,
    )?;
    select_hyperliquid_startup_instrument(
        client,
        consumer_id,
        requested,
        summary,
        command_generation,
        deadline,
    )
}

fn search_hyperliquid_startup_instrument(
    client: &mut EngineSupervisor,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
    command_generation: u64,
    deadline: Instant,
) -> Result<ProviderInstrumentSummary, String> {
    client.search_provider_instruments(SearchProviderInstruments {
        consumer_id,
        search_generation: command_generation,
        provider: requested.provider.clone(),
        query: requested.provider_symbol.clone(),
        maximum_results: 32,
    })?;
    loop {
        let event = client.receive_market_event_for_until(consumer_id, deadline)?;
        if event.reconnected {
            return Err(
                "resident engine restarted during Hyperliquid startup catalog search".to_string(),
            );
        }
        let Some(event) = event.event else {
            if Instant::now() >= deadline {
                return Err("Hyperliquid startup catalog search timed out".to_string());
            }
            continue;
        };
        match event {
            envelope::Payload::ProviderInstrumentSearchResult(result)
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
            envelope::Payload::ProviderCatalogRejected(rejection)
                if rejection.consumer_id == consumer_id
                    && rejection.command_generation == command_generation =>
            {
                return Err("Hyperliquid startup catalog search was rejected".to_string());
            }
            envelope::Payload::Fault(fault) => return Err(fault.redacted_detail),
            _ => {}
        }
    }
}

fn select_hyperliquid_startup_instrument(
    client: &mut EngineSupervisor,
    consumer_id: u64,
    requested: &InstallProviderInstrument,
    summary: ProviderInstrumentSummary,
    command_generation: u64,
    deadline: Instant,
) -> Result<InstallProviderInstrument, String> {
    client.select_provider_instrument(SelectProviderInstrument {
        consumer_id,
        selection_generation: command_generation,
        search_generation: command_generation,
        provider: requested.provider.clone(),
        symbol: summary.symbol,
        exchange: summary.exchange,
        entitlement_id: requested.entitlement_id.clone(),
    })?;

    loop {
        let event = client.receive_market_event_for_until(consumer_id, deadline)?;
        if event.reconnected {
            return Err(
                "resident engine restarted during Hyperliquid startup selection".to_string(),
            );
        }
        let Some(event) = event.event else {
            if Instant::now() >= deadline {
                return Err("Hyperliquid startup selection timed out".to_string());
            }
            continue;
        };
        match event {
            envelope::Payload::ProviderInstrumentSelection(selection)
                if selection.consumer_id == consumer_id
                    && selection.command_generation == command_generation =>
            {
                let instrument = selection.instrument.ok_or_else(|| {
                    "Hyperliquid startup selection omitted canonical identity".to_string()
                })?;
                if instrument.provider != requested.provider
                    || instrument.provider_symbol != requested.provider_symbol
                    || instrument.entitlement_id != requested.entitlement_id
                {
                    return Err("Hyperliquid startup selection identity mismatched".to_string());
                }
                return Ok(instrument);
            }
            envelope::Payload::ProviderCatalogRejected(rejection)
                if rejection.consumer_id == consumer_id
                    && rejection.command_generation == command_generation =>
            {
                return Err("Hyperliquid startup selection was rejected".to_string());
            }
            envelope::Payload::Fault(fault) => return Err(fault.redacted_detail),
            _ => {}
        }
    }
}

pub(super) fn process_command(
    client: &mut EngineSupervisor,
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
            client.search_provider_instruments(request)
        }
        MarketWorkerCommand::ProviderSelect(mut request) => {
            if request.provider != product.provider {
                return Err("unsupported provider catalog command".to_string());
            }
            request.consumer_id = endpoint.consumer_id;
            client.select_provider_instrument(request)
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
                    "Loading {} history through the resident engine",
                    provider_display_name(&request.product.provider)
                ),
            });
            endpoint.publication = None;
            endpoint.live = false;
            endpoint.active_generation = request.sequence;
            product.clone_from(&request.product);
            *interval = request.interval;
            client.set_series_demand(endpoint.consumer_id, request.sequence, series)
        }
        MarketWorkerCommand::Recovery(command) => {
            send_recovery(client, product, *interval, endpoint, command)
        }
        MarketWorkerCommand::ChartViewport(viewport) => {
            if viewport.selection_generation > 0 {
                client.set_market_viewport(
                    endpoint.consumer_id,
                    viewport.selection_generation,
                    viewport.start_unix_nanos,
                    viewport.end_unix_nanos,
                )?;
            }
            Ok(())
        }
        MarketWorkerCommand::ResourceClass(resource_class) => {
            set_resource_class(client, endpoint, resource_class)
        }
        MarketWorkerCommand::Shutdown => {
            retire_endpoint(client, endpoint);
            Ok(())
        }
        MarketWorkerCommand::EngineSeries(_) => {
            Err("Rithmic commands cannot enter the Rithmic engine client".to_string())
        }
    }
}

pub(super) fn set_resource_class(
    client: &mut EngineSupervisor,
    endpoint: &mut WorkerEndpoint,
    resource_class: ConsumerResourceClass,
) -> Result<(), String> {
    client.set_market_resource_class(endpoint.consumer_id, resource_class)?;
    endpoint.resource_class = resource_class;
    Ok(())
}
