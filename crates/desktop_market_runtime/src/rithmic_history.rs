use crate as axiusflow_desktop_market_runtime;
pub(crate) use crate::rithmic_series::{RithmicSeries, RithmicSeriesRequest};
use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, Provenanced,
    ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
    validate_provenanced_market_bar,
};
use axiusflow_desktop_market_runtime::market_worker::{MarketWorkerBootstrap, MarketWorkerMessage};
use axiusflow_desktop_provider_runtime::InstrumentDescriptor;
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{
    BarDefinition, ChartInterval, MarketBar, RithmicChartAggregation, RithmicDailyAggregation,
};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_provider_history::HistoryRange;
use axiusflow_rithmic_protocol_adapter::{
    HistoryBars, HistoryCollectionRequest, HistorySeries, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicApplication, RithmicCredentialBytes,
    RithmicHistorySessionTransport, RithmicHistoryTransport, RithmicProviderInstrument,
    RithmicSessionLimits, RithmicTestSession, RithmicTimeBarResolution, TimeBarType,
    canonical_rithmic_tick_bar, canonical_rithmic_time_bar,
};
use std::{
    collections::BTreeMap,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroize;

pub(crate) const MAXIMUM_VISIBLE_BARS: usize = 300;
const HISTORY_COMMAND_CAPACITY: usize = 1;
const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);
const MAXIMUM_CONTROL_MESSAGES: usize = 64;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const TICK_TRADES_PER_BAR: u16 = 100;
const MAXIMUM_REPLAY_BARS: usize = 10_000;
const MAXIMUM_NON_TRADING_GAP_SECONDS: u64 = 4 * 24 * 60 * 60;
const DAILY_SESSION_PADDING_BARS: usize = 150;

#[derive(Clone)]
pub(crate) struct InstalledRithmicInstrument {
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
            .name("axiusflow-rithmic-history-worker".to_string())
            .spawn(move || run_history_worker(&command_rx, &worker_results))
            .map_err(|_| "Rithmic history worker is unavailable".to_string())?;
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
        request.series.resolution()?;
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
            .ok_or_else(|| "Rithmic history worker stopped".to_string())?
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => "Rithmic history worker is busy".to_string(),
                TrySendError::Disconnected(_) => "Rithmic history worker stopped".to_string(),
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
        if self
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

fn run_history_worker(commands: &Receiver<HistoryCommand>, results: &LatestHistoryResult) {
    while let Ok(command) = commands.recv() {
        let HistoryCommand::Fetch(request) = command;
        let result = fetch_history(&request).map(Box::new);
        results.publish(RithmicHistoryResult {
            selection_generation: request.selection_generation,
            series_generation: request.series_generation,
            result,
        });
    }
}

fn fetch_history(request: &HistoryFetchRequest) -> Result<MarketWorkerBootstrap, String> {
    let replay = replay_envelope(request.series, SystemTime::now())?;
    let connection = connect_history(Arc::clone(&request.stop))?;
    let mut transport = RithmicHistorySessionTransport::try_new(
        connection,
        REPLAY_TIMEOUT,
        NonZeroUsize::new(MAXIMUM_CONTROL_MESSAGES).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(|_| "Rithmic history transport is unavailable".to_string())?;
    let provider_instrument = RithmicProviderInstrument {
        descriptor: request.instrument.descriptor.clone(),
        entitlement_id: request.instrument.entitlement_id.clone(),
        trades: true,
        quotes: true,
        order_book: false,
    };
    let start_seconds = i32::try_from(replay.range.start_unix_nanos / NANOS_PER_SECOND)
        .map_err(|_| "Rithmic history range is invalid".to_string())?;
    let finish_seconds = i32::try_from(replay.range.end_unix_nanos / NANOS_PER_SECOND)
        .map_err(|_| "Rithmic history range is invalid".to_string())?;
    let bars = match request.series.interval().rithmic_aggregation() {
        Some(RithmicChartAggregation::Trades { trades_per_bar }) => {
            debug_assert_eq!(trades_per_bar.get(), TICK_TRADES_PER_BAR);
            collect_tick_history(
                &mut transport,
                &provider_instrument,
                start_seconds,
                finish_seconds,
                replay.maximum_bars,
            )?
        }
        Some(RithmicChartAggregation::Time { .. }) => {
            let resolution = request.series.resolution()?;
            collect_time_history(
                &mut transport,
                &provider_instrument,
                &resolution,
                start_seconds,
                finish_seconds,
                replay.maximum_bars,
            )?
        }
        Some(RithmicChartAggregation::DailySessions { period }) => {
            let resolution = daily_resolution()?;
            let daily = collect_time_history(
                &mut transport,
                &provider_instrument,
                &resolution,
                start_seconds,
                finish_seconds,
                replay.maximum_bars,
            )?;
            aggregate_daily_history(daily, period)?
        }
        None => return Err("Rithmic history series is unavailable".to_string()),
    };
    let response_bars = bars.len();
    let visible_bars = latest_visible_bars(bars);
    let first_marker = visible_bars
        .first()
        .map(|bar| bar.exchange_timestamp_unix_nanos);
    let last_marker = visible_bars
        .last()
        .map(|bar| bar.exchange_timestamp_unix_nanos);
    eprintln!(
        "Rithmic history response: series={} response_bars={} visible_bars={} first_marker={first_marker:?} last_marker={last_marker:?}",
        request.series.label(),
        response_bars,
        visible_bars.len(),
    );
    bootstrap_from_bars(request, visible_bars, unix_nanos_now()?)
}

#[derive(Clone, Copy)]
struct CanonicalHistoryBar {
    value: MarketBar,
    exchange_timestamp_unix_nanos: i64,
}

fn daily_resolution() -> Result<RithmicTimeBarResolution, String> {
    RithmicTimeBarResolution::try_new(
        ChartInterval::Day1.label(),
        TimeBarType::Daily,
        NonZeroU16::MIN,
    )
    .map_err(|_| "Rithmic daily history is unavailable".to_string())
}

fn aggregate_daily_history(
    daily: Vec<CanonicalHistoryBar>,
    period: RithmicDailyAggregation,
) -> Result<Vec<CanonicalHistoryBar>, String> {
    let daily = daily
        .into_iter()
        .map(|bar| (bar.exchange_timestamp_unix_nanos, bar))
        .collect::<BTreeMap<_, _>>();
    let mut aggregated = Vec::<CanonicalHistoryBar>::new();
    let mut active = None::<(i64, CanonicalHistoryBar)>;

    for (_, daily_bar) in daily {
        let bucket = daily_bucket(daily_bar.value.exchange_timestamp_seconds, period);
        match &mut active {
            Some((active_bucket, aggregate)) if *active_bucket == bucket => {
                aggregate.value.high = aggregate.value.high.max(daily_bar.value.high);
                aggregate.value.low = aggregate.value.low.min(daily_bar.value.low);
                aggregate.value.close = daily_bar.value.close;
                aggregate.value.volume = aggregate
                    .value
                    .volume
                    .checked_add(daily_bar.value.volume)
                    .ok_or_else(|| "Rithmic aggregate volume overflowed".to_string())?;
            }
            _ => {
                if let Some((_, completed)) = active.take() {
                    aggregated.push(completed);
                }
                active = Some((bucket, daily_bar));
            }
        }
    }
    if let Some((_, completed)) = active {
        aggregated.push(completed);
    }

    for (index, bar) in aggregated.iter_mut().enumerate() {
        bar.value.source_sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        bar.value.validate().map_err(|error| error.to_string())?;
    }
    Ok(aggregated)
}

fn daily_bucket(timestamp_seconds: i64, period: RithmicDailyAggregation) -> i64 {
    let unix_day = timestamp_seconds.div_euclid(86_400);
    match period {
        RithmicDailyAggregation::Week => unix_day - (unix_day + 3).rem_euclid(7),
        RithmicDailyAggregation::Month => {
            let (year, month) = civil_year_month(unix_day);
            year * 12 + i64::from(month)
        }
    }
}

fn civil_year_month(unix_day: i64) -> (i64, u32) {
    let day = unix_day + 719_468;
    let era = day.div_euclid(146_097);
    let day_of_era = day - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, u32::try_from(month).unwrap_or(1))
}

fn collect_time_history(
    transport: &mut RithmicHistorySessionTransport,
    instrument: &RithmicProviderInstrument,
    resolution: &RithmicTimeBarResolution,
    start_seconds: i32,
    finish_seconds: i32,
    maximum_bars: NonZeroUsize,
) -> Result<Vec<CanonicalHistoryBar>, String> {
    let interval_seconds = i32::try_from(
        resolution
            .interval_seconds()
            .map_err(|_| "Rithmic history series is invalid".to_string())?,
    )
    .map_err(|_| "Rithmic history series is invalid".to_string())?;
    let collection_start = start_seconds
        .checked_sub(interval_seconds)
        .ok_or_else(|| "Rithmic history range is invalid".to_string())?;
    let collection_finish = finish_seconds
        .checked_add(interval_seconds)
        .ok_or_else(|| "Rithmic history range is invalid".to_string())?;
    let collected = transport.collect_history(HistoryCollectionRequest {
        symbol: instrument.descriptor.provider_symbol.clone(),
        exchange: instrument.descriptor.venue_id.clone(),
        series: HistorySeries::Time {
            bar_type: match resolution.bar_type {
                TimeBarType::Second => {
                    axiusflow_rithmic_protocol_adapter::DecodedTimeBarType::Second
                }
                TimeBarType::Minute => {
                    axiusflow_rithmic_protocol_adapter::DecodedTimeBarType::Minute
                }
                TimeBarType::Daily => axiusflow_rithmic_protocol_adapter::DecodedTimeBarType::Daily,
                TimeBarType::Weekly => {
                    return Err("Rithmic history series is invalid".to_string());
                }
            },
            period: i32::from(resolution.period.get()),
        },
        start_seconds: collection_start,
        finish_seconds: collection_finish,
        maximum_bars,
    })?;
    let HistoryBars::Time(decoded) = collected.bars else {
        return Err("Rithmic history returned the wrong series".to_string());
    };
    let bars = decoded
        .iter()
        .filter(|bar| bar.marker_seconds >= start_seconds && bar.marker_seconds <= finish_seconds)
        .enumerate()
        .map(|(index, bar)| {
            canonical_rithmic_time_bar(instrument, resolution, bar)
                .map(|sequenced| {
                    let mut bar = sequenced.value;
                    bar.source_sequence =
                        u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
                    CanonicalHistoryBar {
                        value: bar,
                        exchange_timestamp_unix_nanos: bar.exchange_timestamp_seconds
                            * NANOS_PER_SECOND,
                    }
                })
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(bars)
}

fn collect_tick_history(
    transport: &mut RithmicHistorySessionTransport,
    instrument: &RithmicProviderInstrument,
    start_seconds: i32,
    finish_seconds: i32,
    maximum_bars: NonZeroUsize,
) -> Result<Vec<CanonicalHistoryBar>, String> {
    let collected = transport.collect_history(HistoryCollectionRequest {
        symbol: instrument.descriptor.provider_symbol.clone(),
        exchange: instrument.descriptor.venue_id.clone(),
        series: HistorySeries::Tick {
            trades_per_bar: TICK_TRADES_PER_BAR,
        },
        start_seconds,
        finish_seconds,
        maximum_bars,
    })?;
    let HistoryBars::Tick(decoded) = collected.bars else {
        return Err("Rithmic history returned the wrong series".to_string());
    };
    decoded
        .iter()
        .enumerate()
        .map(|(index, bar)| {
            let sequence =
                NonZeroU64::new(u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1))
                    .unwrap_or(NonZeroU64::MIN);
            canonical_rithmic_tick_bar(instrument, TICK_TRADES_PER_BAR, sequence, bar)
                .map(|bar| CanonicalHistoryBar {
                    value: bar.value,
                    exchange_timestamp_unix_nanos: bar.exchange_timestamp_unix_nanos,
                })
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn connect_history(
    stop: Arc<AtomicBool>,
) -> Result<axiusflow_rithmic_protocol_adapter::RithmicHistoryConnection, String> {
    let vault = NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE)
        .map_err(|_| "native credential vault unavailable".to_string())?;
    let mut stored = vault
        .load(RITHMIC_TEST_VAULT_KEY)
        .map_err(|_| "Rithmic Test credentials are unavailable".to_string())?
        .ok_or_else(|| "Rithmic Test credentials are unavailable".to_string())?;
    let copied = RithmicCredentialBytes::try_copy_from_vault(&stored)
        .map_err(|_| "Rithmic Test credentials are invalid".to_string());
    stored.zeroize();
    let copied = copied?;
    let credentials = copied
        .credentials()
        .map_err(|_| "Rithmic Test credentials are invalid".to_string())?;
    RithmicTestSession::discover_and_login_history(
        credentials,
        RithmicApplication {
            name: "Axiusflow",
            version: env!("CARGO_PKG_VERSION"),
        },
        RithmicSessionLimits::default(),
        Some(stop),
    )
    .map_err(|_| "Rithmic Test history login failed".to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReplayEnvelope {
    range: HistoryRange,
    maximum_bars: NonZeroUsize,
}

fn replay_envelope(series: RithmicSeries, now: SystemTime) -> Result<ReplayEnvelope, String> {
    let now_seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is invalid".to_string())?
        .as_secs();
    if let Some(RithmicChartAggregation::DailySessions { period }) =
        series.interval().rithmic_aggregation()
    {
        return aggregate_replay_envelope(now_seconds, period);
    }
    let interval = series.interval_seconds().unwrap_or(60);
    let end_seconds = if series == RithmicSeries::Tick {
        now_seconds
    } else {
        now_seconds - (now_seconds % interval)
    };
    let requested_bars = if series == RithmicSeries::Daily {
        MAXIMUM_VISIBLE_BARS.saturating_add(DAILY_SESSION_PADDING_BARS)
    } else {
        MAXIMUM_VISIBLE_BARS
    };
    let span_seconds = interval
        .checked_mul(requested_bars as u64)
        .and_then(|span| span.checked_add(MAXIMUM_NON_TRADING_GAP_SECONDS))
        .ok_or_else(|| "Rithmic visible range overflowed".to_string())?;
    let start_seconds = end_seconds
        .checked_sub(span_seconds)
        .ok_or_else(|| "Rithmic visible range underflowed".to_string())?;
    let theoretical_bars = if series == RithmicSeries::Tick {
        MAXIMUM_REPLAY_BARS
    } else {
        usize::try_from(span_seconds.div_ceil(interval))
            .ok()
            .and_then(|bars| bars.checked_add(3))
            .ok_or_else(|| "Rithmic visible range overflowed".to_string())?
    };
    if theoretical_bars > MAXIMUM_REPLAY_BARS {
        return Err("Rithmic visible range exceeds replay capacity".to_string());
    }
    Ok(ReplayEnvelope {
        range: HistoryRange {
            start_unix_nanos: i64::try_from(start_seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
                .ok_or_else(|| "Rithmic visible range overflowed".to_string())?,
            end_unix_nanos: i64::try_from(end_seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
                .ok_or_else(|| "Rithmic visible range overflowed".to_string())?,
        },
        maximum_bars: NonZeroUsize::new(theoretical_bars).unwrap_or(NonZeroUsize::MIN),
    })
}

fn aggregate_replay_envelope(
    now_seconds: u64,
    period: RithmicDailyAggregation,
) -> Result<ReplayEnvelope, String> {
    const DAY_SECONDS: u64 = 24 * 60 * 60;
    let maximum_days_per_bar = match period {
        RithmicDailyAggregation::Week => 7,
        RithmicDailyAggregation::Month => 31,
    };
    let source_days = MAXIMUM_VISIBLE_BARS
        .saturating_mul(maximum_days_per_bar)
        .saturating_add(maximum_days_per_bar)
        .saturating_add(
            usize::try_from(MAXIMUM_NON_TRADING_GAP_SECONDS / DAY_SECONDS).unwrap_or(usize::MAX),
        );
    let maximum_bars = source_days.saturating_add(3);
    if maximum_bars > MAXIMUM_REPLAY_BARS {
        return Err("Rithmic aggregate source exceeds replay capacity".to_string());
    }
    let end_seconds = now_seconds - now_seconds % DAY_SECONDS;
    let span_seconds = u64::try_from(source_days)
        .ok()
        .and_then(|days| days.checked_mul(DAY_SECONDS))
        .ok_or_else(|| "Rithmic aggregate range overflowed".to_string())?;
    let start_seconds = end_seconds
        .checked_sub(span_seconds)
        .ok_or_else(|| "Rithmic aggregate range underflowed".to_string())?;
    Ok(ReplayEnvelope {
        range: HistoryRange {
            start_unix_nanos: i64::try_from(start_seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
                .ok_or_else(|| "Rithmic aggregate range overflowed".to_string())?,
            end_unix_nanos: i64::try_from(end_seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
                .ok_or_else(|| "Rithmic aggregate range overflowed".to_string())?,
        },
        maximum_bars: NonZeroUsize::new(maximum_bars).unwrap_or(NonZeroUsize::MIN),
    })
}

fn latest_visible_bars(mut bars: Vec<CanonicalHistoryBar>) -> Vec<CanonicalHistoryBar> {
    bars.sort_unstable_by_key(|bar| bar.exchange_timestamp_unix_nanos);
    if bars.len() > MAXIMUM_VISIBLE_BARS {
        bars.drain(..bars.len() - MAXIMUM_VISIBLE_BARS);
    }
    for (index, bar) in bars.iter_mut().enumerate() {
        bar.value.source_sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
    }
    bars
}

fn bootstrap_from_bars(
    request: &HistoryFetchRequest,
    bars: Vec<CanonicalHistoryBar>,
    received_unix_nanos: i64,
) -> Result<MarketWorkerBootstrap, String> {
    if bars.is_empty() || bars.len() > MAXIMUM_VISIBLE_BARS {
        return Err("Rithmic visible history is empty or oversized".to_string());
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
    let interval_seconds = request
        .series
        .interval_seconds()
        .map_or(Ok(0), u32::try_from)
        .map_err(|_| "Rithmic interval is invalid".to_string())?;
    let definition = BarDefinition {
        definition_id: format!("rithmic:{}:unadjusted:v1", request.series.label()),
        version: 1,
        interval_seconds,
        trades_per_bar: (request.series == RithmicSeries::Tick)
            .then_some(u32::from(TICK_TRADES_PER_BAR)),
    };
    let session_generation = u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX);
    let provenanced = bars
        .into_iter()
        .map(|bar| {
            let provenance = history_provenance(
                request,
                &bar.value,
                bar.exchange_timestamp_unix_nanos,
                received_unix_nanos,
            )?;
            let item = Provenanced::new(bar.value, provenance);
            validate_provenanced_market_bar(&item).map_err(|error| error.to_string())?;
            Ok(item)
        })
        .collect::<Result<Vec<ProvenancedMarketBar>, String>>()?;
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        instrument,
        ReplayProvenance::LiveProvider,
        definition,
        session_generation,
        provenanced,
    )
    .map_err(|error| error.to_string())?;
    let mut model = MarketBarClientModel::new(
        NonZeroUsize::new(MAXIMUM_VISIBLE_BARS).unwrap_or(NonZeroUsize::MIN),
    );
    let MarketBarModelOutcome::Published(generation) = model
        .apply_update(ReplayStreamUpdate::Snapshot(snapshot.clone()))
        .map_err(|error| error.to_string())?
    else {
        return Err("Rithmic history snapshot was not published".to_string());
    };
    Ok(MarketWorkerBootstrap {
        snapshot,
        subscription_id: format!(
            "{}  ·  {}",
            descriptor.provider_symbol,
            request.series.label()
        ),
        generation,
        worker_label: format!("Rithmic Test · {} visible history", request.series.label()),
    })
}

fn history_provenance(
    request: &HistoryFetchRequest,
    bar: &MarketBar,
    exchange: i64,
    received_unix_nanos: i64,
) -> Result<MarketEventProvenance, String> {
    if exchange.div_euclid(NANOS_PER_SECOND) != bar.exchange_timestamp_seconds {
        return Err("Rithmic history timestamp is inconsistent".to_string());
    }
    Ok(MarketEventProvenance {
        event_id: format!("rithmic_history_{}_{}", request.series.label(), exchange),
        event_time_unix_nanos: exchange,
        publication_time_unix_nanos: received_unix_nanos,
        producer: "axiusflow_desktop_rithmic_history_worker".to_string(),
        schema_version: 1,
        correlation_id: format!(
            "rithmic_selection_{}_series_{}",
            request.selection_generation, request.series_generation
        ),
        causation_id: "rithmic_history_replay".to_string(),
        entitlement_revision: request.instrument.entitlement_id.clone(),
        session_generation: u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX),
        source_id: "rithmic".to_string(),
        source_sequence: bar.source_sequence,
        exchange_timestamp_unix_nanos: exchange,
        provider_receive_timestamp_unix_nanos: received_unix_nanos,
        nic_receive_timestamp_unix_nanos: None,
        axiusflow_receive_timestamp_unix_nanos: received_unix_nanos,
        normalized_timestamp_unix_nanos: received_unix_nanos,
        fanout_enqueue_timestamp_unix_nanos: None,
        correction_flags: 0,
        quality_flags: 0,
        nic_timestamp_source: 0,
        semantic_class: 2,
    })
}

fn unix_nanos_now() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .ok_or_else(|| "system clock is invalid".to_string())
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
    use crate::rithmic_live_chart::{RithmicChartGeneration, RithmicLiveChart};
    use axiusflow_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};

    #[test]
    fn rithmic_series_wraps_the_shared_chart_interval_catalog() {
        assert_eq!(
            RithmicSeries::ALL.map(RithmicSeries::interval),
            ChartInterval::ALL
        );
        assert!(RithmicSeries::Tick.supports_native_history());
        assert!(RithmicSeries::Hour12.supports_native_history());
        assert!(RithmicSeries::Week1.supports_native_history());
        assert!(RithmicSeries::Month1.supports_native_history());
        assert_eq!(
            RithmicSeries::Hour12
                .resolution()
                .expect("12h is a native Rithmic minute period")
                .period
                .get(),
            720
        );
    }

    fn daily_bar(timestamp: i64, ohlc: [i64; 4], volume: i64) -> CanonicalHistoryBar {
        CanonicalHistoryBar {
            value: MarketBar {
                source_sequence: 99,
                exchange_timestamp_seconds: timestamp,
                open: ohlc[0],
                high: ohlc[1],
                low: ohlc[2],
                close: ohlc[3],
                volume,
            },
            exchange_timestamp_unix_nanos: timestamp * NANOS_PER_SECOND,
        }
    }

    #[test]
    fn weekly_aggregation_uses_monday_boundaries_and_whole_daily_sessions() {
        let monday = 1_704_067_200;
        let bars = vec![
            daily_bar(monday, [100, 110, 90, 105], 10),
            daily_bar(monday + 86_400, [105, 115, 95, 112], 20),
            daily_bar(monday + 4 * 86_400, [112, 120, 108, 118], 30),
            daily_bar(monday + 7 * 86_400, [118, 125, 115, 122], 40),
        ];

        let weekly = aggregate_daily_history(bars, RithmicDailyAggregation::Week)
            .expect("weekly aggregation validates");

        assert_eq!(weekly.len(), 2);
        assert_eq!(weekly[0].value.source_sequence, 1);
        assert_eq!(weekly[0].value.exchange_timestamp_seconds, monday);
        assert_eq!(
            [
                weekly[0].value.open,
                weekly[0].value.high,
                weekly[0].value.low,
                weekly[0].value.close,
            ],
            [100, 120, 90, 118]
        );
        assert_eq!(weekly[0].value.volume, 60);
        assert_eq!(weekly[1].value.source_sequence, 2);
        assert_eq!(
            weekly[1].value.exchange_timestamp_seconds,
            monday + 7 * 86_400
        );
    }

    #[test]
    fn monthly_aggregation_honors_calendar_boundaries_and_leap_day() {
        let january_31 = 1_706_659_200;
        let february_1 = 1_706_745_600;
        let february_29 = 1_709_164_800;
        let march_1 = 1_709_251_200;
        let bars = vec![
            daily_bar(january_31, [90, 100, 80, 95], 5),
            daily_bar(february_1, [100, 110, 90, 105], 10),
            daily_bar(february_29, [105, 120, 85, 115], 20),
            daily_bar(march_1, [115, 130, 110, 125], 30),
        ];

        let monthly = aggregate_daily_history(bars, RithmicDailyAggregation::Month)
            .expect("monthly aggregation validates");

        assert_eq!(monthly.len(), 3);
        assert_eq!(
            monthly
                .iter()
                .map(|bar| bar.value.source_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(monthly[1].value.exchange_timestamp_seconds, february_1);
        assert_eq!(
            [
                monthly[1].value.open,
                monthly[1].value.high,
                monthly[1].value.low,
                monthly[1].value.close,
            ],
            [100, 120, 85, 115]
        );
        assert_eq!(monthly[1].value.volume, 30);
        assert_eq!(monthly[2].value.exchange_timestamp_seconds, march_1);
    }

    #[test]
    fn replay_envelopes_are_aligned_and_bounded() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_123_456);
        for series in [
            RithmicSeries::Minute1,
            RithmicSeries::Minute5,
            RithmicSeries::Minute15,
            RithmicSeries::Hour1,
            RithmicSeries::Daily,
        ] {
            let interval = i64::try_from(series.interval_seconds().expect("time series"))
                .expect("interval fits");
            let replay = replay_envelope(series, now).expect("range validates");
            let range = replay.range;
            assert_eq!(range.start_unix_nanos % (interval * NANOS_PER_SECOND), 0);
            assert_eq!(range.end_unix_nanos % (interval * NANOS_PER_SECOND), 0);
            assert!(replay.maximum_bars.get() <= MAXIMUM_REPLAY_BARS);
            assert!(
                range.end_unix_nanos - range.start_unix_nanos
                    >= interval
                        * NANOS_PER_SECOND
                        * i64::try_from(MAXIMUM_VISIBLE_BARS).expect("visible bound fits")
            );
        }
        let tick = replay_envelope(RithmicSeries::Tick, now).expect("tick envelope validates");
        assert_eq!(tick.range.end_unix_nanos, 1_800_123_456 * NANOS_PER_SECOND);
        assert_eq!(tick.maximum_bars.get(), MAXIMUM_REPLAY_BARS);

        for series in [RithmicSeries::Week1, RithmicSeries::Month1] {
            let replay = replay_envelope(series, now).expect("aggregate envelope validates");
            assert_eq!(
                replay.range.start_unix_nanos % (86_400 * NANOS_PER_SECOND),
                0
            );
            assert_eq!(replay.range.end_unix_nanos % (86_400 * NANOS_PER_SECOND), 0);
            assert!(replay.maximum_bars.get() <= MAXIMUM_REPLAY_BARS);
            assert!(replay.maximum_bars.get() > MAXIMUM_VISIBLE_BARS);
        }
    }

    #[test]
    fn saturday_intraday_envelope_reaches_the_prior_session() {
        let saturday_seconds = 1_786_190_400;
        let saturday_noon_utc = UNIX_EPOCH + Duration::from_secs(saturday_seconds);
        let replay = replay_envelope(RithmicSeries::Minute1, saturday_noon_utc)
            .expect("weekend replay envelope validates");
        let tuesday_noon_utc = 1_785_844_800_i64 * NANOS_PER_SECOND;
        let saturday_noon_utc = 1_786_190_400_i64 * NANOS_PER_SECOND;
        assert!(replay.range.start_unix_nanos <= tuesday_noon_utc);
        assert_eq!(replay.range.end_unix_nanos, saturday_noon_utc);
        assert_eq!(replay.maximum_bars.get(), 6_063);
    }

    #[test]
    fn latest_visible_bars_orders_and_caps_an_expanded_replay() {
        let bars = (1_i64..=400)
            .rev()
            .map(|timestamp| CanonicalHistoryBar {
                value: MarketBar {
                    source_sequence: u64::try_from(timestamp).expect("timestamp fits"),
                    exchange_timestamp_seconds: timestamp,
                    open: timestamp,
                    high: timestamp,
                    low: timestamp,
                    close: timestamp,
                    volume: 1,
                },
                exchange_timestamp_unix_nanos: timestamp * NANOS_PER_SECOND,
            })
            .collect();
        let latest = latest_visible_bars(bars);
        assert_eq!(latest.len(), MAXIMUM_VISIBLE_BARS);
        assert_eq!(
            latest
                .first()
                .expect("latest replay is nonempty")
                .value
                .exchange_timestamp_seconds,
            101
        );
        assert_eq!(
            latest
                .last()
                .expect("latest replay is nonempty")
                .value
                .exchange_timestamp_seconds,
            400
        );
        assert!(latest.iter().enumerate().all(|(index, bar)| {
            bar.value.source_sequence == u64::try_from(index).unwrap_or(u64::MAX) + 1
        }));
    }

    #[test]
    fn delayed_entitlement_is_identical_across_history_seed_and_live_trade() {
        const START: i64 = 1_800_000_000;
        let entitlement_id = "rithmic-test:CME-Delayed:MNQU6";
        let selection_generation = NonZeroUsize::MIN;
        let series_generation = NonZeroUsize::MIN;
        let request = HistoryFetchRequest {
            selection_generation,
            series_generation,
            series: RithmicSeries::Minute1,
            instrument: InstalledRithmicInstrument {
                selection_generation,
                descriptor: InstrumentDescriptor {
                    instrument_id: "rithmic:CME:MNQU6".to_string(),
                    provider_symbol: "MNQU6".to_string(),
                    display_symbol: "MNQU6".to_string(),
                    venue_id: "CME".to_string(),
                    price_scale: 2,
                    quantity_scale: 0,
                },
                entitlement_id: entitlement_id.to_string(),
            },
            stop: Arc::new(AtomicBool::new(false)),
        };
        let bootstrap = bootstrap_from_bars(
            &request,
            vec![CanonicalHistoryBar {
                value: MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: START,
                    open: 2_000_000,
                    high: 2_000_100,
                    low: 1_999_900,
                    close: 2_000_025,
                    volume: 10,
                },
                exchange_timestamp_unix_nanos: START * NANOS_PER_SECOND,
            }],
            (START + 1) * NANOS_PER_SECOND,
        )
        .expect("delayed-entitlement history seed validates");
        assert_eq!(
            bootstrap
                .snapshot
                .bars()
                .last()
                .expect("history is nonempty")
                .provenance()
                .entitlement_revision,
            entitlement_id
        );

        let generation = RithmicChartGeneration {
            selection: selection_generation,
            series: series_generation,
        };
        let mut chart = RithmicLiveChart::from_history(generation, &bootstrap.snapshot)
            .expect("history installs into live chart");
        let publication = chart
            .apply_trade(
                generation,
                &MarketTrade {
                    metadata: EventMetadata {
                        provider_id: "rithmic".to_string(),
                        instrument_id: "rithmic:CME:MNQU6".to_string(),
                        entitlement_id: entitlement_id.to_string(),
                        source_sequence: 2,
                        session_generation: 9,
                        timestamps: QualifiedTimestamp {
                            exchange_unix_nanos: Some((START + 1) * NANOS_PER_SECOND),
                            provider_unix_nanos: None,
                            received_unix_nanos: (START + 2) * NANOS_PER_SECOND,
                        },
                    },
                    trade_id: "delayed-live-2".to_string(),
                    price: 2_000_050,
                    quantity: 2,
                    aggressor: AggressorSide::Buy,
                },
            )
            .expect("matching delayed-entitlement trade extends history");
        assert_eq!(
            publication
                .snapshot
                .bars()
                .last()
                .expect("live snapshot is nonempty")
                .provenance()
                .entitlement_revision,
            entitlement_id
        );
    }

    #[test]
    fn stale_result_does_not_release_newer_cancellation_owner() {
        let (command_tx, _command_rx) = mpsc::sync_channel(HISTORY_COMMAND_CAPACITY);
        let results = Arc::new(LatestHistoryResult::default());
        let selection = NonZeroUsize::MIN;
        let stale = NonZeroUsize::MIN;
        let latest = NonZeroUsize::new(2).expect("generation is nonzero");
        let stop = Arc::new(AtomicBool::new(false));
        let mut task = RithmicHistoryTask {
            commands: Some(command_tx),
            results: Arc::clone(&results),
            active: Some((selection, latest, Arc::clone(&stop))),
            handle: None,
        };
        results.publish(RithmicHistoryResult {
            selection_generation: selection,
            series_generation: stale,
            result: Err("cancelled".to_string()),
        });
        assert!(task.try_recv().is_some());
        assert!(task.active.is_some());
        assert!(!stop.load(Ordering::Acquire));
        results.publish(RithmicHistoryResult {
            selection_generation: selection,
            series_generation: latest,
            result: Err("latest failed".to_string()),
        });
        assert!(task.try_recv().is_some());
        assert!(task.active.is_none());
    }

    #[test]
    fn result_mailbox_replaces_an_unconsumed_older_generation() {
        let results = LatestHistoryResult::default();
        let selection = NonZeroUsize::MIN;
        let older = NonZeroUsize::MIN;
        let latest = NonZeroUsize::new(2).expect("generation is nonzero");
        results.publish(RithmicHistoryResult {
            selection_generation: selection,
            series_generation: older,
            result: Err("older".to_string()),
        });
        results.publish(RithmicHistoryResult {
            selection_generation: selection,
            series_generation: latest,
            result: Err("latest".to_string()),
        });
        let retained = results.take().expect("latest result is retained");
        assert_eq!(retained.series_generation, latest);
        assert!(results.take().is_none());
    }

    #[test]
    fn drop_disconnects_a_full_command_queue_before_joining() {
        use std::sync::Barrier;

        let (command_tx, command_rx) = mpsc::sync_channel(HISTORY_COMMAND_CAPACITY);
        let release_worker = Arc::new(Barrier::new(2));
        let worker_release = Arc::clone(&release_worker);
        let handle = thread::spawn(move || {
            worker_release.wait();
            assert!(command_rx.recv().is_ok());
            assert!(command_rx.recv().is_err());
        });
        command_tx
            .send(HistoryCommand::Fetch(HistoryFetchRequest {
                selection_generation: NonZeroUsize::MIN,
                series_generation: NonZeroUsize::MIN,
                series: RithmicSeries::Minute1,
                instrument: InstalledRithmicInstrument {
                    selection_generation: NonZeroUsize::MIN,
                    descriptor: InstrumentDescriptor {
                        instrument_id: "MNQ.CME".to_string(),
                        provider_symbol: "MNQU6".to_string(),
                        display_symbol: "MNQU6".to_string(),
                        venue_id: "CME".to_string(),
                        price_scale: 2,
                        quantity_scale: 0,
                    },
                    entitlement_id: "test".to_string(),
                },
                stop: Arc::new(AtomicBool::new(false)),
            }))
            .expect("command queue accepts one request");
        let task = RithmicHistoryTask {
            commands: Some(command_tx),
            results: Arc::new(LatestHistoryResult::default()),
            active: None,
            handle: Some(handle),
        };
        let (dropped_tx, dropped_rx) = mpsc::channel();
        thread::spawn(move || {
            drop(task);
            let _ = dropped_tx.send(());
        });
        release_worker.wait();
        assert!(dropped_rx.recv_timeout(Duration::from_secs(1)).is_ok());
    }
}
