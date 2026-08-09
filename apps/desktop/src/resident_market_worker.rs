use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicU64, mpsc},
    thread,
    time::{Duration, Instant},
};

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, ReplayProvenance, ReplayStreamUpdate,
};
use axiusflow_coinbase_coordinator::market_worker::{
    ChartViewportUpdate, CoinbaseWorkerStartup, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerCommand, MarketWorkerMessage, MarketWorkerPublication, MarketWorkerSender,
    market_worker_channel,
};
use axiusflow_coinbase_market_adapter::{CoinbaseSpotProduct, coinbase_instrument_id};
use axiusflow_engine::{EngineClient, connect_or_start_engine, sibling_engine_executable};
use axiusflow_local_engine_protocol::{
    CatalogReassembler, ChartProvenance, DomBookState, DomRecoveryReason, DomSnapshot,
    ProviderConnectionState, RithmicHistory, RithmicSearch, RithmicSelect, ViewKind,
    WorkspaceState, envelope,
};
use axiusflow_market_data::{
    ChartAggregation, ChartInterval, DomColumnLevel, DomFrame, DomRow, OrderBookRecoveryReason,
    OrderBookState,
};
use axiusflow_market_protocol_adapter::{BinaryMarketBarStreamDecoder, DecimalConvention};
use axiusflow_observability::FeedConnectionState;
use axiusflow_rithmic_protocol_adapter::{
    CollectedSymbols, RithmicCatalogEvent, RithmicCatalogRejection, SymbolSearchResult,
};
use axiusflow_rithmic_protocol_adapter::{InstrumentType, SearchPattern};

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
const MAXIMUM_INNER_FRAME_BYTES: usize = 900 * 1024;
const MAXIMUM_BUFFERED_BYTES: usize = 2 * MAXIMUM_INNER_FRAME_BYTES;
const MODEL_ITEM_CAPACITY: usize = 20_000;
const WORKER_LABEL: &str = "Resident Coinbase market engine";
const SUBSCRIPTION_ID: &str = "resident_coinbase_market_bars";
const VIEWPORT_PERSIST_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
enum ResidentProvider {
    Coinbase,
    Rithmic,
}

type LatestSnapshot = Arc<Mutex<Option<MarketWorkerBootstrap>>>;

pub(super) fn start() -> (
    axiusflow_coinbase_coordinator::market_worker::MarketWorkerStartup,
    MarketDataWorker,
) {
    start_provider(
        ResidentProvider::Coinbase,
        axiusflow_coinbase_coordinator::market_worker::MarketWorkerStartup::Loading(Box::new(
            CoinbaseWorkerStartup {
                coinbase_product: placeholder_product("BTC-USD"),
                subscription_id: SUBSCRIPTION_ID.to_string(),
                worker_label: WORKER_LABEL.to_string(),
            },
        )),
    )
}

pub(super) fn start_rithmic() -> Result<
    (
        axiusflow_coinbase_coordinator::market_worker::MarketWorkerStartup,
        MarketDataWorker,
    ),
    String,
> {
    let shell = axiusflow_coinbase_coordinator::rithmic_shell::RithmicShellState::local()?;
    Ok(start_provider(
        ResidentProvider::Rithmic,
        axiusflow_coinbase_coordinator::market_worker::MarketWorkerStartup::Shell(shell),
    ))
}

fn start_provider(
    provider: ResidentProvider,
    startup: axiusflow_coinbase_coordinator::market_worker::MarketWorkerStartup,
) -> (
    axiusflow_coinbase_coordinator::market_worker::MarketWorkerStartup,
    MarketDataWorker,
) {
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    let sequence = Arc::new(AtomicU64::new(0));
    thread::Builder::new()
        .name("axiusflow-resident-engine-bridge".to_string())
        .spawn(move || {
            run_bridge(&message_tx, &command_rx, provider);
            let _ = shutdown_tx.send(());
        })
        .expect("resident engine bridge thread starts");

    let worker =
        MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, Some(sequence));
    (startup, worker)
}

fn run_bridge(
    message_tx: &MarketWorkerSender,
    command_rx: &mpsc::Receiver<MarketWorkerCommand>,
    provider: ResidentProvider,
) {
    let executable = match sibling_engine_executable() {
        Ok(executable) => executable,
        Err(error) => {
            send_error(message_tx, error);
            return;
        }
    };
    let Some((mut commands, mut workspace)) = connect_commands_until_ready(message_tx, &executable)
    else {
        return;
    };
    if let Err(error) = select_initial_provider(&mut commands, &mut workspace, provider) {
        send_error(message_tx, error);
        return;
    }
    let latest_snapshot = Arc::new(Mutex::new(None));
    spawn_session_stream(
        message_tx.clone(),
        Arc::clone(&latest_snapshot),
        executable.clone(),
    );
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: axiusflow_coinbase_coordinator::market_worker::ChartState::Loading,
        message: "Attached to the resident market engine".to_string(),
    });
    publish_restored_viewport(message_tx, &workspace);

    let mut pending_viewport: Option<ChartViewportUpdate> = None;
    let mut viewport_deadline: Option<Instant> = None;
    while let Some(command) = receive_bridge_command(
        command_rx,
        &mut pending_viewport,
        &mut viewport_deadline,
        &mut commands,
        &mut workspace,
        &executable,
    ) {
        match command {
            MarketWorkerCommand::CoinbaseSelect(request) => {
                pending_viewport = None;
                viewport_deadline = None;
                let Some(interval_seconds) = interval_seconds(request.interval) else {
                    send_error(message_tx, "unsupported Coinbase interval".to_string());
                    continue;
                };
                match set_selection_reconnecting(
                    &mut commands,
                    &mut workspace,
                    &executable,
                    request.product.product_id,
                    interval_seconds,
                    request.sequence,
                ) {
                    Ok(updated) => {
                        workspace = updated;
                        let _ = message_tx.send(MarketWorkerMessage::CoinbaseSwitchMarker {
                            sequence: request.sequence,
                        });
                        publish_restored_viewport(message_tx, &workspace);
                    }
                    Err(error) => send_error(message_tx, error),
                }
            }
            MarketWorkerCommand::Recovery(command) => {
                publish_recovery(message_tx, &latest_snapshot, command.request_id);
            }
            MarketWorkerCommand::ChartViewport(viewport) => {
                pending_viewport = Some(viewport);
                viewport_deadline = Some(Instant::now() + VIEWPORT_PERSIST_DELAY);
            }
            MarketWorkerCommand::Shutdown => {
                if let Some(viewport) = pending_viewport.take() {
                    let _ = set_viewport_reconnecting(
                        &mut commands,
                        &mut workspace,
                        &executable,
                        viewport,
                    );
                }
                break;
            }
            command @ (MarketWorkerCommand::RithmicSearch(_)
            | MarketWorkerCommand::RithmicSelect(_)
            | MarketWorkerCommand::RithmicHistory(_)) => {
                if let Err(error) = dispatch_rithmic_command(&mut commands, &mut workspace, command)
                {
                    send_error(message_tx, error);
                }
            }
        }
    }
}

fn receive_bridge_command(
    commands_rx: &mpsc::Receiver<MarketWorkerCommand>,
    pending_viewport: &mut Option<ChartViewportUpdate>,
    viewport_deadline: &mut Option<Instant>,
    commands: &mut EngineClient,
    workspace: &mut WorkspaceState,
    executable: &Path,
) -> Option<MarketWorkerCommand> {
    loop {
        let Some(deadline) = *viewport_deadline else {
            return commands_rx.recv().ok();
        };
        match commands_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(command) => return Some(command),
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                *viewport_deadline = None;
                if let Some(viewport) = pending_viewport.take()
                    && let Err(error) =
                        set_viewport_reconnecting(commands, workspace, executable, viewport)
                {
                    eprintln!("resident viewport was not persisted: {error}");
                }
            }
        }
    }
}

fn select_initial_provider(
    commands: &mut EngineClient,
    workspace: &mut WorkspaceState,
    provider: ResidentProvider,
) -> Result<(), String> {
    if matches!(provider, ResidentProvider::Rithmic) {
        *workspace = commands.set_provider_selection(
            "rithmic".to_string(),
            workspace.market.clone(),
            workspace.interval_seconds,
            workspace.workspace_revision,
            1,
        )?;
    }
    Ok(())
}

fn dispatch_rithmic_command(
    commands: &mut EngineClient,
    workspace: &mut WorkspaceState,
    command: MarketWorkerCommand,
) -> Result<(), String> {
    match command {
        MarketWorkerCommand::RithmicSearch(search) => commands.search_rithmic(RithmicSearch {
            generation: usize_to_u64(search.generation().get()),
            query: search.query().to_string(),
            exchange: search.exchange().map(str::to_string),
            product_code: search.product_code().map(str::to_string),
            instrument_type: search.instrument_type().map(instrument_type_name),
            contains: search.pattern() == SearchPattern::Contains,
            maximum_results: usize_to_u32(search.maximum_results().get()),
        }),
        MarketWorkerCommand::RithmicSelect(selection) => {
            let subscription = selection.subscription();
            let request = RithmicSelect {
                selection_generation: usize_to_u64(selection.generation().get()),
                search_generation: usize_to_u64(selection.search_generation().get()),
                symbol: selection.symbol().to_string(),
                exchange: selection.exchange().to_string(),
                entitlement_id: selection.entitlement_id().to_string(),
                trades: subscription.trades(),
                quotes: subscription.quotes(),
                order_book: subscription.order_book(),
            };
            *workspace = commands.set_provider_selection(
                "rithmic".to_string(),
                request.symbol.clone(),
                workspace.interval_seconds,
                workspace.workspace_revision,
                request.selection_generation,
            )?;
            commands.select_rithmic(request)
        }
        MarketWorkerCommand::RithmicHistory(request) => {
            if let Some(interval_seconds) = request.series.interval_seconds()
                && let Ok(interval_seconds) = u32::try_from(interval_seconds)
            {
                *workspace = commands.set_provider_selection(
                    "rithmic".to_string(),
                    workspace.market.clone(),
                    interval_seconds,
                    workspace.workspace_revision,
                    usize_to_u64(request.selection_generation.get()),
                )?;
            }
            commands.request_rithmic_history(RithmicHistory {
                selection_generation: usize_to_u64(request.selection_generation.get()),
                series_generation: usize_to_u64(request.series_generation.get()),
                series: request.series.label().to_string(),
            })
        }
        _ => Err("resident bridge received a non-Rithmic provider command".to_string()),
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn instrument_type_name(instrument_type: InstrumentType) -> String {
    match instrument_type {
        InstrumentType::Future => "FUTURE",
        InstrumentType::FutureOption => "FUTURE_OPTION",
        InstrumentType::FutureStrategy => "FUTURE_STRATEGY",
        InstrumentType::Equity => "EQUITY",
        InstrumentType::EquityOption => "EQUITY_OPTION",
        InstrumentType::EquityStrategy => "EQUITY_STRATEGY",
        InstrumentType::Index => "INDEX",
        InstrumentType::IndexOption => "INDEX_OPTION",
        InstrumentType::Spread => "SPREAD",
        InstrumentType::Synthetic => "SYNTHETIC",
    }
    .to_string()
}

fn publish_recovery(
    message_tx: &MarketWorkerSender,
    latest_snapshot: &LatestSnapshot,
    request_id: u64,
) {
    let result = latest_snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(clone_bootstrap)
        .ok_or_else(|| "resident engine has not published a covering snapshot".to_string());
    let _ = message_tx.send(MarketWorkerMessage::Recovery { request_id, result });
}

fn connect_commands_until_ready(
    message_tx: &MarketWorkerSender,
    executable: &Path,
) -> Option<(EngineClient, WorkspaceState)> {
    loop {
        match connect_commands(executable) {
            Ok(connected) => return Some(connected),
            Err(error) => {
                if !send_recovering(message_tx, &error) {
                    return None;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn publish_restored_viewport(message_tx: &MarketWorkerSender, workspace: &WorkspaceState) {
    let viewport = workspace.hot_series.iter().find(|series| {
        series.provider == workspace.provider
            && series.market == workspace.market
            && series.interval_seconds == workspace.interval_seconds
    });
    if let Some((start_unix_nanos, end_unix_nanos)) = viewport.and_then(|series| {
        series
            .viewport_start_unix_nanos
            .zip(series.viewport_end_unix_nanos)
    }) {
        let _ = message_tx.send(MarketWorkerMessage::ChartViewport {
            start_unix_nanos,
            end_unix_nanos,
        });
    }
}

fn spawn_session_stream(
    message_tx: MarketWorkerSender,
    latest_snapshot: LatestSnapshot,
    executable: PathBuf,
) {
    let _ = thread::Builder::new()
        .name("axiusflow-engine-session-stream".to_string())
        .spawn(move || {
            loop {
                if let Err(error) = run_session_stream(&message_tx, &latest_snapshot, &executable) {
                    eprintln!("resident engine stream stopped: {error}");
                    if !send_recovering(&message_tx, &error) {
                        break;
                    }
                    thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
}

fn run_session_stream(
    message_tx: &MarketWorkerSender,
    latest_snapshot: &LatestSnapshot,
    executable: &Path,
) -> Result<(), String> {
    let mut stream = connect_or_start_engine(executable)?.subscribe_view(ViewKind::Session)?;
    let convention = DecimalConvention::try_new("price_mantissa", "quantity_mantissa")
        .map_err(|error| error.to_string())?;
    let mut decoder = chart_decoder(&convention, ReplayProvenance::LiveProvider)?;
    let mut provenance = ReplayProvenance::LiveProvider;
    let mut model = MarketBarClientModel::new(nonzero(MODEL_ITEM_CAPACITY));
    let mut rithmic_decoder = chart_decoder(&convention, ReplayProvenance::LiveProvider)?;
    let mut rithmic_model = MarketBarClientModel::new(nonzero(MODEL_ITEM_CAPACITY));
    let mut catalog = CatalogReassembler::new();
    loop {
        let publication = stream.receive()?;
        let (payload, next_provenance, is_snapshot) = match publication {
            envelope::Payload::ChartSnapshot(snapshot) => (
                snapshot.payload,
                replay_provenance(snapshot.provenance)?,
                true,
            ),
            envelope::Payload::ChartDelta(delta) => {
                (delta.payload, replay_provenance(delta.provenance)?, false)
            }
            envelope::Payload::CatalogSnapshot(snapshot) => {
                if let Some(entries) = catalog.push(snapshot).map_err(|error| error.to_string())? {
                    publish_catalog(message_tx, entries)?;
                }
                continue;
            }
            envelope::Payload::RithmicCatalog(catalog) => {
                publish_rithmic_catalog(message_tx, catalog)?;
                continue;
            }
            envelope::Payload::RithmicChart(chart) => {
                publish_rithmic_chart(
                    message_tx,
                    latest_snapshot,
                    &mut rithmic_decoder,
                    &mut rithmic_model,
                    &chart,
                )?;
                continue;
            }
            envelope::Payload::ProviderState(provider) => {
                publish_provider_state(message_tx, provider.state)?;
                continue;
            }
            envelope::Payload::DomSnapshot(snapshot) => {
                publish_dom(message_tx, snapshot)?;
                continue;
            }
            envelope::Payload::Fault(fault) => {
                send_error(message_tx, fault.redacted_detail);
                continue;
            }
            _ => continue,
        };
        if is_snapshot && next_provenance != provenance {
            decoder = chart_decoder(&convention, next_provenance)?;
            model = MarketBarClientModel::new(nonzero(MODEL_ITEM_CAPACITY));
            provenance = next_provenance;
        }
        publish_chart_payload(
            message_tx,
            latest_snapshot,
            &mut decoder,
            &mut model,
            &payload,
        )?;
    }
}

fn publish_chart_payload(
    message_tx: &MarketWorkerSender,
    latest_snapshot: &LatestSnapshot,
    decoder: &mut BinaryMarketBarStreamDecoder,
    model: &mut MarketBarClientModel,
    payload: &[u8],
) -> Result<(), String> {
    for projected in decoder.push(payload).map_err(|error| error.to_string())? {
        let update = projected.update;
        let MarketBarModelOutcome::Published(generation) = model
            .apply_update(update.clone())
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if let ReplayStreamUpdate::Snapshot(snapshot) = &update {
            *latest_snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(MarketWorkerBootstrap {
                snapshot: snapshot.clone(),
                subscription_id: projected.subscription_id.clone(),
                generation: generation.clone(),
                worker_label: WORKER_LABEL.to_string(),
            });
        }
        message_tx
            .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                update,
                generation,
                subscription_id: projected.subscription_id,
                worker_label: WORKER_LABEL.to_string(),
                ui_diagnostics: None,
            }))
            .map_err(|_| "desktop market mailbox disconnected".to_string())?;
    }
    Ok(())
}

fn publish_rithmic_chart(
    message_tx: &MarketWorkerSender,
    latest_snapshot: &LatestSnapshot,
    decoder: &mut BinaryMarketBarStreamDecoder,
    model: &mut MarketBarClientModel,
    chart: &axiusflow_local_engine_protocol::RithmicChart,
) -> Result<(), String> {
    let selection_generation = nonzero_usize(chart.selection_generation)?;
    let series_generation = nonzero_usize(chart.series_generation)?;
    for projected in decoder
        .push(&chart.payload)
        .map_err(|error| error.to_string())?
    {
        let MarketBarModelOutcome::Published(generation) = model
            .apply_update(projected.update.clone())
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let ReplayStreamUpdate::Snapshot(snapshot) = projected.update else {
            continue;
        };
        let bootstrap = MarketWorkerBootstrap {
            snapshot: snapshot.clone(),
            subscription_id: projected.subscription_id,
            generation,
            worker_label: "Resident Rithmic market engine".to_string(),
        };
        *latest_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(clone_bootstrap(&bootstrap));
        let message = if chart.live {
            MarketWorkerMessage::RithmicLive {
                selection_generation,
                series_generation,
                snapshot,
            }
        } else {
            MarketWorkerMessage::RithmicHistory {
                selection_generation,
                series_generation,
                result: Ok(Box::new(bootstrap)),
            }
        };
        message_tx
            .send(message)
            .map_err(|_| "desktop market mailbox disconnected".to_string())?;
    }
    Ok(())
}

fn publish_rithmic_catalog(
    message_tx: &MarketWorkerSender,
    catalog: axiusflow_local_engine_protocol::RithmicCatalog,
) -> Result<(), String> {
    let command_generation = nonzero_usize(catalog.command_generation)?;
    let event = match catalog.kind {
        0 => RithmicCatalogEvent::search_completed(
            nonzero_u64(catalog.session_generation)?,
            command_generation,
            CollectedSymbols {
                results: catalog
                    .symbols
                    .into_iter()
                    .map(|symbol| SymbolSearchResult {
                        symbol: symbol.symbol,
                        exchange: symbol.exchange,
                        name: symbol.name,
                        product_code: symbol.product_code,
                        instrument_type: symbol.instrument_type,
                        expiration_date: symbol.expiration_date,
                    })
                    .collect(),
                duplicate_count: 0,
            },
        ),
        1 => RithmicCatalogEvent::selection_installed(
            nonzero_u64(catalog.session_generation)?,
            command_generation,
            required(catalog.instrument_id)?,
            required(catalog.provider_symbol)?,
            required(catalog.display_symbol)?,
            required(catalog.venue_id)?,
            u8::try_from(required(catalog.price_scale)?)
                .map_err(|_| "Rithmic price scale is invalid".to_string())?,
            u8::try_from(required(catalog.quantity_scale)?)
                .map_err(|_| "Rithmic quantity scale is invalid".to_string())?,
            required(catalog.entitlement_id)?,
        )
        .map_err(|_| "Rithmic installed selection is invalid".to_string())?,
        2 => RithmicCatalogEvent::command_rejected(
            catalog
                .session_generation
                .map(|value| NonZeroU64::new(value).ok_or(()))
                .transpose()
                .map_err(|()| "Rithmic session generation is invalid".to_string())?,
            command_generation,
            rithmic_rejection(required(catalog.rejection)?)?,
        ),
        _ => return Err("Rithmic catalog publication kind is invalid".to_string()),
    };
    message_tx
        .send(MarketWorkerMessage::RithmicCatalog(event))
        .map_err(|_| "desktop market mailbox disconnected".to_string())
}

fn required<T>(value: Option<T>) -> Result<T, String> {
    value.ok_or_else(|| "Rithmic catalog publication is incomplete".to_string())
}

fn nonzero_u64(value: Option<u64>) -> Result<NonZeroU64, String> {
    value
        .and_then(NonZeroU64::new)
        .ok_or_else(|| "Rithmic session generation is invalid".to_string())
}

fn nonzero_usize(value: u64) -> Result<NonZeroUsize, String> {
    usize::try_from(value)
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| "Rithmic command generation is invalid".to_string())
}

fn rithmic_rejection(value: u32) -> Result<RithmicCatalogRejection, String> {
    match value {
        0 => Ok(RithmicCatalogRejection::SearchRejected),
        1 => Ok(RithmicCatalogRejection::SupersededSearch),
        2 => Ok(RithmicCatalogRejection::InstrumentUnavailable),
        3 => Ok(RithmicCatalogRejection::SubscriptionRejected),
        4 => Ok(RithmicCatalogRejection::SearchDispatchUnavailable),
        5 => Ok(RithmicCatalogRejection::SelectionDispatchUnavailable),
        _ => Err("Rithmic command rejection is invalid".to_string()),
    }
}

fn connect_commands(executable: &Path) -> Result<(EngineClient, WorkspaceState), String> {
    let mut client = connect_or_start_engine(executable)?;
    let workspace = client.restore_workspace()?;
    Ok((client, workspace))
}

fn set_selection_reconnecting(
    commands: &mut EngineClient,
    workspace: &mut WorkspaceState,
    executable: &Path,
    market: String,
    interval_seconds: u32,
    selection_generation: u64,
) -> Result<WorkspaceState, String> {
    if let Ok(updated) = commands.set_selection(
        market.clone(),
        interval_seconds,
        workspace.workspace_revision,
        selection_generation,
    ) {
        return Ok(updated);
    }
    let (mut replacement, restored) = connect_commands(executable)?;
    let updated = replacement.set_selection(
        market,
        interval_seconds,
        restored.workspace_revision,
        selection_generation,
    )?;
    *commands = replacement;
    Ok(updated)
}

fn set_viewport_reconnecting(
    commands: &mut EngineClient,
    workspace: &mut WorkspaceState,
    executable: &Path,
    viewport: ChartViewportUpdate,
) -> Result<(), String> {
    if let Ok(updated) = commands.set_viewport(
        viewport.start_unix_nanos,
        viewport.end_unix_nanos,
        viewport.selection_generation,
    ) {
        *workspace = updated;
        return Ok(());
    }
    let (mut replacement, restored) = connect_commands(executable)?;
    replacement.set_selection(
        workspace.market.clone(),
        workspace.interval_seconds,
        restored.workspace_revision,
        viewport.selection_generation,
    )?;
    let updated = replacement.set_viewport(
        viewport.start_unix_nanos,
        viewport.end_unix_nanos,
        viewport.selection_generation,
    )?;
    *commands = replacement;
    *workspace = updated;
    Ok(())
}

fn publish_catalog(
    message_tx: &MarketWorkerSender,
    entries: Vec<axiusflow_local_engine_protocol::CatalogEntry>,
) -> Result<(), String> {
    let products = entries
        .into_iter()
        .map(|entry| {
            Ok(CoinbaseSpotProduct {
                instrument_id: coinbase_instrument_id(&entry.product_id)
                    .unwrap_or_else(|_| entry.product_id.clone()),
                display_symbol: format!("{}/{}", entry.base_currency, entry.quote_currency),
                product_id: entry.product_id,
                base_currency: entry.base_currency,
                quote_currency: entry.quote_currency,
                price_scale: u8::try_from(entry.price_scale)
                    .map_err(|_| "resident catalog price scale is invalid".to_string())?,
                quantity_scale: u8::try_from(entry.quantity_scale)
                    .map_err(|_| "resident catalog quantity scale is invalid".to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    message_tx
        .send(MarketWorkerMessage::CoinbaseCatalog(Ok(products)))
        .map_err(|_| "desktop market mailbox disconnected".to_string())
}

fn publish_provider_state(message_tx: &MarketWorkerSender, provider: i32) -> Result<(), String> {
    let state = match ProviderConnectionState::try_from(provider)
        .map_err(|_| "resident engine provider state is invalid".to_string())?
    {
        ProviderConnectionState::Connected => FeedConnectionState::Streaming,
        ProviderConnectionState::Connecting => FeedConnectionState::Recovering,
        ProviderConnectionState::Disconnected
        | ProviderConnectionState::Rejected
        | ProviderConnectionState::ShuttingDown => FeedConnectionState::Disconnected,
    };
    message_tx
        .send(MarketWorkerMessage::Connection {
            state,
            message: match state {
                FeedConnectionState::Streaming => "Resident provider stream is live".to_string(),
                _ => "Resident provider stream is reconnecting".to_string(),
            },
        })
        .map_err(|_| "desktop market mailbox disconnected".to_string())
}

fn publish_dom(message_tx: &MarketWorkerSender, snapshot: DomSnapshot) -> Result<(), String> {
    let state = match DomBookState::try_from(snapshot.state)
        .map_err(|_| "resident DOM state is invalid".to_string())?
    {
        DomBookState::Ready => OrderBookState::Ready,
        DomBookState::Stale => OrderBookState::Stale,
        DomBookState::Recovering => {
            let reason = DomRecoveryReason::try_from(
                snapshot
                    .recovery_reason
                    .ok_or_else(|| "resident recovering DOM has no recovery reason".to_string())?,
            )
            .map_err(|_| "resident DOM recovery reason is invalid".to_string())?;
            OrderBookState::Recovering(match reason {
                DomRecoveryReason::AwaitingSnapshot => OrderBookRecoveryReason::AwaitingSnapshot,
                DomRecoveryReason::SequenceGap => OrderBookRecoveryReason::SequenceGap,
                DomRecoveryReason::CrossedBook => OrderBookRecoveryReason::CrossedBook,
                DomRecoveryReason::InvalidUpdate => OrderBookRecoveryReason::InvalidUpdate,
            })
        }
    };
    let rows = snapshot
        .rows
        .into_iter()
        .map(|row| {
            Ok(DomRow {
                bid: row.bid.map(dom_level).transpose()?,
                ask: row.ask.map(dom_level).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let rithmic = snapshot.provider_id == "rithmic";
    let frame = DomFrame {
        provider_id: snapshot.provider_id,
        instrument_id: snapshot.instrument_id,
        entitlement_id: snapshot.entitlement_id,
        session_generation: snapshot.provider_generation,
        selection_generation: snapshot.selection_generation,
        revision: snapshot.revision,
        source_watermark: snapshot.source_watermark,
        state,
        rows,
    };
    message_tx
        .send(if rithmic {
            MarketWorkerMessage::RithmicDom(frame)
        } else {
            MarketWorkerMessage::CoinbaseDom(frame)
        })
        .map_err(|_| "desktop market mailbox disconnected".to_string())
}

fn dom_level(level: axiusflow_local_engine_protocol::DomLevel) -> Result<DomColumnLevel, String> {
    Ok(DomColumnLevel {
        price: level.price,
        quantity: level.quantity,
        order_count: level.order_count,
        price_text: level.price_text,
        quantity_text: level.quantity_text,
        relative_size_bps: u16::try_from(level.relative_size_bps)
            .map_err(|_| "resident DOM relative size is invalid".to_string())?,
    })
}

fn chart_decoder(
    convention: &DecimalConvention,
    provenance: ReplayProvenance,
) -> Result<BinaryMarketBarStreamDecoder, String> {
    BinaryMarketBarStreamDecoder::try_new(
        convention.clone(),
        provenance,
        nonzero(MAXIMUM_INNER_FRAME_BYTES),
        nonzero(MAXIMUM_BUFFERED_BYTES),
    )
    .map_err(|error| error.to_string())
}

fn replay_provenance(value: i32) -> Result<ReplayProvenance, String> {
    match ChartProvenance::try_from(value)
        .map_err(|_| "resident chart provenance is invalid".to_string())?
    {
        ChartProvenance::LocalCache => Ok(ReplayProvenance::LocalCache),
        ChartProvenance::LiveProvider => Ok(ReplayProvenance::LiveProvider),
        ChartProvenance::EmbeddedFixture => Ok(ReplayProvenance::EmbeddedFixture),
    }
}

fn clone_bootstrap(source: &MarketWorkerBootstrap) -> MarketWorkerBootstrap {
    MarketWorkerBootstrap {
        snapshot: source.snapshot.clone(),
        subscription_id: source.subscription_id.clone(),
        generation: source.generation.clone(),
        worker_label: source.worker_label.clone(),
    }
}

fn placeholder_product(product_id: &str) -> CoinbaseSpotProduct {
    let (base, quote) = product_id.split_once('-').unwrap_or((product_id, "USD"));
    CoinbaseSpotProduct {
        product_id: product_id.to_string(),
        instrument_id: coinbase_instrument_id(product_id)
            .unwrap_or_else(|_| product_id.to_string()),
        display_symbol: format!("{base}/{quote}"),
        base_currency: base.to_string(),
        quote_currency: quote.to_string(),
        price_scale: 0,
        quantity_scale: 0,
    }
}

fn interval_seconds(interval: ChartInterval) -> Option<u32> {
    match interval.aggregation() {
        ChartAggregation::FixedSeconds(seconds) => Some(seconds.get()),
        ChartAggregation::CalendarMonth => Some(30 * 24 * 60 * 60),
        ChartAggregation::Trades(_) => None,
    }
}

fn send_error(message_tx: &MarketWorkerSender, message: String) {
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: axiusflow_coinbase_coordinator::market_worker::ChartState::Error,
        message,
    });
}

fn send_recovering(message_tx: &MarketWorkerSender, message: &str) -> bool {
    message_tx
        .send(MarketWorkerMessage::State {
            state: axiusflow_coinbase_coordinator::market_worker::ChartState::Recovering,
            message: format!("Resident engine reconnecting: {message}"),
        })
        .is_ok()
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}
