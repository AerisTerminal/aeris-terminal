//! Single-owner resident market coordinator and Coinbase history worker.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU64, NonZeroUsize},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryCapabilityAdapter, ENTITLEMENT_CLASS,
    decode_history_bar,
};
use axiusflow_local_engine_protocol::{
    DemandError, EngineFaultCode, MarketBar as IpcMarketBar, PersistenceState, SeriesKey,
    SeriesLoadState, SeriesSnapshot as IpcSeriesSnapshot, SeriesState, envelope,
};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use axiusflow_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, GenerationId, MarketEngine, MarketEngineConfig,
    ProviderCapabilities, ProviderGeneration, Viewport, WorkspaceId,
};
use axiusflow_provider_history::{DataClass, HistoryPageRequest, HistoryRange};

const COMMAND_CAPACITY: usize = 64;
const HISTORY_CAPACITY: usize = 8;
const MAXIMUM_CONSUMERS: usize = 256;
const MAXIMUM_SERIES: usize = 128;
const HISTORY_BARS_PER_SERIES: usize = 350;
const MAXIMUM_STORED_BARS: usize = MAXIMUM_SERIES * HISTORY_BARS_PER_SERIES;
const COINBASE_PROVIDER_GENERATION: u64 = 1;

type Reply<T> = SyncSender<Result<T, String>>;

/// Cloneable command boundary for the process-owned market coordinator.
#[derive(Clone)]
pub struct MarketService {
    commands: SyncSender<Command>,
}

enum Command {
    Attach(ClientId, Reply<()>),
    Detach(ClientId, Reply<()>),
    Register(ConsumerIdentity, Reply<()>),
    Remove(ClientId, ConsumerId, Reply<()>),
    Viewport(ClientId, ConsumerId, GenerationId, Viewport, Reply<()>),
    Visibility(ClientId, ConsumerId, bool, Reply<()>),
    Demand(
        ClientId,
        ConsumerId,
        GenerationId,
        BarSeriesKey,
        Reply<Vec<envelope::Payload>>,
    ),
    HistoryCompleted(BarSeriesKey, Result<HistorySnapshot, String>),
}

struct HistoryRequest {
    series: BarSeriesKey,
}

struct HistorySnapshot {
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
}

struct DemandWaiter {
    consumer_id: ConsumerId,
    generation: GenerationId,
    reply: Reply<Vec<envelope::Payload>>,
}

trait HistorySource: Send + 'static {
    fn fetch(&mut self, series: &BarSeriesKey) -> Result<HistorySnapshot, String>;
}

struct LiveCoinbaseHistory {
    adapter: CoinbaseHistoryCapabilityAdapter,
}

#[cfg(test)]
struct FixtureHistory {
    bars: Vec<MarketBar>,
}

#[cfg(test)]
impl HistorySource for FixtureHistory {
    fn fetch(&mut self, _series: &BarSeriesKey) -> Result<HistorySnapshot, String> {
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: self.bars.clone(),
        })
    }
}

impl LiveCoinbaseHistory {
    fn try_new() -> Result<Self, String> {
        CoinbaseHistoryCapabilityAdapter::try_new()
            .map(|adapter| Self { adapter })
            .map_err(|error| error.to_string())
    }
}

impl HistorySource for LiveCoinbaseHistory {
    fn fetch(&mut self, series: &BarSeriesKey) -> Result<HistorySnapshot, String> {
        if series.provider_id != "coinbase"
            || series.instrument_id != "instrument:coinbase:btc:usd"
            || series.period != BarPeriod::time(60).map_err(|error| error.to_string())?
            || series.definition_version != 1
        {
            return Err(
                "the first engine migration slice supports BTC-USD one-minute bars only"
                    .to_string(),
            );
        }
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is unavailable".to_string())?
            .as_secs();
        let end_seconds = now_seconds - now_seconds % 60;
        let end_unix_nanos = i64::try_from(end_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .ok_or_else(|| "Coinbase history end time overflowed".to_string())?;
        let span_nanos = i64::try_from(HISTORY_BARS_PER_SERIES)
            .ok()
            .and_then(|count| count.checked_mul(60_000_000_000))
            .ok_or_else(|| "Coinbase history span overflowed".to_string())?;
        let request = HistoryPageRequest {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            instrument_id: series.instrument_id.clone(),
            data_class: DataClass::Bars,
            resolution: "1m".to_string(),
            range: HistoryRange {
                start_unix_nanos: end_unix_nanos.saturating_sub(span_nanos),
                end_unix_nanos,
            },
            maximum_items: NonZeroUsize::new(HISTORY_BARS_PER_SERIES).unwrap_or(NonZeroUsize::MIN),
            continuation: None,
        };
        let batch = self.adapter.fetch_paginated(&request)?;
        let bars = batch
            .items
            .iter()
            .map(decode_history_bar)
            .collect::<Result<Vec<_>, _>>()?;
        if bars.is_empty() {
            return Err("Coinbase returned no completed historical bars".to_string());
        }
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars,
        })
    }
}

impl MarketService {
    /// Starts the process-owned market coordinator and its single Coinbase history worker.
    ///
    /// # Errors
    /// Returns an error when provider configuration or either bounded worker cannot start.
    pub fn start() -> Result<Self, String> {
        Self::start_with_source(LiveCoinbaseHistory::try_new()?)
    }

    #[cfg(test)]
    /// Starts a deterministic in-memory history source for IPC integration tests.
    ///
    /// # Errors
    /// Returns an error when either bounded worker cannot start.
    pub(crate) fn start_fixture(bars: Vec<MarketBar>) -> Result<Self, String> {
        Self::start_with_source(FixtureHistory { bars })
    }

    fn start_with_source(source: impl HistorySource) -> Result<Self, String> {
        let engine = configured_engine()?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let completion_tx = command_tx.clone();
        thread::Builder::new()
            .name("axiusflow-coinbase-history".to_string())
            .spawn(move || run_history_worker(source, &history_rx, &completion_tx))
            .map_err(|error| error.to_string())?;
        thread::Builder::new()
            .name("axiusflow-market-engine".to_string())
            .spawn(move || run_coordinator(engine, &command_rx, &history_tx))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            commands: command_tx,
        })
    }

    /// Attaches a client identity to resident market state.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn attach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Attach(id(client_id).map(ClientId)?, reply)))
    }

    /// Detaches a client and all of its consumers.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn detach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Detach(id(client_id).map(ClientId)?, reply)))
    }

    /// Registers one market consumer owned by an attached client.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, bounds, or coordinator failure.
    pub fn register_consumer(
        &self,
        client_id: u64,
        workspace_id: u64,
        consumer_id: u64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Register(
                ConsumerIdentity {
                    client_id: ClientId(id(client_id)?),
                    workspace_id: WorkspaceId(id(workspace_id)?),
                    consumer_id: ConsumerId(id(consumer_id)?),
                },
                reply,
            ))
        })
    }

    /// Removes one market consumer.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn remove_consumer(&self, client_id: u64, consumer_id: u64) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Remove(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    /// Applies a generation-fenced viewport to one consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, range, generation, or coordinator failure.
    pub fn set_viewport(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Viewport(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                Viewport::try_new(start_unix_nanos, end_unix_nanos)
                    .map_err(|error| error.to_string())?,
                reply,
            ))
        })
    }

    /// Updates one consumer's presentation priority.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_visibility(
        &self,
        client_id: u64,
        consumer_id: u64,
        visible: bool,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Visibility(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                visible,
                reply,
            ))
        })
    }

    /// Resolves a covering snapshot for one generation-fenced series demand.
    ///
    /// # Errors
    /// Returns an error for invalid demand, unavailable coordinator, or failed reply delivery.
    pub fn set_demand(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &SeriesKey,
    ) -> Result<Vec<envelope::Payload>, String> {
        self.request(|reply| {
            Ok(Command::Demand(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                internal_series(series)?,
                reply,
            ))
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let command = build(reply_tx)?;
        self.commands
            .send(command)
            .map_err(|_| "market engine coordinator is unavailable".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "market engine coordinator stopped before replying".to_string())?
    }
}

fn run_history_worker(
    mut source: impl HistorySource,
    requests: &Receiver<HistoryRequest>,
    completions: &SyncSender<Command>,
) {
    while let Ok(request) = requests.recv() {
        let result = source.fetch(&request.series);
        if completions
            .send(Command::HistoryCompleted(request.series, result))
            .is_err()
        {
            return;
        }
    }
}

fn run_coordinator(
    mut engine: MarketEngine,
    commands: &Receiver<Command>,
    history: &SyncSender<HistoryRequest>,
) {
    let mut attached = BTreeSet::<ClientId>::new();
    let mut pending = BTreeMap::<BarSeriesKey, Vec<DemandWaiter>>::new();
    while let Ok(command) = commands.recv() {
        match command {
            Command::Attach(client_id, reply) => {
                let result = attached
                    .insert(client_id)
                    .then_some(())
                    .ok_or_else(|| "client identity is already attached".to_string());
                let _ = reply.send(result);
            }
            Command::Detach(client_id, reply) => {
                attached.remove(&client_id);
                engine.detach_client(client_id);
                let _ = reply.send(Ok(()));
            }
            Command::Register(identity, reply) => {
                let result = if attached.contains(&identity.client_id) {
                    engine
                        .register_consumer(identity, true)
                        .map_err(|error| error.to_string())
                } else {
                    Err("client must attach before registering consumers".to_string())
                };
                let _ = reply.send(result);
            }
            Command::Remove(client_id, consumer_id, reply) => {
                let result = authorize_consumer(&engine, client_id, consumer_id).map(|()| {
                    engine.remove_consumer(consumer_id);
                });
                let _ = reply.send(result);
            }
            Command::Viewport(client_id, consumer_id, generation, viewport, reply) => {
                let result = authorize_consumer(&engine, client_id, consumer_id).and_then(|()| {
                    engine
                        .set_viewport(consumer_id, generation, viewport)
                        .map_err(|error| error.to_string())
                });
                let _ = reply.send(result);
            }
            Command::Visibility(client_id, consumer_id, visible, reply) => {
                let result = authorize_consumer(&engine, client_id, consumer_id).and_then(|()| {
                    engine
                        .set_visibility(consumer_id, visible)
                        .map_err(|error| error.to_string())
                });
                let _ = reply.send(result);
            }
            Command::Demand(client_id, consumer_id, generation, series, reply) => {
                handle_demand(
                    &mut engine,
                    &mut pending,
                    history,
                    client_id,
                    &series,
                    DemandWaiter {
                        consumer_id,
                        generation,
                        reply,
                    },
                );
            }
            Command::HistoryCompleted(series, result) => {
                let Some(waiters) = pending.remove(&series) else {
                    continue;
                };
                match result {
                    Ok(snapshot) => match engine.install_history(
                        ProviderGeneration(
                            NonZeroU64::new(COINBASE_PROVIDER_GENERATION)
                                .unwrap_or(NonZeroU64::MIN),
                        ),
                        &series,
                        snapshot.price_scale,
                        snapshot.quantity_scale,
                        snapshot.bars,
                    ) {
                        Ok(_) => complete_waiters(&engine, waiters),
                        Err(error) => fail_waiters(waiters, &error.to_string()),
                    },
                    Err(_) => fail_waiters(waiters, "Coinbase historical bars are unavailable"),
                }
            }
        }
    }
}

fn handle_demand(
    engine: &mut MarketEngine,
    pending: &mut BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    history: &SyncSender<HistoryRequest>,
    client_id: ClientId,
    series: &BarSeriesKey,
    waiter: DemandWaiter,
) {
    let demand = authorize_consumer(engine, client_id, waiter.consumer_id).and_then(|()| {
        engine
            .set_series_demand(waiter.consumer_id, waiter.generation, series)
            .map_err(|error| error.to_string())
    });
    match demand {
        Ok(Some(publication)) => {
            let _ = waiter.reply.send(Ok(ready_messages(&publication)));
        }
        Ok(None) => {
            let first = !pending.contains_key(series);
            pending.entry(series.clone()).or_default().push(waiter);
            if first {
                enqueue_history(history, pending, series);
            }
        }
        Err(error) => {
            let _ = waiter.reply.send(Err(error));
        }
    }
}

fn enqueue_history(
    history: &SyncSender<HistoryRequest>,
    pending: &mut BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    series: &BarSeriesKey,
) {
    let request = HistoryRequest {
        series: series.clone(),
    };
    let failure = match history.try_send(request) {
        Ok(()) => return,
        Err(TrySendError::Full(_)) => "Coinbase history capacity is temporarily exhausted",
        Err(TrySendError::Disconnected(_)) => "Coinbase history worker is unavailable",
    };
    if let Some(waiters) = pending.remove(series) {
        fail_waiters(waiters, failure);
    }
}

fn authorize_consumer(
    engine: &MarketEngine,
    client_id: ClientId,
    consumer_id: ConsumerId,
) -> Result<(), String> {
    engine
        .current_demand(consumer_id)
        .filter(|demand| demand.identity.client_id == client_id)
        .map(|_| ())
        .ok_or_else(|| "consumer is not owned by the attached client".to_string())
}

fn configured_engine() -> Result<MarketEngine, String> {
    let mut engine = MarketEngine::new(MarketEngineConfig {
        maximum_consumers: NonZeroUsize::new(MAXIMUM_CONSUMERS).unwrap_or(NonZeroUsize::MIN),
        maximum_series: NonZeroUsize::new(MAXIMUM_SERIES).unwrap_or(NonZeroUsize::MIN),
        maximum_bars: NonZeroUsize::new(MAXIMUM_STORED_BARS).unwrap_or(NonZeroUsize::MIN),
    });
    engine
        .register_provider(
            "coinbase".to_string(),
            ProviderCapabilities {
                historical_bars: true,
                realtime_bars: false,
            },
        )
        .map_err(|error| error.to_string())?;
    engine
        .begin_provider_session(
            "coinbase",
            ProviderGeneration(
                NonZeroU64::new(COINBASE_PROVIDER_GENERATION).unwrap_or(NonZeroU64::MIN),
            ),
        )
        .map_err(|error| error.to_string())?;
    Ok(engine)
}

fn complete_waiters(engine: &MarketEngine, waiters: Vec<DemandWaiter>) {
    for waiter in waiters {
        let response = engine
            .latest_publication(waiter.consumer_id)
            .filter(|publication| publication.generation == waiter.generation)
            .map_or_else(
                || Ok(superseded_messages(waiter.consumer_id, waiter.generation)),
                |publication| Ok(ready_messages(publication)),
            );
        let _ = waiter.reply.send(response);
    }
}

fn fail_waiters(waiters: Vec<DemandWaiter>, detail: &str) {
    for waiter in waiters {
        let _ = waiter.reply.send(Ok(failed_messages(
            waiter.consumer_id,
            waiter.generation,
            detail,
        )));
    }
}

fn ready_messages(
    publication: &axiusflow_market_engine::ConsumerPublication,
) -> Vec<envelope::Payload> {
    let series = ipc_series(&publication.snapshot.series);
    vec![
        series_state(
            publication.consumer_id,
            publication.generation,
            series.clone(),
            SeriesLoadState::Resolving,
            None,
        ),
        envelope::Payload::SeriesSnapshot(IpcSeriesSnapshot {
            consumer_id: publication.consumer_id.0.get(),
            generation: publication.generation.0.get(),
            series: Some(series.clone()),
            provider_generation: publication.snapshot.provider_generation.0.get(),
            price_scale: u32::from(publication.snapshot.price_scale),
            quantity_scale: u32::from(publication.snapshot.quantity_scale),
            bars: publication
                .snapshot
                .bars
                .iter()
                .copied()
                .map(ipc_bar)
                .collect(),
            publication_generation: publication.publication_generation,
        }),
        series_state(
            publication.consumer_id,
            publication.generation,
            series,
            SeriesLoadState::Ready,
            None,
        ),
    ]
}

fn superseded_messages(
    consumer_id: ConsumerId,
    generation: GenerationId,
) -> Vec<envelope::Payload> {
    vec![series_state(
        consumer_id,
        generation,
        SeriesKey::default(),
        SeriesLoadState::Superseded,
        Some("a newer consumer generation replaced this demand".to_string()),
    )]
}

fn failed_messages(
    consumer_id: ConsumerId,
    generation: GenerationId,
    detail: &str,
) -> Vec<envelope::Payload> {
    vec![
        series_state(
            consumer_id,
            generation,
            SeriesKey::default(),
            SeriesLoadState::Failed,
            Some(detail.to_string()),
        ),
        envelope::Payload::DemandError(DemandError {
            consumer_id: consumer_id.0.get(),
            generation: generation.0.get(),
            code: EngineFaultCode::Retryable as i32,
            stage: "provider_history".to_string(),
            detail: detail.to_string(),
        }),
    ]
}

fn series_state(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: SeriesKey,
    state: SeriesLoadState,
    detail: Option<String>,
) -> envelope::Payload {
    envelope::Payload::SeriesState(SeriesState {
        consumer_id: consumer_id.0.get(),
        generation: generation.0.get(),
        series: Some(series),
        state: state as i32,
        persistence: PersistenceState::NotRequested as i32,
        detail,
    })
}

fn id(value: u64) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| "market identity must be non-zero".to_string())
}

fn internal_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    if series.provider.trim().is_empty()
        || series.instrument_id.trim().is_empty()
        || series.definition_revision == 0
    {
        return Err("market series identity is invalid".to_string());
    }
    Ok(BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        period: BarPeriod::time(series.interval_seconds).map_err(|error| error.to_string())?,
        definition_version: series.definition_revision,
    })
}

fn ipc_series(series: &BarSeriesKey) -> SeriesKey {
    SeriesKey {
        provider: series.provider_id.clone(),
        instrument_id: series.instrument_id.clone(),
        interval_seconds: match series.period {
            BarPeriod::Time { seconds } => seconds,
            BarPeriod::Tick { .. } | BarPeriod::Daily => 0,
        },
        definition_revision: series.definition_version,
    }
}

const fn ipc_bar(bar: MarketBar) -> IpcMarketBar {
    IpcMarketBar {
        source_sequence: bar.source_sequence,
        exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: bar.volume,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    struct ControlledHistory {
        fetches: Arc<AtomicUsize>,
        release: Receiver<()>,
    }

    impl HistorySource for ControlledHistory {
        fn fetch(&mut self, _series: &BarSeriesKey) -> Result<HistorySnapshot, String> {
            self.fetches.fetch_add(1, Ordering::AcqRel);
            self.release
                .recv()
                .map_err(|_| "test history release disconnected".to_string())?;
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: vec![MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: 60,
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 7,
                }],
            })
        }
    }

    fn btc() -> SeriesKey {
        SeriesKey {
            provider: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            interval_seconds: 60,
            definition_revision: 1,
        }
    }

    #[test]
    fn later_consumers_reuse_one_engine_history_fetch() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let service = MarketService::start_with_source(ControlledHistory {
            fetches: Arc::clone(&fetches),
            release: release_rx,
        })
        .expect("test service starts");
        for client in 1..=2 {
            service.attach(client).expect("client attaches");
            service
                .register_consumer(client, 1, client)
                .expect("consumer registers");
        }
        assert_eq!(
            service.attach(1).expect_err("duplicate client is rejected"),
            "client identity is already attached"
        );
        assert_eq!(
            service
                .set_visibility(1, 2, false)
                .expect_err("another client's consumer is rejected"),
            "consumer is not owned by the attached client"
        );
        let first = service.clone();
        let first_request = thread::spawn(move || first.set_demand(1, 1, 1, &btc()));
        while fetches.load(Ordering::Acquire) == 0 {
            thread::yield_now();
        }
        release_tx.send(()).expect("history released");
        let first_messages = first_request
            .join()
            .expect("request thread joins")
            .expect("first demand succeeds");
        let second_messages = service
            .set_demand(2, 2, 1, &btc())
            .expect("cache hit succeeds");
        for messages in [first_messages, second_messages] {
            assert!(messages.iter().any(|message| {
                matches!(message, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.bars.len() == 1)
            }));
        }
        assert_eq!(fetches.load(Ordering::Acquire), 1);
    }

    #[test]
    fn full_history_queue_fails_waiters_without_blocking_the_coordinator() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        history_tx
            .try_send(HistoryRequest {
                series: internal_series(&btc()).expect("first series"),
            })
            .expect("fill history queue");
        let mut second = internal_series(&btc()).expect("second series");
        second.instrument_id = "instrument:coinbase:eth:usd".to_string();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let mut pending = BTreeMap::from([(
            second.clone(),
            vec![DemandWaiter {
                consumer_id: ConsumerId(id(1).expect("consumer id")),
                generation: GenerationId(id(1).expect("generation")),
                reply: reply_tx,
            }],
        )]);

        enqueue_history(&history_tx, &mut pending, &second);

        assert!(pending.is_empty());
        let messages = reply_rx
            .recv()
            .expect("waiter receives overload result")
            .expect("overload is a protocol response");
        assert!(messages.iter().any(|message| {
            matches!(message, envelope::Payload::DemandError(error) if error.detail.contains("capacity"))
        }));
    }
}
