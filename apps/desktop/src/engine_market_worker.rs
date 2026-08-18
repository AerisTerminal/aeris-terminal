//! Desktop-side client for engine-owned Coinbase historical and realtime bars.

use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{Arc, atomic::AtomicU64, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, Provenanced,
    ReplayProvenance, ReplayRecoveryCommand, ReplaySnapshot, ReplayStreamUpdate, ReplayTailUpdate,
};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_local_engine_client::{
    EngineClient, connect_or_start_engine, sibling_engine_executable,
};
use axiusflow_local_engine_protocol::{
    DemandError, EngineFaultCode, InstallProviderInstrument, ProviderConnectionState,
    ProviderState, SeriesCadence, SeriesKey, SeriesLoadState, SeriesSnapshot, SeriesUpdate,
    envelope,
};
use axiusflow_market_data::{BarDefinition, ChartInterval, MarketBar};
use axiusflow_observability::FeedConnectionState;

use crate::resident_market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketPublicationGeneration,
    MarketWorkerBootstrap, MarketWorkerCommand, MarketWorkerMessage, MarketWorkerPublication,
    MarketWorkerSender, MarketWorkerStartup, market_worker_channel,
};

const DEFAULT_WORKSPACE_ID: u64 = 1;
const INITIAL_GENERATION: u64 = 1;
const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 32;
const MODEL_CAPACITY: usize = 350;
const SUBSCRIPTION_ID: &str = "desktop_engine_coinbase_bars";
const WORKER_LABEL: &str = "Coinbase engine - history and realtime IPC";
const POLL_INTERVAL: Duration = Duration::from_millis(16);

pub(super) fn start() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let products = coinbase_products();
    let product = products
        .first()
        .cloned()
        .ok_or_else(|| "Coinbase engine product catalog is empty".to_string())?;
    let mut workers = start_group(vec![(DEFAULT_WORKSPACE_ID, product)])?;
    workers
        .pop()
        .ok_or_else(|| "Coinbase engine worker group is empty".to_string())
}

pub(super) fn start_multi_chart() -> Result<Vec<(MarketWorkerStartup, MarketDataWorker)>, String> {
    let products = coinbase_products();
    let btc = products
        .first()
        .cloned()
        .ok_or_else(|| "Coinbase engine product catalog is empty".to_string())?;
    let eth = products
        .get(1)
        .cloned()
        .ok_or_else(|| "Coinbase engine ETH product is unavailable".to_string())?;
    start_group(vec![(1, btc), (2, eth)])
}

struct WorkerEndpoint {
    consumer_id: u64,
    messages: MarketWorkerSender,
    commands: mpsc::Receiver<MarketWorkerCommand>,
    shutdown: mpsc::SyncSender<()>,
    model: MarketBarClientModel,
    publication: Option<MarketPublicationGeneration>,
    active_generation: u64,
    active: bool,
}

fn start_group(
    configurations: Vec<(u64, InstallProviderInstrument)>,
) -> Result<Vec<(MarketWorkerStartup, MarketDataWorker)>, String> {
    let client_id = random_identity()?;
    let mut workers = Vec::with_capacity(configurations.len());
    let mut endpoints = Vec::with_capacity(configurations.len());
    for (workspace_id, product) in configurations {
        let consumer_id = random_identity()?;
        let startup = MarketWorkerStartup::Loading(Box::new(
            axiusflow_desktop::market_worker::CoinbaseWorkerStartup {
                coinbase_product: product.clone(),
                subscription_id: SUBSCRIPTION_ID.to_string(),
                worker_label: WORKER_LABEL.to_string(),
            },
        ));
        let (message_tx, message_rx) =
            market_worker_channel(NonZeroUsize::new(MESSAGE_CAPACITY).unwrap_or(NonZeroUsize::MIN));
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let selection_sequence = Arc::new(AtomicU64::new(INITIAL_GENERATION));
        workers.push((
            startup,
            MarketDataWorker::from_channels(
                command_tx,
                message_rx,
                shutdown_rx,
                None,
                Some(selection_sequence),
            ),
        ));
        endpoints.push((
            workspace_id,
            product,
            WorkerEndpoint {
                consumer_id,
                messages: message_tx,
                commands: command_rx,
                shutdown: shutdown_tx,
                model: empty_model(),
                publication: None,
                active_generation: INITIAL_GENERATION,
                active: true,
            },
        ));
    }
    thread::Builder::new()
        .name("axiusflow-engine-market-client".to_string())
        .spawn(move || {
            if let Err(error) = run_workers(client_id, &mut endpoints) {
                for (_, _, endpoint) in &endpoints {
                    let _ = endpoint.messages.send(MarketWorkerMessage::State {
                        state: ChartState::Error,
                        message: error.clone(),
                    });
                }
            }
            for (_, _, endpoint) in endpoints {
                let _ = endpoint.shutdown.try_send(());
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(workers)
}

fn run_workers(
    client_id: u64,
    endpoints: &mut [(u64, InstallProviderInstrument, WorkerEndpoint)],
) -> Result<(), String> {
    for (_, _, endpoint) in endpoints.iter() {
        let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
            state: FeedConnectionState::Discovering,
            message: "Connecting to the resident market engine".to_string(),
        });
    }
    let executable = sibling_engine_executable()?;
    let mut client = connect_or_start_engine(&executable)?;
    client.attach_client(client_id)?;
    let result = run_attached_workers(&mut client, client_id, endpoints);
    for (_, _, endpoint) in endpoints
        .iter_mut()
        .filter(|(_, _, endpoint)| endpoint.active)
    {
        let _ = client.remove_market_consumer(endpoint.consumer_id);
        endpoint.active = false;
        let _ = endpoint.shutdown.try_send(());
    }
    let detach_result = client.detach_client(client_id);
    result.and(detach_result)
}

fn run_attached_workers(
    client: &mut EngineClient,
    client_id: u64,
    endpoints: &mut [(u64, InstallProviderInstrument, WorkerEndpoint)],
) -> Result<(), String> {
    for (workspace_id, product, endpoint) in endpoints.iter_mut() {
        client.register_consumer(client_id, *workspace_id, endpoint.consumer_id)?;
        let _ =
            endpoint.messages.send(MarketWorkerMessage::CoinbaseCatalog(
                Ok(coinbase_products()),
            ));
        match request_snapshot(
            client,
            endpoint.consumer_id,
            INITIAL_GENERATION,
            series_key(product, ChartInterval::Minute1)?,
            &mut endpoint.model,
            &endpoint.messages,
        ) {
            Ok((snapshot, generation)) => {
                let publication = MarketPublicationGeneration::from_generation(&generation);
                send_publication(
                    &endpoint.messages,
                    ReplayStreamUpdate::Snapshot(snapshot),
                    publication,
                )?;
                endpoint.publication = Some(publication);
                let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
                    state: FeedConnectionState::Discovering,
                    message: "Historical bars are visible; Coinbase realtime is connecting"
                        .to_string(),
                });
            }
            Err(error) => {
                let _ = endpoint.messages.send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: error,
                });
            }
        }
    }

    while endpoints.iter().any(|(_, _, endpoint)| endpoint.active) {
        for (_, _, endpoint) in endpoints
            .iter_mut()
            .filter(|(_, _, endpoint)| endpoint.active)
        {
            match endpoint.commands.try_recv() {
                Ok(command) => {
                    if let Err(error) = process_command(client, endpoint, command) {
                        let _ = endpoint.messages.send(MarketWorkerMessage::State {
                            state: ChartState::Error,
                            message: error,
                        });
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    retire_endpoint(client, endpoint);
                    continue;
                }
            }
            if endpoint.active
                && let Some(event) = client.poll_market_event(endpoint.consumer_id)?
                && let Err(error) = apply_polled_event(
                    event,
                    endpoint.consumer_id,
                    endpoint.active_generation,
                    &mut endpoint.model,
                    &mut endpoint.publication,
                    &endpoint.messages,
                )
            {
                let _ = endpoint.messages.send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: error,
                });
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

fn process_command(
    client: &mut EngineClient,
    endpoint: &mut WorkerEndpoint,
    command: MarketWorkerCommand,
) -> Result<(), String> {
    match command {
        MarketWorkerCommand::CoinbaseSelect(request) => {
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
            let _ = endpoint
                .messages
                .send(MarketWorkerMessage::CoinbaseSwitchMarker {
                    sequence: request.sequence,
                });
            let _ = endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Loading,
                message: "Loading Coinbase history through the resident engine".to_string(),
            });
            endpoint.model = empty_model();
            endpoint.publication = None;
            endpoint.active_generation = request.sequence;
            client.set_series_demand(endpoint.consumer_id, request.sequence, series)
        }
        MarketWorkerCommand::Recovery(command) => send_recovery(
            client,
            endpoint.consumer_id,
            endpoint.active_generation,
            command,
            &mut endpoint.model,
            &mut endpoint.publication,
            &endpoint.messages,
        ),
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
        MarketWorkerCommand::Shutdown => {
            retire_endpoint(client, endpoint);
            Ok(())
        }
        MarketWorkerCommand::ProviderSearch(_)
        | MarketWorkerCommand::ProviderSelect(_)
        | MarketWorkerCommand::EngineSeries(_) => {
            Err("Rithmic commands cannot enter the Coinbase engine client".to_string())
        }
    }
}

fn retire_endpoint(client: &mut EngineClient, endpoint: &mut WorkerEndpoint) {
    let _ = client.remove_market_consumer(endpoint.consumer_id);
    endpoint.active = false;
    let _ = endpoint.shutdown.try_send(());
}

fn apply_polled_event(
    event: envelope::Payload,
    consumer_id: u64,
    active_generation: u64,
    model: &mut MarketBarClientModel,
    publication: &mut Option<MarketPublicationGeneration>,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    match event {
        envelope::Payload::SeriesSnapshot(snapshot) => {
            if snapshot.consumer_id != consumer_id || snapshot.generation != active_generation {
                return Err("engine realtime snapshot identity mismatched".to_string());
            }
            let replay = replay_snapshot(&snapshot)?;
            let outcome = model
                .apply_update(ReplayStreamUpdate::Snapshot(replay.clone()))
                .map_err(|error| error.to_string())?;
            let MarketBarModelOutcome::Published(generation) = outcome else {
                return Err("engine realtime snapshot was not publishable".to_string());
            };
            let status = MarketPublicationGeneration::from_generation(&generation);
            *publication = Some(status);
            send_publication(messages, ReplayStreamUpdate::Snapshot(replay), status)
        }
        envelope::Payload::SeriesUpdate(update) => {
            if update.consumer_id != consumer_id || update.generation != active_generation {
                return Err("engine realtime update identity mismatched".to_string());
            }
            let tail = replay_tail_update(&update)?;
            let current = publication.ok_or_else(|| {
                "engine realtime update arrived before a covering snapshot".to_string()
            })?;
            let (first_sequence, last_sequence) = current.sequence_range();
            let sequence = tail.item().value().source_sequence;
            if sequence < last_sequence || sequence > last_sequence.saturating_add(1) {
                return Err("engine realtime update sequence was not contiguous".to_string());
            }
            let appended = usize::from(sequence > last_sequence);
            let retained_items = current
                .retained_items()
                .saturating_add(appended)
                .min(MODEL_CAPACITY);
            let first_sequence = if appended > 0 && retained_items == MODEL_CAPACITY {
                first_sequence.saturating_add(1)
            } else {
                first_sequence
            };
            let status = MarketPublicationGeneration::from_tail(
                update.publication_generation,
                retained_items,
                first_sequence,
                sequence.max(last_sequence),
            );
            *publication = Some(status);
            send_publication(messages, ReplayStreamUpdate::Tail(tail), status)
        }
        envelope::Payload::ProviderState(state) => apply_provider_state(&state, messages),
        envelope::Payload::SeriesState(state) => {
            if state.consumer_id != consumer_id || state.generation != active_generation {
                return Err("engine realtime state identity mismatched".to_string());
            }
            match SeriesLoadState::try_from(state.state)
                .map_err(|_| "engine returned an invalid realtime state".to_string())?
            {
                SeriesLoadState::Live => messages
                    .send(MarketWorkerMessage::State {
                        state: ChartState::Ready,
                        message: "Coinbase history/live handoff is current".to_string(),
                    })
                    .map_err(|error| error.to_string()),
                SeriesLoadState::Failed => Err(state
                    .detail
                    .unwrap_or_else(|| "Coinbase realtime failed".to_string())),
                SeriesLoadState::Empty
                | SeriesLoadState::Resolving
                | SeriesLoadState::Partial
                | SeriesLoadState::Ready
                | SeriesLoadState::Superseded => Ok(()),
            }
        }
        envelope::Payload::DemandError(error) => Err(demand_error(&error)),
        envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected polled market event".to_string()),
    }
}

fn apply_provider_state(
    state: &ProviderState,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if state.provider != "coinbase" {
        return Err("engine provider state identity mismatched".to_string());
    }
    let provider_state = ProviderConnectionState::try_from(state.state)
        .map_err(|_| "engine returned an invalid provider state".to_string())?;
    let (connection, detail) = match provider_state {
        ProviderConnectionState::Disconnected => (
            FeedConnectionState::Disconnected,
            "Coinbase realtime is disconnected",
        ),
        ProviderConnectionState::Connecting => (
            FeedConnectionState::Discovering,
            "Coinbase realtime is connecting",
        ),
        ProviderConnectionState::Online => (
            FeedConnectionState::Streaming,
            "Coinbase history and realtime are current",
        ),
        ProviderConnectionState::Recovering => (
            FeedConnectionState::Recovering,
            "Coinbase realtime is recovering; retained history remains visible",
        ),
        ProviderConnectionState::Failed => {
            (FeedConnectionState::Stopped, "Coinbase realtime stopped")
        }
    };
    messages
        .send(MarketWorkerMessage::Connection {
            state: connection,
            message: state.detail.clone().unwrap_or_else(|| detail.to_string()),
        })
        .map_err(|error| error.to_string())
}

fn request_snapshot(
    client: &mut EngineClient,
    consumer_id: u64,
    generation: u64,
    series: SeriesKey,
    model: &mut MarketBarClientModel,
    messages: &MarketWorkerSender,
) -> Result<(ReplaySnapshot, DesktopMarketGeneration), String> {
    client.set_series_demand(consumer_id, generation, series)?;
    let mut accepted = None;
    loop {
        let Some(event) = client.poll_market_event(consumer_id)? else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };
        match event {
            envelope::Payload::SeriesState(state) => {
                let load_state = SeriesLoadState::try_from(state.state)
                    .map_err(|_| "engine returned an invalid series state".to_string())?;
                match load_state {
                    SeriesLoadState::Resolving => {
                        let _ = messages.send(MarketWorkerMessage::State {
                            state: ChartState::Loading,
                            message: "Resident engine is resolving Coinbase history".to_string(),
                        });
                    }
                    SeriesLoadState::Ready | SeriesLoadState::Live => {
                        return accepted.ok_or_else(|| {
                            "engine marked history ready without a covering snapshot".to_string()
                        });
                    }
                    SeriesLoadState::Failed => {
                        return Err(state.detail.unwrap_or_else(|| {
                            "resident engine could not resolve Coinbase history".to_string()
                        }));
                    }
                    SeriesLoadState::Superseded => {
                        return Err("Coinbase history demand was superseded".to_string());
                    }
                    SeriesLoadState::Empty | SeriesLoadState::Partial => {}
                }
            }
            envelope::Payload::ProviderState(state) => {
                apply_provider_state(&state, messages)?;
            }
            envelope::Payload::SeriesSnapshot(snapshot) => {
                let replay = replay_snapshot(&snapshot)?;
                let outcome = model
                    .apply_update(ReplayStreamUpdate::Snapshot(replay.clone()))
                    .map_err(|error| error.to_string())?;
                let MarketBarModelOutcome::Published(generation) = outcome else {
                    return Err("engine snapshot did not publish a client generation".to_string());
                };
                accepted = Some((replay, generation));
            }
            envelope::Payload::DemandError(error) => return Err(demand_error(&error)),
            envelope::Payload::Fault(fault) => return Err(fault.redacted_detail),
            _ => return Err("engine returned an unexpected market response".to_string()),
        }
    }
}

fn send_recovery(
    client: &mut EngineClient,
    consumer_id: u64,
    active_generation: u64,
    command: ReplayRecoveryCommand,
    model: &mut MarketBarClientModel,
    publication: &mut Option<MarketPublicationGeneration>,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let product = coinbase_products()
        .into_iter()
        .next()
        .ok_or_else(|| "Coinbase engine product catalog is empty".to_string())?;
    let result = request_snapshot(
        client,
        consumer_id,
        active_generation,
        series_key(&product, ChartInterval::Minute1)?,
        model,
        messages,
    )
    .map(|(snapshot, generation)| {
        *publication = Some(MarketPublicationGeneration::from_generation(&generation));
        MarketWorkerBootstrap {
            snapshot,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            generation,
            worker_label: WORKER_LABEL.to_string(),
        }
    });
    messages
        .send(MarketWorkerMessage::Recovery {
            request_id: command.request_id,
            result,
        })
        .map_err(|error| error.to_string())
}

fn send_publication(
    messages: &MarketWorkerSender,
    update: ReplayStreamUpdate,
    generation: MarketPublicationGeneration,
) -> Result<(), String> {
    messages
        .send(MarketWorkerMessage::Update(MarketWorkerPublication {
            update,
            generation,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: WORKER_LABEL.to_string(),
            ui_diagnostics: None,
        }))
        .map_err(|error| error.to_string())
}

fn replay_snapshot(snapshot: &SeriesSnapshot) -> Result<ReplaySnapshot, String> {
    let series = snapshot
        .series
        .clone()
        .ok_or_else(|| "engine snapshot has no series identity".to_string())?;
    if series.provider != "coinbase"
        || SeriesCadence::try_from(series.cadence) != Ok(SeriesCadence::FixedSeconds)
        || !coinbase_products().into_iter().any(|product| {
            product.instrument_id == series.instrument_id
                && product.entitlement_id == series.entitlement_id
        })
    {
        return Err("engine Coinbase snapshot identity is invalid".to_string());
    }
    let price_scale = u8::try_from(snapshot.price_scale)
        .map_err(|_| "engine price scale is invalid".to_string())?;
    let quantity_scale = u8::try_from(snapshot.quantity_scale)
        .map_err(|_| "engine quantity scale is invalid".to_string())?;
    let product = coinbase_products()
        .into_iter()
        .find(|product| product.instrument_id == series.instrument_id)
        .ok_or_else(|| "engine snapshot instrument is unsupported".to_string())?;
    if series.entitlement_id != product.entitlement_id {
        return Err("engine Coinbase snapshot entitlement is invalid".to_string());
    }
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(series.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: u64::from(series.definition_revision),
        asset_class: AssetClass::CryptoAsset,
        symbol: product.display_symbol,
        venue_id: "COINBASE".to_string(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(price_scale, quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    let definition = BarDefinition {
        definition_id: format!(
            "{}:{}:{}s",
            series.provider, series.instrument_id, series.cadence_value
        ),
        version: series.definition_revision,
        interval_seconds: series.cadence_value,
        trades_per_bar: None,
    };
    let received = now_unix_nanos();
    let bars = snapshot
        .bars
        .iter()
        .map(|bar| {
            provenanced_engine_bar(
                &series,
                snapshot.provider_generation,
                snapshot.consumer_id,
                snapshot.generation,
                bar,
                received,
            )
        })
        .collect();
    ReplaySnapshot::try_from_provenanced_values(
        instrument,
        ReplayProvenance::LiveProvider,
        definition,
        snapshot.publication_generation,
        bars,
    )
    .map_err(|error| error.to_string())
}

fn replay_tail_update(update: &SeriesUpdate) -> Result<ReplayTailUpdate, String> {
    let series = update
        .series
        .as_ref()
        .ok_or_else(|| "engine update has no series identity".to_string())?;
    if series.provider != "coinbase"
        || SeriesCadence::try_from(series.cadence) != Ok(SeriesCadence::FixedSeconds)
    {
        return Err("engine Coinbase update identity is invalid".to_string());
    }
    let bar = update
        .bar
        .as_ref()
        .ok_or_else(|| "engine Coinbase update has no bar".to_string())?;
    let item = provenanced_engine_bar(
        series,
        update.provider_generation,
        update.consumer_id,
        update.generation,
        bar,
        now_unix_nanos(),
    );
    ReplayTailUpdate::try_new(item, update.publication_generation, update.forming)
        .map_err(|error| error.to_string())
}

fn provenanced_engine_bar(
    series: &SeriesKey,
    provider_generation: u64,
    consumer_id: u64,
    generation: u64,
    bar: &axiusflow_local_engine_protocol::MarketBar,
    received: i64,
) -> Provenanced<MarketBar> {
    let bar = MarketBar {
        source_sequence: bar.source_sequence,
        exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
        exchange_timestamp_unix_nanos: bar.exchange_timestamp_unix_nanos,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: bar.volume,
    };
    let exchange = bar.exchange_timestamp_unix_nanos;
    Provenanced::new(
        bar,
        MarketEventProvenance {
            event_id: format!(
                "engine-{provider_generation}-{generation}-{}",
                bar.source_sequence
            ),
            event_time_unix_nanos: exchange,
            publication_time_unix_nanos: received,
            producer: "axiusflow_engine".to_string(),
            schema_version: 1,
            correlation_id: format!("engine-series-{consumer_id}-{generation}"),
            causation_id: String::new(),
            entitlement_revision: series.entitlement_id.clone(),
            session_generation: provider_generation,
            source_id: series.provider.clone(),
            source_sequence: bar.source_sequence,
            exchange_timestamp_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: received,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: received,
            normalized_timestamp_unix_nanos: received,
            fanout_enqueue_timestamp_unix_nanos: Some(received),
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        },
    )
}

fn series_key(
    product: &InstallProviderInstrument,
    interval: ChartInterval,
) -> Result<SeriesKey, String> {
    let supported_product = coinbase_products().into_iter().any(|supported| {
        product.provider == supported.provider
            && product.provider_symbol == supported.provider_symbol
            && product.instrument_id == supported.instrument_id
            && product.venue_id == supported.venue_id
            && product.price_scale == supported.price_scale
            && product.quantity_scale == supported.quantity_scale
            && product.entitlement_id == supported.entitlement_id
    });
    if !supported_product
        || !matches!(
            interval,
            ChartInterval::Minute1
                | ChartInterval::Minute5
                | ChartInterval::Minute15
                | ChartInterval::Hour1
        )
    {
        return Err(
            "this migration slice supports BTC-USD/ETH-USD at 1m, 5m, 15m, and 1h".to_string(),
        );
    }
    Ok(SeriesKey {
        provider: "coinbase".to_string(),
        instrument_id: product.instrument_id.clone(),
        cadence_value: match interval {
            ChartInterval::Minute1 => 60,
            ChartInterval::Minute5 => 300,
            ChartInterval::Minute15 => 900,
            ChartInterval::Hour1 => 3_600,
            _ => unreachable!("supported intervals were validated above"),
        },
        definition_revision: 1,
        entitlement_id: product.entitlement_id.clone(),
        cadence: SeriesCadence::FixedSeconds as i32,
    })
}

fn coinbase_products() -> Vec<InstallProviderInstrument> {
    [("BTC", "btc"), ("ETH", "eth")]
        .into_iter()
        .map(|(base, canonical)| InstallProviderInstrument {
            provider: "coinbase".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: format!("instrument:coinbase:{canonical}:usd"),
            provider_symbol: format!("{base}-USD"),
            display_symbol: format!("{base}/USD"),
            venue_id: "coinbase".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: "crypto_public_realtime".to_string(),
        })
        .collect()
}

fn empty_model() -> MarketBarClientModel {
    MarketBarClientModel::new(NonZeroUsize::new(MODEL_CAPACITY).unwrap_or(NonZeroUsize::MIN))
}

fn demand_error(error: &DemandError) -> String {
    let class = EngineFaultCode::try_from(error.code).map_or("unknown", |code| match code {
        EngineFaultCode::Retryable => "retryable",
        EngineFaultCode::Offline => "offline",
        EngineFaultCode::Cancelled => "cancelled",
        EngineFaultCode::Permanent => "permanent",
        EngineFaultCode::CorruptLocalState => "corrupt local state",
        EngineFaultCode::Unauthenticated => "unauthenticated",
        EngineFaultCode::VersionMismatch => "version mismatch",
        EngineFaultCode::Backpressure => "backpressure",
        EngineFaultCode::MalformedMessage => "malformed message",
        EngineFaultCode::OversizedFrame => "oversized frame",
    });
    format!("{} failed ({class}): {}", error.stage, error.detail)
}

fn now_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

fn random_identity() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    Ok(NonZeroU64::new(u64::from_le_bytes(bytes))
        .unwrap_or(NonZeroU64::MIN)
        .get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_local_engine_protocol::MarketBar as IpcMarketBar;

    #[test]
    fn ipc_snapshot_preserves_fixed_point_precision_and_engine_provenance() {
        let snapshot = replay_snapshot(&SeriesSnapshot {
            consumer_id: 1,
            generation: 1,
            series: Some(
                series_key(
                    coinbase_products().first().expect("BTC product"),
                    ChartInterval::Minute1,
                )
                .expect("series"),
            ),
            provider_generation: 7,
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_123_456_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            publication_generation: 1,
            forming: false,
        })
        .expect("snapshot converts");
        assert_eq!(snapshot.instrument().precision.price_scale(), 2);
        assert_eq!(snapshot.instrument().precision.quantity_scale(), 8);
        assert_eq!(snapshot.evidence().session_generation, 7);
        assert_eq!(snapshot.bars()[0].provenance().producer, "axiusflow_engine");
    }

    #[test]
    fn ipc_live_update_preserves_one_tail_without_rebuilding_history() {
        let update = replay_tail_update(&SeriesUpdate {
            consumer_id: 1,
            generation: 2,
            series: Some(
                series_key(
                    coinbase_products().first().expect("BTC product"),
                    ChartInterval::Minute1,
                )
                .expect("series"),
            ),
            provider_generation: 7,
            bar: Some(IpcMarketBar {
                source_sequence: 3,
                exchange_timestamp_seconds: 120,
                exchange_timestamp_unix_nanos: 120_000_000_000,
                open: 100,
                high: 120,
                low: 90,
                close: 115,
                volume: 9,
            }),
            forming: true,
            publication_generation: 8,
        })
        .expect("tail converts");
        assert_eq!(update.item().value().source_sequence, 3);
        assert_eq!(update.item().value().close, 115);
        assert_eq!(update.publication_generation(), 8);
        assert!(update.forming());
    }

    #[test]
    fn engine_recovery_state_remains_explicit_while_history_is_retained() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "coinbase".to_string(),
                state: ProviderConnectionState::Recovering as i32,
                generation: 2,
                detail: None,
            },
            &sender,
        )
        .expect("provider state applies");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message,
            }] if message.contains("retained history")
        ));
    }

    #[test]
    fn phase_four_series_keys_cover_required_symbols_and_intervals() {
        let products = coinbase_products();
        assert_eq!(products.len(), 2);
        for product in &products {
            for (interval, seconds) in [
                (ChartInterval::Minute1, 60),
                (ChartInterval::Minute5, 300),
                (ChartInterval::Minute15, 900),
                (ChartInterval::Hour1, 3_600),
            ] {
                let series = series_key(product, interval).expect("phase-four series validates");
                assert_eq!(series.cadence_value, seconds);
                assert_eq!(series.instrument_id, product.instrument_id);
            }
        }
    }

    #[test]
    fn coinbase_series_keys_reject_conflicting_provider_metadata() {
        let mut product = coinbase_products().remove(0);
        product.provider = "rithmic".to_string();
        assert!(series_key(&product, ChartInterval::Minute1).is_err());
    }
}
