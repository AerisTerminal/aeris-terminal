pub(crate) use crate::rithmic_series::{RithmicSeries, RithmicSeriesRequest};

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, Provenanced,
    ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
};
use axiusflow_desktop_provider_runtime::InstrumentDescriptor;
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_local_engine_client::{
    EngineClient, connect_or_start_engine, sibling_engine_executable,
};
use axiusflow_local_engine_protocol::{
    DemandError, InstallProviderInstrument, SeriesCadence, SeriesKey, SeriesLoadState,
    SeriesSnapshot, envelope,
};
use axiusflow_market_data::{BarDefinition, ChartAggregation, ChartInterval, MarketBar};
use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::market_worker::{MarketWorkerBootstrap, MarketWorkerMessage};

pub(crate) const MAXIMUM_VISIBLE_BARS: usize = 300;
const HISTORY_COMMAND_CAPACITY: usize = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const HISTORY_TIMEOUT: Duration = Duration::from_secs(35);
const ENGINE_WORKSPACE_ID: u64 = 1;

#[derive(Clone)]
pub(crate) struct InstalledRithmicInstrument {
    pub(crate) session_generation: u64,
    pub(crate) selection_generation: NonZeroUsize,
    pub(crate) descriptor: InstrumentDescriptor,
    pub(crate) entitlement_id: String,
}

struct HistoryFetchRequest {
    selection_generation: NonZeroUsize,
    series_generation: NonZeroUsize,
    series: RithmicSeries,
    instrument: InstalledRithmicInstrument,
    stop: Arc<AtomicBool>,
}

pub(crate) struct RithmicHistoryResult {
    pub(crate) selection_generation: NonZeroUsize,
    pub(crate) series_generation: NonZeroUsize,
    pub(crate) result: Result<Box<MarketWorkerBootstrap>, String>,
}

enum HistoryCommand {
    Fetch(HistoryFetchRequest),
}

#[derive(Default)]
struct LatestHistoryResult {
    value: Mutex<Option<RithmicHistoryResult>>,
}

impl LatestHistoryResult {
    fn publish(&self, result: RithmicHistoryResult) {
        *self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
    }

    fn take(&self) -> Option<RithmicHistoryResult> {
        self.value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

pub(crate) struct RithmicHistoryTask {
    commands: Option<SyncSender<HistoryCommand>>,
    results: Arc<LatestHistoryResult>,
    active: Option<(NonZeroUsize, NonZeroUsize, Arc<AtomicBool>)>,
    handle: Option<JoinHandle<()>>,
}

impl RithmicHistoryTask {
    pub(crate) fn start() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::sync_channel(HISTORY_COMMAND_CAPACITY);
        let results = Arc::new(LatestHistoryResult::default());
        let worker_results = Arc::clone(&results);
        let handle = thread::Builder::new()
            .name("axiusflow-rithmic-engine-history-client".to_string())
            .spawn(move || run_history_worker(&command_rx, &worker_results))
            .map_err(|_| "Rithmic engine history client is unavailable".to_string())?;
        Ok(Self {
            commands: Some(command_tx),
            results,
            active: None,
            handle: Some(handle),
        })
    }

    pub(crate) fn request(
        &mut self,
        request: RithmicSeriesRequest,
        instrument: InstalledRithmicInstrument,
    ) -> Result<(), String> {
        if request.selection_generation != instrument.selection_generation {
            return Err("Rithmic history selection is stale".to_string());
        }
        if instrument.session_generation == 0 || !request.series.supports_native_history() {
            return Err("Rithmic history series is unavailable".to_string());
        }
        self.cancel();
        let stop = Arc::new(AtomicBool::new(false));
        let command = HistoryCommand::Fetch(HistoryFetchRequest {
            selection_generation: request.selection_generation,
            series_generation: request.series_generation,
            series: request.series,
            instrument,
            stop: Arc::clone(&stop),
        });
        self.commands
            .as_ref()
            .ok_or_else(|| "Rithmic engine history client stopped".to_string())?
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => "Rithmic engine history client is busy".to_string(),
                TrySendError::Disconnected(_) => {
                    "Rithmic engine history client stopped".to_string()
                }
            })?;
        self.active = Some((
            request.selection_generation,
            request.series_generation,
            stop,
        ));
        Ok(())
    }

    pub(crate) fn try_recv(&mut self) -> Option<RithmicHistoryResult> {
        let result = self.results.take()?;
        if result.result.is_err()
            && self
                .active
                .as_ref()
                .is_some_and(|(selection_generation, series_generation, _)| {
                    *selection_generation == result.selection_generation
                        && *series_generation == result.series_generation
                })
        {
            self.active = None;
        }
        Some(result)
    }

    pub(crate) fn cancel(&mut self) {
        if let Some((_, _, stop)) = self.active.take() {
            stop.store(true, Ordering::Release);
        }
    }

    pub(crate) const fn has_active_request(&self) -> bool {
        self.active.is_some()
    }
}

impl Drop for RithmicHistoryTask {
    fn drop(&mut self) {
        self.cancel();
        self.commands.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct EngineHistorySession {
    client: EngineClient,
    client_id: u64,
    consumer_id: u64,
}

impl EngineHistorySession {
    fn connect() -> Result<Self, String> {
        let executable = sibling_engine_executable()?;
        let mut client = connect_or_start_engine(&executable)?;
        let client_id = random_identity()?;
        let consumer_id = random_identity()?;
        client.attach_client(client_id)?;
        if let Err(error) = client.register_consumer(client_id, ENGINE_WORKSPACE_ID, consumer_id) {
            let _ = client.detach_client(client_id);
            return Err(error);
        }
        Ok(Self {
            client,
            client_id,
            consumer_id,
        })
    }

    fn fetch(&mut self, request: &HistoryFetchRequest) -> Result<MarketWorkerBootstrap, String> {
        let series = engine_series_key(request)?;
        self.client
            .install_provider_instrument(engine_instrument(&request.instrument)?)?;
        self.client.set_series_demand(
            self.consumer_id,
            u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX),
            series.clone(),
        )?;
        let deadline = Instant::now() + HISTORY_TIMEOUT;
        loop {
            if request.stop.load(Ordering::Acquire) {
                self.reset_consumer()?;
                return Err("Rithmic history request was cancelled".to_string());
            }
            if Instant::now() >= deadline {
                self.reset_consumer()?;
                return Err("Rithmic engine history request timed out".to_string());
            }
            let Some(bootstrap) = self.poll_update(request)? else {
                thread::sleep(POLL_INTERVAL);
                continue;
            };
            return Ok(bootstrap);
        }
    }

    fn poll_update(
        &mut self,
        request: &HistoryFetchRequest,
    ) -> Result<Option<MarketWorkerBootstrap>, String> {
        let series = engine_series_key(request)?;
        let Some(event) = self.client.poll_market_event(self.consumer_id)? else {
            return Ok(None);
        };
        match event {
            envelope::Payload::SeriesSnapshot(snapshot) => {
                if snapshot.consumer_id != self.consumer_id
                    || snapshot.generation
                        != u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX)
                    || snapshot.series.as_ref() != Some(&series)
                {
                    return Err("Rithmic engine snapshot identity mismatched".to_string());
                }
                bootstrap_from_snapshot(request, &snapshot).map(Some)
            }
            envelope::Payload::SeriesState(state) => {
                if state.consumer_id != self.consumer_id
                    || state.generation
                        != u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX)
                {
                    return Ok(None);
                }
                match SeriesLoadState::try_from(state.state)
                    .map_err(|_| "Rithmic engine returned invalid history state".to_string())?
                {
                    SeriesLoadState::Failed => Err(state
                        .detail
                        .unwrap_or_else(|| "Rithmic engine history is unavailable".to_string())),
                    SeriesLoadState::Superseded => {
                        Err("Rithmic engine history demand was superseded".to_string())
                    }
                    SeriesLoadState::Empty
                    | SeriesLoadState::Resolving
                    | SeriesLoadState::Partial
                    | SeriesLoadState::Ready
                    | SeriesLoadState::Live => Ok(None),
                }
            }
            envelope::Payload::DemandError(error) => Err(demand_error(&error)),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            envelope::Payload::ProviderState(_) | envelope::Payload::MarketEventIdle(_) => Ok(None),
            _ => Err("Rithmic engine returned an unexpected history event".to_string()),
        }
    }

    fn reset_consumer(&mut self) -> Result<(), String> {
        self.client
            .remove_market_consumer(self.consumer_id)
            .and_then(|()| {
                self.client
                    .register_consumer(self.client_id, ENGINE_WORKSPACE_ID, self.consumer_id)
            })
    }
}

impl Drop for EngineHistorySession {
    fn drop(&mut self) {
        let _ = self.client.remove_market_consumer(self.consumer_id);
        let _ = self.client.detach_client(self.client_id);
    }
}

fn run_history_worker(commands: &Receiver<HistoryCommand>, results: &LatestHistoryResult) {
    let mut session: Option<EngineHistorySession> = None;
    let mut active_request: Option<HistoryFetchRequest> = None;
    loop {
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(HistoryCommand::Fetch(request)) => {
                let result = if let Some(active) = session.as_mut() {
                    active.fetch(&request)
                } else {
                    EngineHistorySession::connect().and_then(|mut active| {
                        let result = active.fetch(&request);
                        session = Some(active);
                        result
                    })
                };
                let succeeded = result.is_ok();
                publish_history_result(results, &request, result);
                if succeeded {
                    active_request = Some(request);
                } else {
                    active_request = None;
                    session = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(request) = active_request.as_ref() else {
                    continue;
                };
                if request.stop.load(Ordering::Acquire) {
                    if let Some(active) = session.as_mut() {
                        let _ = active.reset_consumer();
                    }
                    active_request = None;
                    continue;
                }
                let Some(active) = session.as_mut() else {
                    active_request = None;
                    continue;
                };
                match active.poll_update(request) {
                    Ok(Some(bootstrap)) => {
                        publish_history_result(results, request, Ok(bootstrap));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        publish_history_result(results, request, Err(error));
                        active_request = None;
                        session = None;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn publish_history_result(
    results: &LatestHistoryResult,
    request: &HistoryFetchRequest,
    result: Result<MarketWorkerBootstrap, String>,
) {
    results.publish(RithmicHistoryResult {
        selection_generation: request.selection_generation,
        series_generation: request.series_generation,
        result: result.map(Box::new),
    });
}

fn engine_instrument(
    installed: &InstalledRithmicInstrument,
) -> Result<InstallProviderInstrument, String> {
    installed
        .descriptor
        .validate()
        .map_err(|error| error.to_string())?;
    Ok(InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation: installed.session_generation,
        selection_generation: u64::try_from(installed.selection_generation.get())
            .unwrap_or(u64::MAX),
        instrument_id: installed.descriptor.instrument_id.clone(),
        provider_symbol: installed.descriptor.provider_symbol.clone(),
        display_symbol: installed.descriptor.display_symbol.clone(),
        venue_id: installed.descriptor.venue_id.clone(),
        price_scale: u32::from(installed.descriptor.price_scale),
        quantity_scale: u32::from(installed.descriptor.quantity_scale),
        entitlement_id: installed.entitlement_id.clone(),
    })
}

fn engine_series_key(request: &HistoryFetchRequest) -> Result<SeriesKey, String> {
    let (cadence, cadence_value) = match request.series.interval() {
        ChartInterval::Tick100 => (SeriesCadence::Trades, 100),
        ChartInterval::Day1 => (SeriesCadence::SessionDays, 1),
        ChartInterval::Day3 => (SeriesCadence::SessionDays, 3),
        ChartInterval::Week1 => (SeriesCadence::CalendarWeeks, 1),
        ChartInterval::Month1 => (SeriesCadence::CalendarMonths, 1),
        interval => match interval.aggregation() {
            ChartAggregation::FixedSeconds(seconds) => (SeriesCadence::FixedSeconds, seconds.get()),
            ChartAggregation::Trades(_) | ChartAggregation::CalendarMonth => {
                return Err("Rithmic series cadence is invalid".to_string());
            }
        },
    };
    Ok(SeriesKey {
        provider: "rithmic".to_string(),
        instrument_id: request.instrument.descriptor.instrument_id.clone(),
        cadence_value,
        definition_revision: 1,
        entitlement_id: request.instrument.entitlement_id.clone(),
        cadence: cadence as i32,
    })
}

fn bootstrap_from_snapshot(
    request: &HistoryFetchRequest,
    snapshot: &SeriesSnapshot,
) -> Result<MarketWorkerBootstrap, String> {
    if snapshot.provider_generation < request.instrument.session_generation
        || snapshot.bars.is_empty()
        || snapshot.bars.len() > MAXIMUM_VISIBLE_BARS
        || snapshot.price_scale != u32::from(request.instrument.descriptor.price_scale)
        || snapshot.quantity_scale != u32::from(request.instrument.descriptor.quantity_scale)
    {
        return Err("Rithmic engine snapshot is invalid".to_string());
    }
    let descriptor = &request.instrument.descriptor;
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(descriptor.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: 1,
        asset_class: AssetClass::Future,
        symbol: descriptor.display_symbol.clone(),
        venue_id: descriptor.venue_id.clone(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(descriptor.price_scale, descriptor.quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    let interval_seconds = match request.series.interval().aggregation() {
        ChartAggregation::FixedSeconds(seconds) => seconds.get(),
        ChartAggregation::Trades(_) | ChartAggregation::CalendarMonth => 0,
    };
    let definition = BarDefinition {
        definition_id: format!("rithmic:{}:unadjusted:v1", request.series.label()),
        version: 1,
        interval_seconds,
        trades_per_bar: (request.series == RithmicSeries::Tick).then_some(100),
    };
    let received = unix_nanos_now()?;
    let bars = provenanced_engine_bars(request, snapshot, received);
    let replay = ReplaySnapshot::try_from_provenanced_values(
        instrument,
        ReplayProvenance::LiveProvider,
        definition,
        snapshot.provider_generation,
        bars,
    )
    .map_err(|error| error.to_string())?;
    let mut model = MarketBarClientModel::new(
        NonZeroUsize::new(MAXIMUM_VISIBLE_BARS).unwrap_or(NonZeroUsize::MIN),
    );
    let MarketBarModelOutcome::Published(generation) = model
        .apply_update(ReplayStreamUpdate::Snapshot(replay.clone()))
        .map_err(|error| error.to_string())?
    else {
        return Err("Rithmic engine snapshot was not publishable".to_string());
    };
    Ok(MarketWorkerBootstrap {
        snapshot: replay,
        subscription_id: format!(
            "{}  ·  {}",
            descriptor.provider_symbol,
            request.series.label()
        ),
        generation,
        worker_label: format!("Rithmic Test · {} engine history", request.series.label()),
    })
}

fn provenanced_engine_bars(
    request: &HistoryFetchRequest,
    snapshot: &SeriesSnapshot,
    received: i64,
) -> Vec<ProvenancedMarketBar> {
    snapshot
        .bars
        .iter()
        .map(|bar| {
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
                        "engine-rithmic-{}-{}-{}",
                        snapshot.provider_generation, snapshot.generation, bar.source_sequence
                    ),
                    event_time_unix_nanos: exchange,
                    publication_time_unix_nanos: received,
                    producer: "axiusflow_engine".to_string(),
                    schema_version: 1,
                    correlation_id: format!(
                        "rithmic-selection-{}-series-{}",
                        request.selection_generation, request.series_generation
                    ),
                    causation_id: "resident_engine_history".to_string(),
                    entitlement_revision: request.instrument.entitlement_id.clone(),
                    session_generation: snapshot.provider_generation,
                    source_id: "rithmic".to_string(),
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
        .collect()
}

fn demand_error(error: &DemandError) -> String {
    format!("{} failed: {}", error.stage, error.detail)
}

fn unix_nanos_now() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .ok_or_else(|| "system clock is invalid".to_string())
}

fn random_identity() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    Ok(NonZeroU64::new(u64::from_le_bytes(bytes))
        .unwrap_or(NonZeroU64::MIN)
        .get())
}

pub(crate) fn history_message(result: RithmicHistoryResult) -> MarketWorkerMessage {
    MarketWorkerMessage::RithmicHistory {
        selection_generation: result.selection_generation,
        series_generation: result.series_generation,
        result: result.result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_local_engine_protocol::MarketBar as IpcMarketBar;

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test generation is non-zero")
    }

    fn installed() -> InstalledRithmicInstrument {
        InstalledRithmicInstrument {
            session_generation: 7,
            selection_generation: nonzero(2),
            descriptor: InstrumentDescriptor {
                instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
                provider_symbol: "MNQU6".to_string(),
                display_symbol: "MNQU6".to_string(),
                venue_id: "CME".to_string(),
                price_scale: 2,
                quantity_scale: 0,
            },
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        }
    }

    fn request(series: RithmicSeries) -> HistoryFetchRequest {
        HistoryFetchRequest {
            selection_generation: nonzero(2),
            series_generation: nonzero(3),
            series,
            instrument: installed(),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn engine_series_keys_cover_every_rithmic_interval() {
        for series in RithmicSeries::ALL {
            let key = engine_series_key(&request(series)).expect("series key validates");
            assert_eq!(key.provider, "rithmic");
            assert_eq!(key.definition_revision, 1);
            assert_ne!(key.cadence, SeriesCadence::Unspecified as i32);
        }
    }

    #[test]
    fn engine_snapshot_preserves_exact_time_and_provider_generation() {
        let request = request(RithmicSeries::Tick);
        let series = engine_series_key(&request).expect("series key");
        let snapshot = SeriesSnapshot {
            consumer_id: 5,
            generation: 3,
            series: Some(series),
            provider_generation: 8,
            price_scale: 2,
            quantity_scale: 0,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 1_700_000_000,
                exchange_timestamp_unix_nanos: 1_700_000_000_123_456_789,
                open: 10_000,
                high: 10_100,
                low: 9_900,
                close: 10_050,
                volume: 8,
            }],
            publication_generation: 4,
            forming: false,
        };
        let bootstrap = bootstrap_from_snapshot(&request, &snapshot).expect("bootstrap validates");
        assert_eq!(
            bootstrap.snapshot.bars()[0].provenance().session_generation,
            8
        );
        assert_eq!(
            bootstrap.snapshot.bars()[0]
                .provenance()
                .exchange_timestamp_unix_nanos,
            1_700_000_000_123_456_789
        );
        assert_eq!(
            bootstrap.snapshot.bars()[0].provenance().producer,
            "axiusflow_engine"
        );
    }

    #[test]
    fn latest_result_conflates_obsolete_generations() {
        let results = LatestHistoryResult::default();
        for generation in [1, 2] {
            results.publish(RithmicHistoryResult {
                selection_generation: nonzero(1),
                series_generation: nonzero(generation),
                result: Err(format!("generation {generation}")),
            });
        }
        let result = results.take().expect("latest result remains");
        assert_eq!(result.series_generation, nonzero(2));
        assert!(results.take().is_none());
    }
}
