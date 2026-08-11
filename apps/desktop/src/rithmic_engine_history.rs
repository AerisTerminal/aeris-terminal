use crate::rithmic_history::{RithmicSeries, RithmicSeriesRequest};

use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, Provenanced,
    ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_local_engine_client::{
    EngineClient, connect_or_start_engine, sibling_engine_executable,
};
use axiusflow_local_engine_protocol::{
    DemandError, InstallProviderInstrument, OrderBookSnapshot as IpcOrderBookSnapshot,
    OrderBookState as IpcOrderBookState, SeriesCadence, SeriesKey, SeriesLoadState, SeriesSnapshot,
    envelope,
};
use axiusflow_market_data::{
    BarDefinition, ChartAggregation, ChartInterval, DepthLevel, MarketBar, OrderBookPublication,
    OrderBookRecoveryReason, OrderBookState,
};
use axiusflow_terminal_ui::{DomFrame, DomSelection, ReadOnlyDom};
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

use crate::resident_market_worker::{MarketWorkerBootstrap, MarketWorkerMessage};

pub(crate) const MAXIMUM_VISIBLE_BARS: usize = 300;
const MAXIMUM_DOM_LEVELS: usize = 20;
const HISTORY_COMMAND_CAPACITY: usize = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const HISTORY_TIMEOUT: Duration = Duration::from_secs(35);
const ENGINE_WORKSPACE_ID: u64 = 1;

struct HistoryFetchRequest {
    selection_generation: NonZeroUsize,
    series_generation: NonZeroUsize,
    series: RithmicSeries,
    instrument: InstallProviderInstrument,
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

#[derive(Default)]
struct LatestDomFrame {
    value: Mutex<Option<DomFrame>>,
}

impl LatestDomFrame {
    fn publish(&self, frame: DomFrame) {
        *self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frame);
    }

    fn take(&self) -> Option<DomFrame> {
        self.value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
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
    dom: Arc<LatestDomFrame>,
    active: Option<(NonZeroUsize, NonZeroUsize, Arc<AtomicBool>)>,
    handle: Option<JoinHandle<()>>,
}

impl RithmicHistoryTask {
    pub(crate) fn start() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::sync_channel(HISTORY_COMMAND_CAPACITY);
        let results = Arc::new(LatestHistoryResult::default());
        let dom = Arc::new(LatestDomFrame::default());
        let worker_results = Arc::clone(&results);
        let worker_dom = Arc::clone(&dom);
        let handle = thread::Builder::new()
            .name("axiusflow-rithmic-engine-history-client".to_string())
            .spawn(move || run_history_worker(&command_rx, &worker_results, &worker_dom))
            .map_err(|_| "Rithmic engine history client is unavailable".to_string())?;
        Ok(Self {
            commands: Some(command_tx),
            results,
            dom,
            active: None,
            handle: Some(handle),
        })
    }

    pub(crate) fn request(
        &mut self,
        request: RithmicSeriesRequest,
        instrument: InstallProviderInstrument,
    ) -> Result<(), String> {
        if u64::try_from(request.selection_generation.get()).unwrap_or(u64::MAX)
            != instrument.selection_generation
        {
            return Err("Rithmic history selection is stale".to_string());
        }
        if instrument.session_generation == 0
            || validate_engine_instrument(&instrument).is_err()
            || !request.series.supports_native_history()
        {
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

    pub(crate) fn try_recv_dom(&self) -> Option<DomFrame> {
        self.dom.take()
    }

    pub(crate) fn cancel(&mut self) {
        if let Some((_, _, stop)) = self.active.take() {
            stop.store(true, Ordering::Release);
        }
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

enum EngineUpdate {
    History(Box<MarketWorkerBootstrap>),
    Dom(DomFrame),
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

    fn fetch(
        &mut self,
        request: &HistoryFetchRequest,
        dom: &LatestDomFrame,
    ) -> Result<MarketWorkerBootstrap, String> {
        let series = engine_series_key(request)?;
        self.client
            .install_provider_instrument(request.instrument.clone())?;
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
            let Some(update) = self.poll_update(request)? else {
                thread::sleep(POLL_INTERVAL);
                continue;
            };
            match update {
                EngineUpdate::History(bootstrap) => return Ok(*bootstrap),
                EngineUpdate::Dom(frame) => dom.publish(frame),
            }
        }
    }

    fn poll_update(
        &mut self,
        request: &HistoryFetchRequest,
    ) -> Result<Option<EngineUpdate>, String> {
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
                bootstrap_from_snapshot(request, &snapshot)
                    .map(Box::new)
                    .map(EngineUpdate::History)
                    .map(Some)
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
            envelope::Payload::OrderBookSnapshot(snapshot) => {
                if snapshot.consumer_id != self.consumer_id {
                    return Err("Rithmic engine order-book consumer mismatched".to_string());
                }
                dom_from_snapshot(request, &snapshot)
                    .map(EngineUpdate::Dom)
                    .map(Some)
            }
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

fn run_history_worker(
    commands: &Receiver<HistoryCommand>,
    results: &LatestHistoryResult,
    dom: &LatestDomFrame,
) {
    let mut session: Option<EngineHistorySession> = None;
    let mut active_request: Option<HistoryFetchRequest> = None;
    loop {
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(HistoryCommand::Fetch(request)) => {
                let result = if let Some(active) = session.as_mut() {
                    active.fetch(&request, dom)
                } else {
                    EngineHistorySession::connect().and_then(|mut active| {
                        let result = active.fetch(&request, dom);
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
                    Ok(Some(EngineUpdate::History(bootstrap))) => {
                        publish_history_result(results, request, Ok(*bootstrap));
                    }
                    Ok(Some(EngineUpdate::Dom(frame))) => dom.publish(frame),
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

pub(crate) fn validate_engine_instrument(
    instrument: &InstallProviderInstrument,
) -> Result<(), String> {
    if instrument.provider != "rithmic"
        || instrument.instrument_id.trim().is_empty()
        || instrument.provider_symbol.trim().is_empty()
        || instrument.display_symbol.trim().is_empty()
        || instrument.venue_id.trim().is_empty()
        || instrument.entitlement_id.trim().is_empty()
        || instrument.price_scale > 18
        || instrument.quantity_scale > 18
    {
        return Err("Rithmic engine instrument is invalid".to_string());
    }
    Ok(())
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
        instrument_id: request.instrument.instrument_id.clone(),
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
        || snapshot.price_scale != request.instrument.price_scale
        || snapshot.quantity_scale != request.instrument.quantity_scale
    {
        return Err("Rithmic engine snapshot is invalid".to_string());
    }
    let price_scale = u8::try_from(request.instrument.price_scale)
        .map_err(|_| "Rithmic engine price scale is invalid".to_string())?;
    let quantity_scale = u8::try_from(request.instrument.quantity_scale)
        .map_err(|_| "Rithmic engine quantity scale is invalid".to_string())?;
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(request.instrument.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: 1,
        asset_class: AssetClass::Future,
        symbol: request.instrument.display_symbol.clone(),
        venue_id: request.instrument.venue_id.clone(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(price_scale, quantity_scale)
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
            request.instrument.provider_symbol,
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

fn dom_from_snapshot(
    request: &HistoryFetchRequest,
    snapshot: &IpcOrderBookSnapshot,
) -> Result<DomFrame, String> {
    let expected_generation = u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX);
    let expected_selection = u64::try_from(request.selection_generation.get()).unwrap_or(u64::MAX);
    if snapshot.consumer_id == 0
        || snapshot.generation != expected_generation
        || snapshot.provider != "rithmic"
        || snapshot.instrument_id != request.instrument.instrument_id
        || snapshot.entitlement_id != request.instrument.entitlement_id
        || snapshot.provider_generation < request.instrument.session_generation
        || snapshot.selection_generation != expected_selection
        || snapshot.bids.len() > MAXIMUM_DOM_LEVELS
        || snapshot.asks.len() > MAXIMUM_DOM_LEVELS
    {
        return Err("Rithmic engine order-book identity is invalid".to_string());
    }
    let state = match IpcOrderBookState::try_from(snapshot.state)
        .map_err(|_| "Rithmic engine order-book state is invalid".to_string())?
    {
        IpcOrderBookState::Unspecified => {
            return Err("Rithmic engine order-book state is unspecified".to_string());
        }
        IpcOrderBookState::AwaitingSnapshot => {
            OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot)
        }
        IpcOrderBookState::Ready => OrderBookState::Ready,
        IpcOrderBookState::Stale => OrderBookState::Stale,
        IpcOrderBookState::SequenceGap => {
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        }
        IpcOrderBookState::CrossedBook => {
            OrderBookState::Recovering(OrderBookRecoveryReason::CrossedBook)
        }
        IpcOrderBookState::InvalidUpdate => {
            OrderBookState::Recovering(OrderBookRecoveryReason::InvalidUpdate)
        }
    };
    let bids = ipc_depth_levels(&snapshot.bids, true)?;
    let asks = ipc_depth_levels(&snapshot.asks, false)?;
    if bids
        .first()
        .zip(asks.first())
        .is_some_and(|(bid, ask)| bid.price >= ask.price)
    {
        return Err("Rithmic engine order book is crossed".to_string());
    }
    let publication = OrderBookPublication {
        provider_id: snapshot.provider.clone(),
        instrument_id: snapshot.instrument_id.clone(),
        entitlement_id: snapshot.entitlement_id.clone(),
        session_generation: snapshot.provider_generation,
        revision: snapshot.revision,
        source_watermark: snapshot.source_watermark,
        bids,
        asks,
        state,
    };
    let selection = DomSelection {
        provider_id: snapshot.provider.clone(),
        instrument_id: snapshot.instrument_id.clone(),
        entitlement_id: snapshot.entitlement_id.clone(),
        session_generation: snapshot.provider_generation,
        selection_generation: snapshot.selection_generation,
        precision: InstrumentPrecision::try_new(
            u8::try_from(request.instrument.price_scale)
                .map_err(|_| "Rithmic engine price scale is invalid".to_string())?,
            u8::try_from(request.instrument.quantity_scale)
                .map_err(|_| "Rithmic engine quantity scale is invalid".to_string())?,
        )
        .map_err(|error| error.to_string())?,
    };
    ReadOnlyDom::project_publication(&selection, &publication)
        .ok_or_else(|| "Rithmic engine order-book publication is stale".to_string())
}

fn ipc_depth_levels(
    levels: &[axiusflow_local_engine_protocol::OrderBookLevel],
    bids: bool,
) -> Result<Vec<DepthLevel>, String> {
    let mut previous = None;
    let mut converted = Vec::with_capacity(levels.len());
    for level in levels {
        if level.price <= 0 || level.quantity <= 0 {
            return Err("Rithmic engine order-book level is invalid".to_string());
        }
        if previous.is_some_and(|previous| {
            if bids {
                level.price >= previous
            } else {
                level.price <= previous
            }
        }) {
            return Err("Rithmic engine order-book levels are unordered".to_string());
        }
        previous = Some(level.price);
        converted.push(DepthLevel {
            price: level.price,
            quantity: level.quantity,
            order_count: level.order_count,
        });
    }
    Ok(converted)
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
    use axiusflow_local_engine_protocol::{MarketBar as IpcMarketBar, OrderBookLevel};

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test generation is non-zero")
    }

    fn installed() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation: 7,
            selection_generation: 2,
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQU6".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
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
    fn engine_order_book_projects_without_desktop_reconstruction() {
        let request = request(RithmicSeries::Minute1);
        let frame = dom_from_snapshot(
            &request,
            &IpcOrderBookSnapshot {
                consumer_id: 5,
                generation: 3,
                provider: "rithmic".to_string(),
                instrument_id: request.instrument.instrument_id.clone(),
                entitlement_id: request.instrument.entitlement_id.clone(),
                provider_generation: 8,
                selection_generation: 2,
                revision: 4,
                source_watermark: 11,
                state: IpcOrderBookState::Ready as i32,
                bids: vec![OrderBookLevel {
                    price: 2_000_000,
                    quantity: 7,
                    order_count: Some(3),
                }],
                asks: vec![OrderBookLevel {
                    price: 2_000_025,
                    quantity: 4,
                    order_count: Some(2),
                }],
            },
        )
        .expect("engine book projects");
        assert_eq!(frame.session_generation, 8);
        assert_eq!(frame.selection_generation, 2);
        assert_eq!(frame.source_watermark, 11);
        assert_eq!(
            frame.rows[0]
                .bid
                .as_ref()
                .map(|level| level.price_text.as_str()),
            Some("20000.00")
        );
        assert_eq!(
            frame.rows[0]
                .ask
                .as_ref()
                .map(|level| level.relative_size_bps),
            Some(5_714)
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
