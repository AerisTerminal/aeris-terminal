use std::{
    num::NonZeroUsize,
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
    ProviderConnectionState, ViewKind, WorkspaceState, envelope,
};
use axiusflow_market_data::{
    ChartAggregation, ChartInterval, DomColumnLevel, DomFrame, DomRow, OrderBookRecoveryReason,
    OrderBookState,
};
use axiusflow_market_protocol_adapter::{BinaryMarketBarStreamDecoder, DecimalConvention};
use axiusflow_observability::FeedConnectionState;

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
const MAXIMUM_INNER_FRAME_BYTES: usize = 900 * 1024;
const MAXIMUM_BUFFERED_BYTES: usize = 2 * MAXIMUM_INNER_FRAME_BYTES;
const MODEL_ITEM_CAPACITY: usize = 20_000;
const WORKER_LABEL: &str = "Resident Coinbase market engine";
const SUBSCRIPTION_ID: &str = "resident_coinbase_market_bars";
const VIEWPORT_PERSIST_DELAY: Duration = Duration::from_millis(250);

type LatestSnapshot = Arc<Mutex<Option<MarketWorkerBootstrap>>>;

pub(super) fn start() -> (
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
            run_bridge(&message_tx, &command_rx);
            let _ = shutdown_tx.send(());
        })
        .expect("resident engine bridge thread starts");

    let startup = axiusflow_coinbase_coordinator::market_worker::MarketWorkerStartup::Loading(
        Box::new(CoinbaseWorkerStartup {
            coinbase_product: placeholder_product("BTC-USD"),
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: WORKER_LABEL.to_string(),
        }),
    );
    let worker =
        MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, Some(sequence));
    (startup, worker)
}

fn run_bridge(message_tx: &MarketWorkerSender, command_rx: &mpsc::Receiver<MarketWorkerCommand>) {
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
    loop {
        let command = if let Some(deadline) = viewport_deadline {
            match command_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    viewport_deadline = None;
                    if let Some(viewport) = pending_viewport.take()
                        && let Err(error) = set_viewport_reconnecting(
                            &mut commands,
                            &mut workspace,
                            &executable,
                            viewport,
                        )
                    {
                        eprintln!("resident viewport was not persisted: {error}");
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            let Ok(command) = command_rx.recv() else {
                break;
            };
            command
        };
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
            MarketWorkerCommand::RithmicSearch(_)
            | MarketWorkerCommand::RithmicSelect(_)
            | MarketWorkerCommand::RithmicHistory(_) => {}
        }
    }
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
                FeedConnectionState::Streaming => "Resident Coinbase stream is live".to_string(),
                _ => "Resident Coinbase stream is reconnecting".to_string(),
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
    message_tx
        .send(MarketWorkerMessage::CoinbaseDom(DomFrame {
            provider_id: snapshot.provider_id,
            instrument_id: snapshot.instrument_id,
            entitlement_id: snapshot.entitlement_id,
            session_generation: snapshot.provider_generation,
            selection_generation: snapshot.selection_generation,
            revision: snapshot.revision,
            source_watermark: snapshot.source_watermark,
            state,
            rows,
        }))
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
