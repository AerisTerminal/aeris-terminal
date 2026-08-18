//! Engine-owned authenticated Rithmic history collection and bounded replay planning.

use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axiusflow_engine_protocol::InstallProviderInstrument;
use axiusflow_market_data::{
    BarPeriod, BarSeriesKey, ChartAggregation, ChartInterval, MarketBar, RithmicChartAggregation,
    RithmicDailyAggregation,
};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_provider_history::HistoryRange;
use axiusflow_rithmic_protocol_adapter::{
    InstrumentDescriptor, RITHMIC_TEST_VAULT_KEY, RITHMIC_TEST_VAULT_SERVICE, RithmicApplication,
    RithmicCredentialBytes, RithmicHistorySessionTransport, RithmicProviderInstrument,
    RithmicSessionLimits, RithmicTestSession, collect_rithmic_chart_history,
};
use zeroize::Zeroize;

const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);
const MAXIMUM_CONTROL_MESSAGES: usize = 64;
const MAXIMUM_VISIBLE_BARS: usize = 300;
const MAXIMUM_REPLAY_BARS: usize = 10_000;
const MAXIMUM_NON_TRADING_GAP_SECONDS: u64 = 4 * 24 * 60 * 60;
const DAILY_SESSION_PADDING_BARS: usize = 150;
const NANOS_PER_SECOND: i64 = 1_000_000_000;

pub(super) struct Snapshot {
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    pub(super) bars: Vec<MarketBar>,
}

pub(super) fn fetch(
    series: &BarSeriesKey,
    provider_generation: u64,
    installed: &InstallProviderInstrument,
    stop: &Arc<AtomicBool>,
) -> Result<Snapshot, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Rithmic history request was cancelled".to_string());
    }
    if installed.provider != "rithmic"
        || installed.instrument_id != series.instrument_id
        || installed.entitlement_id != series.entitlement_id
        || installed.session_generation != provider_generation
        || series.definition_version != 1
    {
        return Err("Rithmic history identity is inconsistent".to_string());
    }
    let interval = chart_interval(series.period)?;
    let replay = replay_envelope(interval, SystemTime::now())?;
    let connection = connect(Arc::clone(stop))?;
    let mut transport = RithmicHistorySessionTransport::try_new(
        connection,
        REPLAY_TIMEOUT,
        NonZeroUsize::new(MAXIMUM_CONTROL_MESSAGES).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(|_| "Rithmic history transport is unavailable".to_string())?;
    let instrument = provider_instrument(installed)?;
    let mut bars = collect_rithmic_chart_history(
        &mut transport,
        &instrument,
        interval,
        replay.range,
        replay.maximum_bars,
    )
    .map_err(|error| error.to_string())?;
    bars.sort_unstable_by_key(|bar| bar.exchange_timestamp_unix_nanos);
    if bars.len() > MAXIMUM_VISIBLE_BARS {
        bars.drain(..bars.len() - MAXIMUM_VISIBLE_BARS);
    }
    if bars.is_empty() {
        return Err("Rithmic returned no completed historical bars".to_string());
    }
    for (index, bar) in bars.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "Rithmic history sequence overflowed".to_string())?;
    }
    Ok(Snapshot {
        price_scale: instrument.descriptor.price_scale,
        quantity_scale: instrument.descriptor.quantity_scale,
        bars,
    })
}

fn provider_instrument(
    installed: &InstallProviderInstrument,
) -> Result<RithmicProviderInstrument, String> {
    Ok(RithmicProviderInstrument {
        descriptor: InstrumentDescriptor {
            instrument_id: installed.instrument_id.clone(),
            provider_symbol: installed.provider_symbol.clone(),
            display_symbol: installed.display_symbol.clone(),
            venue_id: installed.venue_id.clone(),
            price_scale: u8::try_from(installed.price_scale)
                .map_err(|_| "Rithmic price scale is invalid".to_string())?,
            quantity_scale: u8::try_from(installed.quantity_scale)
                .map_err(|_| "Rithmic quantity scale is invalid".to_string())?,
        },
        entitlement_id: installed.entitlement_id.clone(),
        trades: true,
        quotes: true,
        order_book: false,
    })
}

fn connect(
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

#[derive(Clone, Copy)]
struct ReplayEnvelope {
    range: HistoryRange,
    maximum_bars: NonZeroUsize,
}

fn replay_envelope(interval: ChartInterval, now: SystemTime) -> Result<ReplayEnvelope, String> {
    let now_seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is invalid".to_string())?
        .as_secs();
    if let Some(RithmicChartAggregation::DailySessions { period }) = interval.rithmic_aggregation()
    {
        return aggregate_replay_envelope(now_seconds, period);
    }
    let interval_seconds = match interval.aggregation() {
        ChartAggregation::FixedSeconds(seconds) => u64::from(seconds.get()),
        ChartAggregation::Trades(_) | ChartAggregation::CalendarMonth => 60,
    };
    let end_seconds = if interval == ChartInterval::Tick100 {
        now_seconds
    } else {
        now_seconds - now_seconds % interval_seconds
    };
    let requested_bars = if interval == ChartInterval::Day1 {
        MAXIMUM_VISIBLE_BARS.saturating_add(DAILY_SESSION_PADDING_BARS)
    } else {
        MAXIMUM_VISIBLE_BARS
    };
    let span_seconds = interval_seconds
        .checked_mul(requested_bars as u64)
        .and_then(|span| span.checked_add(MAXIMUM_NON_TRADING_GAP_SECONDS))
        .ok_or_else(|| "Rithmic visible range overflowed".to_string())?;
    let start_seconds = end_seconds
        .checked_sub(span_seconds)
        .ok_or_else(|| "Rithmic visible range underflowed".to_string())?;
    let theoretical_bars = if interval == ChartInterval::Tick100 {
        MAXIMUM_REPLAY_BARS
    } else {
        usize::try_from(span_seconds.div_ceil(interval_seconds))
            .ok()
            .and_then(|bars| bars.checked_add(3))
            .ok_or_else(|| "Rithmic visible range overflowed".to_string())?
    };
    if theoretical_bars > MAXIMUM_REPLAY_BARS {
        return Err("Rithmic visible range exceeds replay capacity".to_string());
    }
    Ok(ReplayEnvelope {
        range: history_range(start_seconds, end_seconds)?,
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
        range: history_range(start_seconds, end_seconds)?,
        maximum_bars: NonZeroUsize::new(maximum_bars).unwrap_or(NonZeroUsize::MIN),
    })
}

fn history_range(start_seconds: u64, end_seconds: u64) -> Result<HistoryRange, String> {
    Ok(HistoryRange {
        start_unix_nanos: i64::try_from(start_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
            .ok_or_else(|| "Rithmic history range overflowed".to_string())?,
        end_unix_nanos: i64::try_from(end_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
            .ok_or_else(|| "Rithmic history range overflowed".to_string())?,
    })
}

pub(super) fn chart_interval(period: BarPeriod) -> Result<ChartInterval, String> {
    match period {
        BarPeriod::Tick { trades: 100 } => Ok(ChartInterval::Tick100),
        BarPeriod::Time { seconds: 60 } => Ok(ChartInterval::Minute1),
        BarPeriod::Time { seconds: 180 } => Ok(ChartInterval::Minute3),
        BarPeriod::Time { seconds: 300 } => Ok(ChartInterval::Minute5),
        BarPeriod::Time { seconds: 900 } => Ok(ChartInterval::Minute15),
        BarPeriod::Time { seconds: 1_800 } => Ok(ChartInterval::Minute30),
        BarPeriod::Time { seconds: 3_600 } => Ok(ChartInterval::Hour1),
        BarPeriod::Time { seconds: 7_200 } => Ok(ChartInterval::Hour2),
        BarPeriod::Time { seconds: 14_400 } => Ok(ChartInterval::Hour4),
        BarPeriod::Time { seconds: 28_800 } => Ok(ChartInterval::Hour8),
        BarPeriod::Time { seconds: 43_200 } => Ok(ChartInterval::Hour12),
        BarPeriod::Session { days: 1 } => Ok(ChartInterval::Day1),
        BarPeriod::Session { days: 3 } => Ok(ChartInterval::Day3),
        BarPeriod::Week { weeks: 1 } => Ok(ChartInterval::Week1),
        BarPeriod::Month { months: 1 } => Ok(ChartInterval::Month1),
        BarPeriod::Tick { .. }
        | BarPeriod::Time { .. }
        | BarPeriod::Session { .. }
        | BarPeriod::Week { .. }
        | BarPeriod::Month { .. } => Err("unsupported Rithmic engine interval".to_string()),
    }
}
