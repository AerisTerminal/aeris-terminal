#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    path::PathBuf,
    process,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

use axiusflow_application::{ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate};
use axiusflow_coinbase_coordinator::market_worker::{
    ChartState, MarketDataWorker, MarketWorkerMessage, MarketWorkerPublication,
};
use axiusflow_coinbase_market_adapter::{
    CoinbaseHttpsHistoryTransport, CoinbaseProductCatalog, CoinbaseSpotProduct,
};
use axiusflow_engine::{
    ENGINE_SOCKET_NAME, EnginePublicationHub, EngineState, bind_listener,
    default_engine_state_root, load_coinbase_catalog, native_installation_token,
    persist_coinbase_catalog, serve_client_with_publications,
};
use axiusflow_local_engine_protocol::{
    CatalogEntry, ChartDelta, ChartProvenance, ChartSnapshot, DomBookState, DomLevel,
    DomRecoveryReason, DomRow as WireDomRow, DomSnapshot, EngineFaultCode, Fault,
    ProviderConnectionState, ProviderState, ViewKind, envelope, split_catalog,
};
use axiusflow_market_data::{
    ChartAggregation, ChartInterval, DomColumnLevel, OrderBookRecoveryReason, OrderBookState,
};
use axiusflow_market_protocol_adapter::{
    DecimalConvention, encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
    try_encode_replay_snapshot_chunk_envelopes,
};
use interprocess::local_socket::traits::Listener as _;

fn main() {
    if let Err(error) = run() {
        eprintln!("Axiusflow engine failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    const MAXIMUM_CLIENTS: usize = 4;

    let token = Arc::new(native_installation_token()?);
    let listener = bind_listener(ENGINE_SOCKET_NAME).map_err(|error| error.to_string())?;
    let mut epoch_bytes = [0_u8; 8];
    getrandom::fill(&mut epoch_bytes).map_err(|error| error.to_string())?;
    let engine_epoch = u64::from_le_bytes(epoch_bytes).max(1);
    let state_root = default_engine_state_root()?;
    let state = EngineState::open(&state_root)?;
    let publications = EnginePublicationHub::default();
    start_market_runtime(
        state.clone(),
        publications.clone(),
        engine_epoch,
        state_root,
    )?;
    let active_clients = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = listener.accept().map_err(|error| error.to_string())?;
        if active_clients
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAXIMUM_CLIENTS).then_some(active + 1)
            })
            .is_err()
        {
            drop(stream);
            continue;
        }
        let token = Arc::clone(&token);
        let state = state.clone();
        let publications = publications.clone();
        let active_clients = Arc::clone(&active_clients);
        thread::Builder::new()
            .name("axiusflow-engine-client".to_string())
            .spawn(move || {
                if let Err(error) = serve_client_with_publications(
                    stream,
                    token.as_slice(),
                    engine_epoch,
                    &state,
                    &publications,
                ) {
                    eprintln!("Axiusflow engine rejected a local client: {error}");
                }
                active_clients.fetch_sub(1, Ordering::AcqRel);
            })
            .map_err(|error| error.to_string())?;
    }
}

fn start_market_runtime(
    state: EngineState,
    publications: EnginePublicationHub,
    engine_epoch: u64,
    state_root: PathBuf,
) -> Result<(), String> {
    let (selection_tx, selection_rx) = mpsc::channel();
    thread::Builder::new()
        .name("axiusflow-engine-market".to_string())
        .spawn(move || {
            let market_thread = thread::current();
            state.set_selection_callback(Arc::new(move |workspace| {
                if selection_tx.send(workspace).is_ok() {
                    market_thread.unpark();
                }
            }));
            run_market_runtime(
                &state,
                &publications,
                engine_epoch,
                &state_root,
                &selection_rx,
            );
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn run_market_runtime(
    state: &EngineState,
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    state_root: &std::path::Path,
    selections: &Receiver<axiusflow_local_engine_protocol::WorkspaceState>,
) {
    let workspace = state.workspace();
    let history_root = default_coinbase_history_root();
    let Some(interval) = interval_from_seconds(workspace.interval_seconds) else {
        publish_fault(publications, "Workspace interval is unsupported");
        return;
    };
    let (products, refresh_catalog) = match bootstrap_catalog(state_root) {
        Ok(products) => products,
        Err(error) => {
            publish_fault(publications, &format!("Catalog bootstrap failed: {error}"));
            return;
        }
    };
    let Some(product) = products
        .iter()
        .find(|product| product.product_id == workspace.market)
        .cloned()
    else {
        publish_fault(
            publications,
            "Workspace market is unavailable in the active catalog",
        );
        return;
    };
    publish_catalog(publications, &products);
    let worker = MarketDataWorker::start_coinbase_product(
        product,
        interval,
        history_root,
        thread::current().id(),
        false,
        false,
        true,
    );
    let (_startup, mut worker) = match worker {
        Ok(worker) => worker,
        Err(error) => {
            publish_fault(
                publications,
                &format!("Market runtime could not start: {error}"),
            );
            return;
        }
    };
    let market_thread = thread::current();
    worker.set_message_wake(Arc::new(move || market_thread.unpark()));
    let catalog_refresh = spawn_catalog_refresh(refresh_catalog);
    let Ok(convention) = DecimalConvention::try_new("price_mantissa", "quantity_mantissa") else {
        return;
    };
    let mut chart_context: Option<ReplaySnapshot> = None;
    let mut products = products
        .into_iter()
        .map(|product| (product.product_id.clone(), product))
        .collect::<BTreeMap<_, _>>();
    let mut active_market = workspace.market;
    let mut active_interval_seconds = workspace.interval_seconds;
    loop {
        let (messages, disconnected) = worker.drain_messages();
        for message in messages {
            publish_market_message(
                publications,
                engine_epoch,
                &convention,
                &mut chart_context,
                &mut products,
                message,
            );
        }
        for selection in selections.try_iter() {
            if selection.market == active_market
                && selection.interval_seconds == active_interval_seconds
            {
                continue;
            }
            let Some(product) = products.get(&selection.market).cloned() else {
                continue;
            };
            let Some(interval) = interval_from_seconds(selection.interval_seconds) else {
                continue;
            };
            if worker.try_select_coinbase(product, interval).is_ok() {
                active_market = selection.market;
                active_interval_seconds = selection.interval_seconds;
            }
        }
        apply_catalog_refresh(
            catalog_refresh.as_ref(),
            state_root,
            publications,
            &mut products,
        );
        if disconnected {
            return;
        }
        thread::park();
    }
}

fn bootstrap_catalog(
    state_root: &std::path::Path,
) -> Result<(Vec<CoinbaseSpotProduct>, bool), String> {
    if let Some(products) = load_coinbase_catalog(state_root)? {
        return Ok((products, true));
    }
    let products = fetch_coinbase_catalog()?;
    if let Err(error) = persist_coinbase_catalog(state_root, &products) {
        eprintln!("Axiusflow engine catalog persistence failed: {error}");
    }
    Ok((products, false))
}

fn spawn_catalog_refresh(
    enabled: bool,
) -> Option<Receiver<Result<Vec<CoinbaseSpotProduct>, String>>> {
    enabled.then(|| {
        let (sender, receiver) = mpsc::sync_channel(1);
        let market_thread = thread::current();
        let _ = thread::Builder::new()
            .name("axiusflow-engine-catalog-refresh".to_string())
            .spawn(move || {
                let _ = sender.send(fetch_coinbase_catalog());
                market_thread.unpark();
            });
        receiver
    })
}

fn apply_catalog_refresh(
    refresh: Option<&Receiver<Result<Vec<CoinbaseSpotProduct>, String>>>,
    state_root: &std::path::Path,
    publications: &EnginePublicationHub,
    products: &mut BTreeMap<String, CoinbaseSpotProduct>,
) {
    let Some(refresh) = refresh else {
        return;
    };
    for refreshed in refresh.try_iter() {
        let Ok(refreshed) = refreshed else {
            continue;
        };
        if let Err(error) = persist_coinbase_catalog(state_root, &refreshed) {
            eprintln!("Axiusflow engine catalog persistence failed: {error}");
        }
        *products = refreshed
            .iter()
            .cloned()
            .map(|product| (product.product_id.clone(), product))
            .collect();
        publish_catalog(publications, &refreshed);
    }
}

fn fetch_coinbase_catalog() -> Result<Vec<CoinbaseSpotProduct>, String> {
    let mut catalog = CoinbaseProductCatalog::with_transport(CoinbaseHttpsHistoryTransport::new());
    catalog
        .fetch_active_spot_products()
        .map_err(|error| error.to_string())
}

fn publish_market_message(
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    convention: &DecimalConvention,
    chart_context: &mut Option<ReplaySnapshot>,
    product_catalog: &mut BTreeMap<String, CoinbaseSpotProduct>,
    message: MarketWorkerMessage,
) {
    match message {
        MarketWorkerMessage::Update(publication) => publish_chart_update(
            publications,
            engine_epoch,
            convention,
            chart_context,
            publication,
        ),
        MarketWorkerMessage::CoinbaseCatalog(Ok(products)) => {
            product_catalog.extend(
                products
                    .iter()
                    .cloned()
                    .map(|product| (product.product_id.clone(), product)),
            );
            publish_catalog(publications, &products);
        }
        MarketWorkerMessage::State { state, message } => {
            eprintln!("Axiusflow engine market state {state:?}: {message}");
            let state = match state {
                ChartState::Loading | ChartState::Recovering => ProviderConnectionState::Connecting,
                ChartState::Ready => ProviderConnectionState::Connected,
                ChartState::Stale | ChartState::Error => ProviderConnectionState::Disconnected,
            };
            publications.publish(
                ViewKind::Diagnostics,
                &envelope::Payload::ProviderState(ProviderState {
                    state: state as i32,
                    generation: 0,
                }),
            );
        }
        MarketWorkerMessage::Connection { state, .. } => {
            use axiusflow_observability::FeedConnectionState;
            let state = match state {
                FeedConnectionState::Discovering
                | FeedConnectionState::Authenticating
                | FeedConnectionState::Recovering => ProviderConnectionState::Connecting,
                FeedConnectionState::Streaming => ProviderConnectionState::Connected,
                FeedConnectionState::Disconnected | FeedConnectionState::Stopped => {
                    ProviderConnectionState::Disconnected
                }
            };
            publications.publish(
                ViewKind::Diagnostics,
                &envelope::Payload::ProviderState(ProviderState {
                    state: state as i32,
                    generation: 0,
                }),
            );
        }
        MarketWorkerMessage::CoinbaseDom(frame) => {
            let market = product_catalog
                .values()
                .find(|product| product.instrument_id == frame.instrument_id)
                .map_or_else(
                    || frame.instrument_id.clone(),
                    |product| product.product_id.clone(),
                );
            let (state, recovery_reason) = dom_wire_state(frame.state);
            let rows = frame
                .rows
                .into_iter()
                .map(|row| WireDomRow {
                    bid: row.bid.map(dom_wire_level),
                    ask: row.ask.map(dom_wire_level),
                })
                .collect();
            publications.publish(
                ViewKind::Dom,
                &envelope::Payload::DomSnapshot(DomSnapshot {
                    market,
                    engine_epoch,
                    selection_generation: frame.selection_generation,
                    provider_generation: frame.session_generation,
                    payload: Vec::new(),
                    provider_id: frame.provider_id,
                    instrument_id: frame.instrument_id,
                    entitlement_id: frame.entitlement_id,
                    revision: frame.revision,
                    source_watermark: frame.source_watermark,
                    state: state as i32,
                    recovery_reason,
                    rows,
                }),
            );
        }
        MarketWorkerMessage::Diagnostics(_)
        | MarketWorkerMessage::Recovery { .. }
        | MarketWorkerMessage::RithmicCatalog(_)
        | MarketWorkerMessage::RithmicHistory { .. }
        | MarketWorkerMessage::RithmicLive { .. }
        | MarketWorkerMessage::RithmicDom(_)
        | MarketWorkerMessage::CoinbaseSwitchMarker { .. }
        | MarketWorkerMessage::CoinbaseCatalog(Err(_)) => {}
    }
}

fn dom_wire_level(level: DomColumnLevel) -> DomLevel {
    DomLevel {
        price: level.price,
        quantity: level.quantity,
        order_count: level.order_count,
        price_text: level.price_text,
        quantity_text: level.quantity_text,
        relative_size_bps: u32::from(level.relative_size_bps),
    }
}

fn dom_wire_state(state: OrderBookState) -> (DomBookState, Option<i32>) {
    match state {
        OrderBookState::Ready => (DomBookState::Ready, None),
        OrderBookState::Stale => (DomBookState::Stale, None),
        OrderBookState::Recovering(reason) => (
            DomBookState::Recovering,
            Some(match reason {
                OrderBookRecoveryReason::AwaitingSnapshot => DomRecoveryReason::AwaitingSnapshot,
                OrderBookRecoveryReason::SequenceGap => DomRecoveryReason::SequenceGap,
                OrderBookRecoveryReason::CrossedBook => DomRecoveryReason::CrossedBook,
                OrderBookRecoveryReason::InvalidUpdate => DomRecoveryReason::InvalidUpdate,
            } as i32),
        ),
    }
}

fn publish_catalog(publications: &EnginePublicationHub, products: &[CoinbaseSpotProduct]) {
    let entries = products
        .iter()
        .map(|product| CatalogEntry {
            product_id: product.product_id.clone(),
            base_currency: product.base_currency.clone(),
            quote_currency: product.quote_currency.clone(),
            price_scale: u32::from(product.price_scale),
            quantity_scale: u32::from(product.quantity_scale),
        })
        .collect();
    let payloads: Vec<envelope::Payload> = split_catalog(entries, 1)
        .into_iter()
        .map(envelope::Payload::CatalogSnapshot)
        .collect();
    publications.publish_covering(ViewKind::Catalog, &payloads);
}

fn publish_fault(publications: &EnginePublicationHub, detail: &str) {
    eprintln!("Axiusflow engine market fault: {detail}");
    publications.publish(
        ViewKind::Diagnostics,
        &envelope::Payload::Fault(Fault {
            code: EngineFaultCode::Retryable as i32,
            redacted_detail: detail.to_string(),
        }),
    );
}

fn interval_from_seconds(seconds: u32) -> Option<ChartInterval> {
    ChartInterval::ALL.iter().copied().find(|interval| {
        matches!(
            interval.aggregation(),
            ChartAggregation::FixedSeconds(interval_seconds) if interval_seconds.get() == seconds
        ) || (*interval == ChartInterval::Month1 && seconds == 30 * 24 * 60 * 60)
    })
}

fn publish_chart_update(
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    convention: &DecimalConvention,
    chart_context: &mut Option<ReplaySnapshot>,
    publication: MarketWorkerPublication,
) {
    const MAXIMUM_INNER_FRAME_BYTES: usize = 900 * 1024;
    const SNAPSHOT_CHUNK_ITEMS: usize = 256;

    let maximum_frame = NonZeroUsize::new(MAXIMUM_INNER_FRAME_BYTES).unwrap_or(NonZeroUsize::MIN);
    let provenance = match &publication.update {
        ReplayStreamUpdate::Snapshot(snapshot) => snapshot.provenance(),
        ReplayStreamUpdate::Delta(_) => chart_context
            .as_ref()
            .map_or(ReplayProvenance::LiveProvider, ReplaySnapshot::provenance),
    };
    let wire_provenance = chart_provenance(provenance);
    match publication.update {
        ReplayStreamUpdate::Snapshot(snapshot) => {
            let Ok(envelopes) = try_encode_replay_snapshot_chunk_envelopes(
                publication.subscription_id,
                format!(
                    "engine-{engine_epoch}-{}",
                    publication.generation.generation()
                ),
                &snapshot,
                convention,
                NonZeroUsize::new(SNAPSHOT_CHUNK_ITEMS).unwrap_or(NonZeroUsize::MIN),
            ) else {
                return;
            };
            let payloads = envelopes
                .into_iter()
                .filter_map(|envelope| {
                    encode_market_bar_stream_frame(&envelope, maximum_frame)
                        .ok()
                        .map(|payload| {
                            envelope::Payload::ChartSnapshot(ChartSnapshot {
                                market: snapshot.instrument().instrument_id.as_str().to_string(),
                                interval_seconds: snapshot.bar_definition().interval_seconds,
                                engine_epoch,
                                selection_generation: 0,
                                provider_generation: publication.generation.generation(),
                                payload,
                                provenance: wire_provenance as i32,
                            })
                        })
                })
                .collect::<Vec<_>>();
            if !payloads.is_empty() {
                publications.publish_covering(ViewKind::Chart, &payloads);
                *chart_context = Some(snapshot);
            }
        }
        ReplayStreamUpdate::Delta(delta) => {
            let Some(snapshot) = chart_context.as_ref() else {
                return;
            };
            let Ok(envelope) = try_encode_replay_delta_envelope(
                publication.subscription_id,
                snapshot.instrument(),
                snapshot.bar_definition(),
                &delta,
                convention,
            ) else {
                return;
            };
            let Ok(payload) = encode_market_bar_stream_frame(&envelope, maximum_frame) else {
                return;
            };
            publications.publish_transient(
                ViewKind::Chart,
                &envelope::Payload::ChartDelta(ChartDelta {
                    market: snapshot.instrument().instrument_id.as_str().to_string(),
                    interval_seconds: snapshot.bar_definition().interval_seconds,
                    engine_epoch,
                    selection_generation: 0,
                    provider_generation: publication.generation.generation(),
                    payload,
                    provenance: wire_provenance as i32,
                }),
            );
        }
    }
}

const fn chart_provenance(provenance: ReplayProvenance) -> ChartProvenance {
    match provenance {
        ReplayProvenance::LocalCache => ChartProvenance::LocalCache,
        ReplayProvenance::LiveProvider => ChartProvenance::LiveProvider,
        ReplayProvenance::EmbeddedFixture => ChartProvenance::EmbeddedFixture,
    }
}

fn default_coinbase_history_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("local-data").join("coinbase-history"),
        |root| {
            PathBuf::from(root)
                .join("Axiusflow")
                .join("market-history")
                .join("coinbase")
        },
    )
}
