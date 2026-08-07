use crate::market_worker::{MarketWorkerBootstrap, MarketWorkerMessage};
use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, Provenanced,
    ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
    validate_provenanced_market_bar,
};
use axiusflow_desktop_provider_runtime::InstrumentDescriptor;
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_provider_history::HistoryRange;
use axiusflow_rithmic_protocol_adapter::{
    HistoryBars, HistoryCollectionRequest, HistorySeries, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicApplication, RithmicCredentialBytes,
    RithmicHistorySessionTransport, RithmicHistoryTransport, RithmicProviderInstrument,
    RithmicSessionLimits, RithmicTestSession, RithmicTimeBarResolution, TimeBarType,
    canonical_rithmic_time_bar,
};
use std::{
    num::{NonZeroU16, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroize;

pub(crate) const MAXIMUM_VISIBLE_BARS: usize = 300;
const HISTORY_COMMAND_CAPACITY: usize = 1;
const HISTORY_RESULT_CAPACITY: usize = 2;
const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);
const MAXIMUM_CONTROL_MESSAGES: usize = 64;
const NANOS_PER_SECOND: i64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RithmicSeries {
    Tick,
    Minute1,
    Minute5,
    Minute15,
    Hour1,
    Daily,
}

impl RithmicSeries {
    pub(crate) const ALL: [Self; 6] = [
        Self::Tick,
        Self::Minute1,
        Self::Minute5,
        Self::Minute15,
        Self::Hour1,
        Self::Daily,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Tick => "Tick",
            Self::Minute1 => "1m",
            Self::Minute5 => "5m",
            Self::Minute15 => "15m",
            Self::Hour1 => "1h",
            Self::Daily => "Daily",
        }
    }

    const fn interval_seconds(self) -> Option<u64> {
        match self {
            Self::Tick => None,
            Self::Minute1 => Some(60),
            Self::Minute5 => Some(300),
            Self::Minute15 => Some(900),
            Self::Hour1 => Some(3_600),
            Self::Daily => Some(86_400),
        }
    }

    fn resolution(self) -> Result<RithmicTimeBarResolution, String> {
        let (bar_type, period) = match self {
            Self::Tick => return Err("Historical tick continuity is unavailable".to_string()),
            Self::Minute1 => (TimeBarType::Minute, 1),
            Self::Minute5 => (TimeBarType::Minute, 5),
            Self::Minute15 => (TimeBarType::Minute, 15),
            Self::Hour1 => (TimeBarType::Minute, 60),
            Self::Daily => (TimeBarType::Daily, 1),
        };
        RithmicTimeBarResolution::try_new(
            self.label(),
            bar_type,
            NonZeroU16::new(period).unwrap_or(NonZeroU16::MIN),
        )
        .map_err(|_| "Rithmic series is unavailable".to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSeriesRequest {
    pub(crate) selection_generation: NonZeroUsize,
    pub(crate) series_generation: NonZeroUsize,
    pub(crate) series: RithmicSeries,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RithmicSeriesBrowser {
    next_generation: usize,
    pending: Option<RithmicSeriesRequest>,
    selected: Option<RithmicSeriesRequest>,
}

impl RithmicSeriesBrowser {
    pub(crate) fn select(
        &mut self,
        selection_generation: NonZeroUsize,
        series: RithmicSeries,
    ) -> RithmicSeriesRequest {
        self.next_generation = self.next_generation.saturating_add(1).max(1);
        let request = RithmicSeriesRequest {
            selection_generation,
            series_generation: NonZeroUsize::new(self.next_generation).unwrap_or(NonZeroUsize::MIN),
            series,
        };
        self.pending = Some(request);
        request
    }

    pub(crate) fn accept(
        &mut self,
        selection_generation: NonZeroUsize,
        series_generation: NonZeroUsize,
    ) -> bool {
        let Some(request) = self.pending.take_if(|request| {
            request.selection_generation == selection_generation
                && request.series_generation == series_generation
        }) else {
            return false;
        };
        self.selected = Some(request);
        true
    }

    pub(crate) fn reject(&mut self, series_generation: NonZeroUsize) -> bool {
        self.pending
            .take_if(|request| request.series_generation == series_generation)
            .is_some()
    }

    pub(crate) fn reset(&mut self) {
        self.pending = None;
        self.selected = None;
    }

    pub(crate) const fn selected(&self) -> Option<RithmicSeriesRequest> {
        self.selected
    }
}

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
    Shutdown,
}

pub(crate) struct RithmicHistoryTask {
    commands: SyncSender<HistoryCommand>,
    results: Receiver<RithmicHistoryResult>,
    active: Option<(NonZeroUsize, NonZeroUsize, Arc<AtomicBool>)>,
    handle: Option<JoinHandle<()>>,
}

impl RithmicHistoryTask {
    pub(crate) fn start() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::sync_channel(HISTORY_COMMAND_CAPACITY);
        let (result_tx, result_rx) = mpsc::sync_channel(HISTORY_RESULT_CAPACITY);
        let handle = thread::Builder::new()
            .name("axiusflow-rithmic-history-worker".to_string())
            .spawn(move || run_history_worker(&command_rx, &result_tx))
            .map_err(|_| "Rithmic history worker is unavailable".to_string())?;
        Ok(Self {
            commands: command_tx,
            results: result_rx,
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
        match self.results.try_recv() {
            Ok(result) => {
                if self.active.as_ref().is_some_and(
                    |(selection_generation, series_generation, _)| {
                        *selection_generation == result.selection_generation
                            && *series_generation == result.series_generation
                    },
                ) {
                    self.active = None;
                }
                Some(result)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
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
        let _ = self.commands.try_send(HistoryCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_history_worker(
    commands: &Receiver<HistoryCommand>,
    results: &SyncSender<RithmicHistoryResult>,
) {
    while let Ok(command) = commands.recv() {
        let HistoryCommand::Fetch(request) = command else {
            return;
        };
        let result = fetch_history(&request).map(Box::new);
        let _ = results.try_send(RithmicHistoryResult {
            selection_generation: request.selection_generation,
            series_generation: request.series_generation,
            result,
        });
    }
}

fn fetch_history(request: &HistoryFetchRequest) -> Result<MarketWorkerBootstrap, String> {
    let range = visible_range(request.series, SystemTime::now())?;
    let resolution = request.series.resolution()?;
    let connection = connect_history(Arc::clone(&request.stop))?;
    let transport = RithmicHistorySessionTransport::try_new(
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
    let start_seconds = i32::try_from(range.start_unix_nanos / NANOS_PER_SECOND)
        .map_err(|_| "Rithmic history range is invalid".to_string())?;
    let finish_seconds = i32::try_from(range.end_unix_nanos / NANOS_PER_SECOND)
        .map_err(|_| "Rithmic history range is invalid".to_string())?;
    let interval_seconds = i32::try_from(
        request
            .series
            .interval_seconds()
            .ok_or_else(|| "Rithmic history series is invalid".to_string())?,
    )
    .map_err(|_| "Rithmic history series is invalid".to_string())?;
    let collection_start = start_seconds
        .checked_sub(interval_seconds)
        .ok_or_else(|| "Rithmic history range is invalid".to_string())?;
    let collection_finish = finish_seconds
        .checked_add(interval_seconds)
        .ok_or_else(|| "Rithmic history range is invalid".to_string())?;
    let mut transport = transport;
    let collected = transport.collect_history(HistoryCollectionRequest {
        symbol: provider_instrument.descriptor.provider_symbol.clone(),
        exchange: provider_instrument.descriptor.venue_id.clone(),
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
        maximum_bars: NonZeroUsize::new(MAXIMUM_VISIBLE_BARS).unwrap_or(NonZeroUsize::MIN),
    })?;
    let HistoryBars::Time(decoded) = collected.bars else {
        return Err("Rithmic history returned the wrong series".to_string());
    };
    let bars = decoded
        .iter()
        .filter(|bar| bar.marker_seconds >= start_seconds && bar.marker_seconds <= finish_seconds)
        .enumerate()
        .map(|(index, bar)| {
            canonical_rithmic_time_bar(&provider_instrument, &resolution, bar)
                .map(|sequenced| {
                    let mut bar = sequenced.value;
                    bar.source_sequence =
                        u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
                    bar
                })
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    bootstrap_from_bars(request, bars, unix_nanos_now()?)
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

fn visible_range(series: RithmicSeries, now: SystemTime) -> Result<HistoryRange, String> {
    let interval = series
        .interval_seconds()
        .ok_or_else(|| "Historical tick continuity is unavailable".to_string())?;
    let now_seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is invalid".to_string())?
        .as_secs();
    let end_seconds = now_seconds - (now_seconds % interval);
    let span_seconds = interval
        .checked_mul(MAXIMUM_VISIBLE_BARS as u64)
        .ok_or_else(|| "Rithmic visible range overflowed".to_string())?;
    let start_seconds = end_seconds
        .checked_sub(span_seconds)
        .ok_or_else(|| "Rithmic visible range underflowed".to_string())?;
    Ok(HistoryRange {
        start_unix_nanos: i64::try_from(start_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
            .ok_or_else(|| "Rithmic visible range overflowed".to_string())?,
        end_unix_nanos: i64::try_from(end_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
            .ok_or_else(|| "Rithmic visible range overflowed".to_string())?,
    })
}

fn bootstrap_from_bars(
    request: &HistoryFetchRequest,
    bars: Vec<MarketBar>,
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
    let interval_seconds = u32::try_from(
        request
            .series
            .interval_seconds()
            .ok_or_else(|| "Historical tick continuity is unavailable".to_string())?,
    )
    .map_err(|_| "Rithmic interval is invalid".to_string())?;
    let definition = BarDefinition {
        definition_id: format!("rithmic:{}:unadjusted:v1", request.series.label()),
        version: 1,
        interval_seconds,
    };
    let ownership_epoch = u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX);
    let provenanced = bars
        .into_iter()
        .map(|bar| {
            let provenance = history_provenance(request, &bar, received_unix_nanos)?;
            let item = Provenanced::new(bar, provenance);
            validate_provenanced_market_bar(&item).map_err(|error| error.to_string())?;
            Ok(item)
        })
        .collect::<Result<Vec<ProvenancedMarketBar>, String>>()?;
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        instrument,
        ReplayProvenance::LiveProvider,
        definition,
        ownership_epoch,
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
    received_unix_nanos: i64,
) -> Result<MarketEventProvenance, String> {
    let exchange = bar
        .exchange_timestamp_seconds
        .checked_mul(NANOS_PER_SECOND)
        .ok_or_else(|| "Rithmic history timestamp overflowed".to_string())?;
    Ok(MarketEventProvenance {
        event_id: format!(
            "rithmic_history_{}_{}",
            request.series.label(),
            bar.exchange_timestamp_seconds
        ),
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
        partition_id: 0,
        ownership_epoch: u64::try_from(request.series_generation.get()).unwrap_or(u64::MAX),
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

    #[test]
    fn series_browser_fences_replaced_selection_and_series_generations() {
        let mut browser = RithmicSeriesBrowser::default();
        let selection = NonZeroUsize::MIN;
        let first = browser.select(selection, RithmicSeries::Minute1);
        let second = browser.select(selection, RithmicSeries::Minute5);
        assert!(!browser.accept(first.selection_generation, first.series_generation));
        assert!(browser.accept(second.selection_generation, second.series_generation));
        assert_eq!(browser.selected(), Some(second));
        let replacement = browser.select(
            NonZeroUsize::new(2).expect("selection generation is nonzero"),
            RithmicSeries::Daily,
        );
        assert!(!browser.accept(second.selection_generation, second.series_generation));
        assert!(browser.accept(
            replacement.selection_generation,
            replacement.series_generation
        ));
        assert_eq!(browser.selected(), Some(replacement));
        browser.reset();
        assert_eq!(browser.selected(), None);
    }

    #[test]
    fn visible_ranges_are_aligned_and_capped_at_three_hundred_bars() {
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
            let range = visible_range(series, now).expect("range validates");
            assert_eq!(range.start_unix_nanos % (interval * NANOS_PER_SECOND), 0);
            assert_eq!(range.end_unix_nanos % (interval * NANOS_PER_SECOND), 0);
            assert_eq!(
                range.end_unix_nanos - range.start_unix_nanos,
                interval
                    * NANOS_PER_SECOND
                    * i64::try_from(MAXIMUM_VISIBLE_BARS).expect("visible bound fits")
            );
        }
        assert!(visible_range(RithmicSeries::Tick, now).is_err());
    }

    #[test]
    fn stale_result_does_not_release_newer_cancellation_owner() {
        let (command_tx, _command_rx) = mpsc::sync_channel(HISTORY_COMMAND_CAPACITY);
        let (result_tx, result_rx) = mpsc::sync_channel(HISTORY_RESULT_CAPACITY);
        let selection = NonZeroUsize::MIN;
        let stale = NonZeroUsize::MIN;
        let latest = NonZeroUsize::new(2).expect("generation is nonzero");
        let stop = Arc::new(AtomicBool::new(false));
        let mut task = RithmicHistoryTask {
            commands: command_tx,
            results: result_rx,
            active: Some((selection, latest, Arc::clone(&stop))),
            handle: None,
        };
        result_tx
            .send(RithmicHistoryResult {
                selection_generation: selection,
                series_generation: stale,
                result: Err("cancelled".to_string()),
            })
            .expect("stale result sends");
        assert!(task.try_recv().is_some());
        assert!(task.active.is_some());
        assert!(!stop.load(Ordering::Acquire));
        result_tx
            .send(RithmicHistoryResult {
                selection_generation: selection,
                series_generation: latest,
                result: Err("latest failed".to_string()),
            })
            .expect("latest result sends");
        assert!(task.try_recv().is_some());
        assert!(task.active.is_none());
    }
}
