use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, atomic::AtomicU64, mpsc},
    thread,
};

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, ReplayProvenance, ReplayStreamUpdate,
};
use axiusflow_coinbase_coordinator::market_worker::{
    CoinbaseWorkerStartup, MarketDataWorker, MarketWorkerBootstrap, MarketWorkerCommand,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerSender, market_worker_channel,
};
use axiusflow_coinbase_market_adapter::{CoinbaseSpotProduct, coinbase_instrument_id};
use axiusflow_engine::{
    ENGINE_SOCKET_NAME, EngineClient, connect_or_start_engine, native_installation_token,
    sibling_engine_executable,
};
use axiusflow_local_engine_protocol::{
    CatalogReassembler, ChartProvenance, ProviderConnectionState, ViewKind, envelope,
};
use axiusflow_market_data::{ChartAggregation, ChartInterval};
use axiusflow_market_protocol_adapter::{BinaryMarketBarStreamDecoder, DecimalConvention};
use axiusflow_observability::FeedConnectionState;

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
const MAXIMUM_INNER_FRAME_BYTES: usize = 900 * 1024;
const MAXIMUM_BUFFERED_BYTES: usize = 2 * MAXIMUM_INNER_FRAME_BYTES;
const MODEL_ITEM_CAPACITY: usize = 20_000;
const WORKER_LABEL: &str = "Resident Coinbase market engine";
const SUBSCRIPTION_ID: &str = "resident_coinbase_market_bars";

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
    let mut commands = match connect_or_start_engine(&executable) {
        Ok(client) => client,
        Err(error) => {
            send_error(message_tx, error);
            return;
        }
    };
    let mut workspace = match commands.restore_workspace() {
        Ok(workspace) => workspace,
        Err(error) => {
            send_error(message_tx, error);
            return;
        }
    };
    let latest_snapshot = Arc::new(Mutex::new(None));
    spawn_session_stream(message_tx.clone(), Arc::clone(&latest_snapshot));
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: axiusflow_coinbase_coordinator::market_worker::ChartState::Loading,
        message: "Attached to the resident market engine".to_string(),
    });

    while let Ok(command) = command_rx.recv() {
        match command {
            MarketWorkerCommand::CoinbaseSelect(request) => {
                let Some(interval_seconds) = interval_seconds(request.interval) else {
                    send_error(message_tx, "unsupported Coinbase interval".to_string());
                    continue;
                };
                match commands.set_selection(
                    request.product.product_id,
                    interval_seconds,
                    workspace.workspace_revision,
                    request.sequence,
                ) {
                    Ok(updated) => {
                        workspace = updated;
                        let _ = message_tx.send(MarketWorkerMessage::CoinbaseSwitchMarker {
                            sequence: request.sequence,
                        });
                    }
                    Err(error) => send_error(message_tx, error),
                }
            }
            MarketWorkerCommand::Recovery(command) => {
                let result = latest_snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .map(clone_bootstrap)
                    .ok_or_else(|| {
                        "resident engine has not published a covering snapshot".to_string()
                    });
                let _ = message_tx.send(MarketWorkerMessage::Recovery {
                    request_id: command.request_id,
                    result,
                });
            }
            MarketWorkerCommand::Shutdown => break,
            MarketWorkerCommand::RithmicSearch(_)
            | MarketWorkerCommand::RithmicSelect(_)
            | MarketWorkerCommand::RithmicHistory(_) => {}
        }
    }
}

fn spawn_session_stream(message_tx: MarketWorkerSender, latest_snapshot: LatestSnapshot) {
    let stream_error_tx = message_tx.clone();
    spawn_stream(
        "axiusflow-engine-session-stream",
        stream_error_tx,
        move || {
            let mut stream = connect_stream(ViewKind::Session)?;
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
                        if let Some(entries) =
                            catalog.push(snapshot).map_err(|error| error.to_string())?
                        {
                            publish_catalog(&message_tx, entries)?;
                        }
                        continue;
                    }
                    envelope::Payload::ProviderState(provider) => {
                        publish_provider_state(&message_tx, provider.state)?;
                        continue;
                    }
                    envelope::Payload::Fault(fault) => {
                        send_error(&message_tx, fault.redacted_detail);
                        continue;
                    }
                    _ => continue,
                };
                if is_snapshot && next_provenance != provenance {
                    decoder = chart_decoder(&convention, next_provenance)?;
                    model = MarketBarClientModel::new(nonzero(MODEL_ITEM_CAPACITY));
                    provenance = next_provenance;
                }
                for projected in decoder.push(&payload).map_err(|error| error.to_string())? {
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
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(MarketWorkerBootstrap {
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
            }
        },
    );
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

fn spawn_stream(
    name: &str,
    error_tx: MarketWorkerSender,
    run: impl FnOnce() -> Result<(), String> + Send + 'static,
) {
    let name = name.to_string();
    let _ = thread::Builder::new().name(name).spawn(move || {
        if let Err(error) = run() {
            eprintln!("resident engine stream stopped: {error}");
            send_error(&error_tx, error);
        }
    });
}

fn connect_stream(view: ViewKind) -> Result<axiusflow_engine::EngineViewStream, String> {
    let token = native_installation_token()?;
    EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())?.subscribe_view(view)
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

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}
