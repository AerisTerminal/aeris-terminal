//! Desktop-side client for engine-owned Coinbase historical and realtime bars.

use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{Arc, atomic::AtomicU64, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, Provenanced,
    ReplayProvenance, ReplayRecoveryCommand, ReplaySnapshot, ReplayStreamUpdate,
};
use axiusflow_coinbase_market_adapter::{CoinbaseSpotProduct, ENTITLEMENT_CLASS};
use axiusflow_engine::{EngineClient, connect_or_start_engine, sibling_engine_executable};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_local_engine_protocol::{
    DemandError, EngineFaultCode, ProviderConnectionState, ProviderState, SeriesKey,
    SeriesLoadState, SeriesSnapshot, envelope,
};
use axiusflow_market_data::{BarDefinition, ChartInterval, MarketBar};
use axiusflow_observability::FeedConnectionState;

use crate::resident_market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerCommand, MarketWorkerMessage, MarketWorkerPublication, MarketWorkerSender,
    MarketWorkerStartup, market_worker_channel,
};

const WORKSPACE_ID: u64 = 1;
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
    let client_id = random_identity()?;
    let consumer_id = random_identity()?;
    let startup = MarketWorkerStartup::Loading(Box::new(
        axiusflow_desktop_market_runtime::market_worker::CoinbaseWorkerStartup {
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
    thread::Builder::new()
        .name("axiusflow-engine-market-client".to_string())
        .spawn(move || {
            let error_tx = message_tx.clone();
            if let Err(error) =
                run_worker(client_id, consumer_id, &product, &message_tx, &command_rx)
            {
                let _ = error_tx.send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: error,
                });
            }
            let _ = shutdown_tx.send(());
        })
        .map_err(|error| error.to_string())?;
    Ok((
        startup,
        MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            None,
            Some(selection_sequence),
        ),
    ))
}

fn run_worker(
    client_id: u64,
    consumer_id: u64,
    product: &CoinbaseSpotProduct,
    messages: &MarketWorkerSender,
    commands: &mpsc::Receiver<MarketWorkerCommand>,
) -> Result<(), String> {
    let _ = messages.send(MarketWorkerMessage::Connection {
        state: FeedConnectionState::Discovering,
        message: "Connecting to the resident market engine".to_string(),
    });
    let executable = sibling_engine_executable()?;
    let mut client = connect_or_start_engine(&executable)?;
    client.attach_client(client_id)?;
    client.register_consumer(client_id, WORKSPACE_ID, consumer_id)?;
    let _ = messages.send(MarketWorkerMessage::CoinbaseCatalog(
        Ok(coinbase_products()),
    ));
    let mut model = empty_model();
    let (snapshot, generation) = request_snapshot(
        &mut client,
        consumer_id,
        INITIAL_GENERATION,
        series_key(product, ChartInterval::Minute1)?,
        &mut model,
        messages,
    )?;
    send_publication(messages, snapshot, generation)?;
    let _ = messages.send(MarketWorkerMessage::Connection {
        state: FeedConnectionState::Discovering,
        message: "Historical bars are visible; Coinbase realtime is connecting".to_string(),
    });
    let mut active_generation = INITIAL_GENERATION;

    loop {
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(command) => match command {
                MarketWorkerCommand::CoinbaseSelect(request) => {
                    let series = match series_key(&request.product, request.interval) {
                        Ok(series) => series,
                        Err(error) => {
                            let _ = messages.send(MarketWorkerMessage::State {
                                state: ChartState::Error,
                                message: error,
                            });
                            continue;
                        }
                    };
                    let _ = messages.send(MarketWorkerMessage::CoinbaseSwitchMarker {
                        sequence: request.sequence,
                    });
                    let _ = messages.send(MarketWorkerMessage::State {
                        state: ChartState::Loading,
                        message: "Loading Coinbase history through the resident engine".to_string(),
                    });
                    model = empty_model();
                    active_generation = request.sequence;
                    client.set_series_demand(consumer_id, request.sequence, series)?;
                }
                MarketWorkerCommand::Recovery(command) => {
                    send_recovery(
                        &mut client,
                        consumer_id,
                        active_generation,
                        command,
                        &mut model,
                        messages,
                    )?;
                }
                MarketWorkerCommand::ChartViewport(viewport) => {
                    if viewport.selection_generation > 0 {
                        client.set_market_viewport(
                            consumer_id,
                            viewport.selection_generation,
                            viewport.start_unix_nanos,
                            viewport.end_unix_nanos,
                        )?;
                    }
                }
                MarketWorkerCommand::Shutdown => break,
                MarketWorkerCommand::RithmicSearch(_)
                | MarketWorkerCommand::RithmicSelect(_)
                | MarketWorkerCommand::RithmicHistory(_) => {
                    return Err(
                        "Rithmic commands cannot enter the Coinbase engine client".to_string()
                    );
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Some(event) = client.poll_market_event(consumer_id)? {
            apply_polled_event(event, consumer_id, active_generation, &mut model, messages)?;
        }
    }
    let _ = client.remove_market_consumer(consumer_id);
    let _ = client.detach_client(client_id);
    Ok(())
}

fn apply_polled_event(
    event: envelope::Payload,
    consumer_id: u64,
    active_generation: u64,
    model: &mut MarketBarClientModel,
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
            send_publication(messages, replay, generation)
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
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let result = request_snapshot(
        client,
        consumer_id,
        active_generation,
        SeriesKey {
            provider: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            interval_seconds: 60,
            definition_revision: 1,
        },
        model,
        messages,
    )
    .map(|(snapshot, generation)| MarketWorkerBootstrap {
        snapshot,
        subscription_id: SUBSCRIPTION_ID.to_string(),
        generation,
        worker_label: WORKER_LABEL.to_string(),
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
    snapshot: ReplaySnapshot,
    generation: DesktopMarketGeneration,
) -> Result<(), String> {
    messages
        .send(MarketWorkerMessage::Update(MarketWorkerPublication {
            update: ReplayStreamUpdate::Snapshot(snapshot),
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
    let price_scale = u8::try_from(snapshot.price_scale)
        .map_err(|_| "engine price scale is invalid".to_string())?;
    let quantity_scale = u8::try_from(snapshot.quantity_scale)
        .map_err(|_| "engine quantity scale is invalid".to_string())?;
    let product = coinbase_products()
        .into_iter()
        .find(|product| product.instrument_id == series.instrument_id)
        .ok_or_else(|| "engine snapshot instrument is unsupported".to_string())?;
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
            series.provider, series.instrument_id, series.interval_seconds
        ),
        version: series.definition_revision,
        interval_seconds: series.interval_seconds,
        trades_per_bar: None,
    };
    let received = now_unix_nanos();
    let bars = snapshot
        .bars
        .iter()
        .map(|bar| {
            let bar = MarketBar {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
            };
            let exchange = bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
            Provenanced::new(
                bar,
                MarketEventProvenance {
                    event_id: format!(
                        "engine-{}-{}-{}",
                        snapshot.provider_generation, snapshot.generation, bar.source_sequence
                    ),
                    event_time_unix_nanos: exchange,
                    publication_time_unix_nanos: received,
                    producer: "axiusflow_engine".to_string(),
                    schema_version: 1,
                    correlation_id: format!(
                        "engine-series-{}-{}",
                        snapshot.consumer_id, snapshot.generation
                    ),
                    causation_id: String::new(),
                    entitlement_revision: ENTITLEMENT_CLASS.to_string(),
                    session_generation: snapshot.provider_generation,
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

fn series_key(product: &CoinbaseSpotProduct, interval: ChartInterval) -> Result<SeriesKey, String> {
    let supported_product = coinbase_products().into_iter().any(|supported| {
        product.product_id == supported.product_id
            && product.instrument_id == supported.instrument_id
            && product.price_scale == supported.price_scale
            && product.quantity_scale == supported.quantity_scale
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
        interval_seconds: match interval {
            ChartInterval::Minute1 => 60,
            ChartInterval::Minute5 => 300,
            ChartInterval::Minute15 => 900,
            ChartInterval::Hour1 => 3_600,
            _ => unreachable!("supported intervals were validated above"),
        },
        definition_revision: 1,
    })
}

fn coinbase_products() -> Vec<CoinbaseSpotProduct> {
    [("BTC", "btc"), ("ETH", "eth")]
        .into_iter()
        .map(|(base, canonical)| CoinbaseSpotProduct {
            product_id: format!("{base}-USD"),
            instrument_id: format!("instrument:coinbase:{canonical}:usd"),
            display_symbol: format!("{base}/USD"),
            base_currency: base.to_string(),
            quote_currency: "USD".to_string(),
            price_scale: 2,
            quantity_scale: 8,
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
                assert_eq!(series.interval_seconds, seconds);
                assert_eq!(series.instrument_id, product.instrument_id);
            }
        }
    }
}
