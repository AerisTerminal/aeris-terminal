//! Engine-owned authenticated Rithmic history collection and bounded replay planning.

use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aeris_contracts::InstallProviderInstrument;
use aeris_market_data::{
    BarPeriod, BarSeriesKey, ChartAggregation, ChartInterval, MarketBar, RithmicChartAggregation,
    RithmicDailyAggregation,
};
use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use aeris_provider_history::HistoryRange;

use crate::market_service::{FormingBar, HistoryFetchWindow};
use aeris_rithmic_protocol_adapter::{
    InstrumentContractMetadata, InstrumentDescriptor, RITHMIC_APPLICATION_NAME,
    RITHMIC_TEST_VAULT_KEY, RITHMIC_TEST_VAULT_SERVICE, RithmicApplication, RithmicCredentialBytes,
    RithmicHistorySessionTransport, RithmicProviderInstrument, RithmicSessionLimits,
    RithmicTestSession, collect_rithmic_chart_history, collect_rithmic_trade_history,
};
use zeroize::Zeroize;

const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);
const MAXIMUM_CONTROL_MESSAGES: usize = 64;
const MAXIMUM_REPLAY_BARS: usize = 10_000;
const MAXIMUM_NON_TRADING_GAP_SECONDS: u64 = 4 * 24 * 60 * 60;
const DAILY_SESSION_PADDING_BARS: usize = 150;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// Upper bound on the one-trade replay that rebuilds an open tick bundle.
const MAXIMUM_FORMING_TRADES: usize = 1_000;

pub(super) struct Snapshot {
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    /// Periods the provider has closed. Only these become canonical history.
    pub(super) bars: Vec<MarketBar>,
    /// The period that was still open when the replay ran.
    pub(super) forming: Option<FormingBar>,
    pub(super) handoff_boundary_unix_nanos: i64,
}

/// How the trailing open period is separated from closed history.
#[derive(Clone, Copy)]
enum FormingPlan {
    /// A returned bar at or after this instant belongs to the open bucket.
    Bucket { closed_before_unix_nanos: i64 },
    /// Fold the trades after the newest complete bundle into the open bundle.
    Bundle { trades_per_bar: u32 },
    /// The provider only serves closed periods for this interval.
    Closed,
}

pub(super) fn fetch(
    series: &BarSeriesKey,
    provider_generation: u64,
    installed: &InstallProviderInstrument,
    window: HistoryFetchWindow,
    stop: &Arc<AtomicBool>,
) -> Result<Snapshot, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Rithmic history request was cancelled".to_string());
    }
    validate_history_identity(series, provider_generation, installed)?;
    let interval = chart_interval(series.period)?;
    let maximum_visible_bars = bounded_visible_bars(window.maximum_bars);
    let replay = match window.range {
        Some(range) => explicit_replay_envelope(maximum_visible_bars, range)?,
        None => replay_envelope(interval, maximum_visible_bars, SystemTime::now())?,
    };
    let connection = connect(Arc::clone(stop))?;
    let mut transport = RithmicHistorySessionTransport::try_new(
        connection,
        REPLAY_TIMEOUT,
        NonZeroUsize::new(MAXIMUM_CONTROL_MESSAGES).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(|_| "Rithmic history transport is unavailable".to_string())?;
    let instrument = provider_instrument(installed)?;
    // Both replays run on one session, because the tick case needs a second
    // one-trade pass to rebuild the bundle that is still open.
    let collected = collect_replay(
        &mut transport,
        &instrument,
        interval,
        replay,
        maximum_visible_bars,
        stop,
    );
    transport.close().map_err(|error| error.to_string())?;
    let (bars, forming, handoff_boundary_unix_nanos) = collected?;
    Ok(Snapshot {
        price_scale: instrument.descriptor.price_scale,
        quantity_scale: instrument.descriptor.quantity_scale,
        bars,
        forming,
        handoff_boundary_unix_nanos,
    })
}

fn bounded_visible_bars(requested: usize) -> usize {
    requested.clamp(1, MAXIMUM_REPLAY_BARS)
}

fn validate_history_identity(
    series: &BarSeriesKey,
    provider_generation: u64,
    installed: &InstallProviderInstrument,
) -> Result<(), String> {
    // Instrument metadata is retained across provider reconnects. Its install
    // generation records when that metadata was resolved; the HistoryRequest's
    // provider generation is the current engine/session fence. Requiring those
    // to be equal forces an unrelated catalog re-selection after every
    // reconnect and makes multi-instrument recovery impossible.
    let mismatch = if provider_generation == 0 {
        Some("provider generation is unset")
    } else if installed.provider != "rithmic" {
        Some("installed provider is not Rithmic")
    } else if installed.instrument_id != series.instrument_id {
        Some("installed instrument differs from the series")
    } else if installed.entitlement_id != series.entitlement_id {
        Some("installed entitlement differs from the series")
    } else if installed.session_generation == 0 {
        Some("installed session generation is unset")
    } else if installed.session_generation > provider_generation {
        Some("installed session is newer than the request")
    } else if series.definition_version != 1 {
        Some("series definition version is unsupported")
    } else {
        None
    };
    mismatch.map_or(Ok(()), |reason| {
        Err(format!(
            "Rithmic history identity is inconsistent: {reason}"
        ))
    })
}

/// Runs both replay passes and returns closed history, the open period, and the
/// handoff boundary.
fn collect_replay(
    transport: &mut RithmicHistorySessionTransport,
    instrument: &RithmicProviderInstrument,
    interval: ChartInterval,
    replay: ReplayEnvelope,
    maximum_visible_bars: usize,
    stop: &Arc<AtomicBool>,
) -> Result<(Vec<MarketBar>, Option<FormingBar>, i64), String> {
    let mut bars = collect_rithmic_chart_history(
        transport,
        instrument,
        interval,
        replay.range,
        replay.maximum_bars,
    )
    .map_err(|error| error.to_string())?;
    bars.sort_unstable_by_key(|bar| bar.exchange_timestamp_unix_nanos);
    if bars.len() > maximum_visible_bars {
        bars.drain(..bars.len() - maximum_visible_bars);
    }
    if bars.is_empty() {
        return Ok(empty_replay_result(replay));
    }
    for (index, bar) in bars.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "Rithmic history sequence overflowed".to_string())?;
    }
    let (forming, boundary) = split_forming_period(transport, instrument, replay, &mut bars, stop)?;
    Ok((bars, forming, boundary))
}

/// A replay that completed with no bars is the provider's answer for that
/// window, not a transport fault: retrying the same window returns the same
/// nothing and costs another history-plant login. It is reported as an empty
/// snapshot so the market service records it once instead of retrying.
fn empty_replay_result(replay: ReplayEnvelope) -> (Vec<MarketBar>, Option<FormingBar>, i64) {
    (Vec::new(), None, replay.range.end_unix_nanos)
}

/// Separates the period the replay caught mid-flight from closed history.
///
/// History is a record of closed periods; a half-built bar written into it stays
/// wrong until the period ends, which on an hourly or daily chart is a long time
/// to show bad data. The open period is handed to the live handoff instead,
/// which is what gives a freshly selected chart the OHLCV that accrued before
/// the trader got there.
///
/// The returned instant is the handoff boundary: every buffered trade at or
/// before it is already inside what is returned here, and replaying it would
/// count it twice.
fn split_forming_period(
    transport: &mut RithmicHistorySessionTransport,
    instrument: &RithmicProviderInstrument,
    replay: ReplayEnvelope,
    bars: &mut Vec<MarketBar>,
    stop: &Arc<AtomicBool>,
) -> Result<(Option<FormingBar>, i64), String> {
    match replay.forming {
        FormingPlan::Closed => Ok((None, replay.range.end_unix_nanos)),
        FormingPlan::Bucket {
            closed_before_unix_nanos,
        } => {
            // One completed bar always has to remain: a series whose only bar is
            // still forming has no history to hand over.
            let open = bars.len() > 1
                && bars.last().is_some_and(|bar| {
                    bar.exchange_timestamp_unix_nanos >= closed_before_unix_nanos
                });
            if !open {
                return Ok((None, replay.range.end_unix_nanos));
            }
            let bar = bars
                .pop()
                .ok_or_else(|| "Rithmic history is empty".to_string())?;
            // The provider built this page at some instant no later than the end
            // of the replay window, so trades after that window are certainly
            // absent from it.
            Ok((
                Some(FormingBar { bar, trades: None }),
                replay.range.end_unix_nanos,
            ))
        }
        FormingPlan::Bundle { trades_per_bar } => {
            if stop.load(Ordering::Acquire) {
                return Err("Rithmic history request was cancelled".to_string());
            }
            let newest = bars
                .last()
                .ok_or_else(|| "Rithmic history is empty".to_string())?;
            let after_unix_nanos = newest.exchange_timestamp_unix_nanos;
            let sequence = newest
                .source_sequence
                .checked_add(1)
                .ok_or_else(|| "Rithmic history sequence overflowed".to_string())?;
            let forming = forming_tick_bundle(
                transport,
                instrument,
                trades_per_bar,
                after_unix_nanos,
                replay.range.end_unix_nanos,
                sequence,
            )?;
            let boundary = forming.as_ref().map_or(after_unix_nanos, |forming| {
                forming.bar.exchange_timestamp_unix_nanos
            });
            Ok((forming, boundary))
        }
    }
}

/// Rebuilds the still-open tick bundle from one-trade history.
///
/// Returns `None` when the bundle cannot be reconstructed unambiguously — no
/// trades since the newest complete bundle, or so many that complete bundles
/// must have been missed. Guessing there would put a fabricated candle on the
/// chart, which is worse than opening the bundle on the next live trade.
fn forming_tick_bundle(
    transport: &mut RithmicHistorySessionTransport,
    instrument: &RithmicProviderInstrument,
    trades_per_bar: u32,
    after_unix_nanos: i64,
    end_unix_nanos: i64,
    source_sequence: u64,
) -> Result<Option<FormingBar>, String> {
    let start_unix_nanos = after_unix_nanos
        .div_euclid(NANOS_PER_SECOND)
        .checked_mul(NANOS_PER_SECOND)
        .ok_or_else(|| "Rithmic forming range overflowed".to_string())?;
    if start_unix_nanos >= end_unix_nanos {
        return Ok(None);
    }
    let trades = collect_rithmic_trade_history(
        transport,
        instrument,
        HistoryRange {
            start_unix_nanos,
            end_unix_nanos,
        },
        NonZeroUsize::new(MAXIMUM_FORMING_TRADES).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(|error| error.to_string())?;
    let mut open = trades
        .into_iter()
        .filter(|trade| trade.exchange_timestamp_unix_nanos > after_unix_nanos)
        .collect::<Vec<_>>();
    open.sort_unstable_by_key(|trade| trade.exchange_timestamp_unix_nanos);
    if open.is_empty() || open.len() >= usize::try_from(trades_per_bar).unwrap_or(usize::MAX) {
        return Ok(None);
    }
    let first = *open
        .first()
        .ok_or_else(|| "Rithmic forming bundle is empty".to_string())?;
    let last = *open
        .last()
        .ok_or_else(|| "Rithmic forming bundle is empty".to_string())?;
    let mut bar = MarketBar {
        source_sequence,
        exchange_timestamp_seconds: last.exchange_timestamp_seconds,
        exchange_timestamp_unix_nanos: last.exchange_timestamp_unix_nanos,
        open: first.open,
        high: first.high,
        low: first.low,
        close: last.close,
        volume: 0,
    };
    for trade in &open {
        bar.high = bar.high.max(trade.high);
        bar.low = bar.low.min(trade.low);
        bar.volume = bar
            .volume
            .checked_add(trade.volume)
            .ok_or_else(|| "Rithmic forming volume overflowed".to_string())?;
    }
    bar.validate().map_err(|error| error.to_string())?;
    Ok(Some(FormingBar {
        bar,
        trades: Some(u32::try_from(open.len()).unwrap_or(trades_per_bar)),
    }))
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
            price_increment: installed.price_increment,
            contract: installed.contract_metadata.as_deref().map(|metadata| {
                Box::new(InstrumentContractMetadata {
                    point_value: metadata
                        .point_value
                        .zip(metadata.point_value_scale)
                        .and_then(|(value, scale)| {
                            u8::try_from(scale).ok().map(|scale| (value, scale))
                        }),
                    currency: metadata.currency.clone(),
                    expiration_date: metadata.contract_expiry.clone(),
                    first_notice_date: metadata.first_notice_date.clone(),
                    last_trade_date: metadata.last_trade_date.clone(),
                })
            }),
        },
        entitlement_id: installed.entitlement_id.clone(),
        trades: true,
        quotes: true,
        order_book: false,
    })
}

fn connect(
    stop: Arc<AtomicBool>,
) -> Result<aeris_rithmic_protocol_adapter::RithmicHistoryConnection, String> {
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
            name: RITHMIC_APPLICATION_NAME,
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
    forming: FormingPlan,
}

fn replay_envelope(
    interval: ChartInterval,
    maximum_visible_bars: usize,
    now: SystemTime,
) -> Result<ReplayEnvelope, String> {
    let now_seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is invalid".to_string())?
        .as_secs();
    if let Some(RithmicChartAggregation::DailySessions { period }) = interval.rithmic_aggregation()
    {
        return aggregate_replay_envelope(now_seconds, period, maximum_visible_bars);
    }
    let interval_seconds = match interval.aggregation() {
        ChartAggregation::FixedSeconds(seconds) => u64::from(seconds.get()),
        ChartAggregation::Trades(_) | ChartAggregation::CalendarMonth => 60,
    };
    // The window runs to *now*, not to the last closed boundary, so the reply
    // also carries the period that is still open. It is split back out in
    // `split_forming_period`; history keeps only closed periods.
    let closed_before_seconds = now_seconds - now_seconds % interval_seconds;
    let end_seconds = now_seconds;
    let forming = if interval == ChartInterval::Tick100 {
        FormingPlan::Bundle {
            trades_per_bar: match interval.aggregation() {
                ChartAggregation::Trades(trades) => trades.get(),
                ChartAggregation::FixedSeconds(_) | ChartAggregation::CalendarMonth => {
                    return Err("Rithmic tick interval has no trade count".to_string());
                }
            },
        }
    } else {
        FormingPlan::Bucket {
            closed_before_unix_nanos: i64::try_from(closed_before_seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
                .ok_or_else(|| "Rithmic forming boundary overflowed".to_string())?,
        }
    };
    let requested_bars = if interval == ChartInterval::Day1 {
        maximum_visible_bars.saturating_add(DAILY_SESSION_PADDING_BARS)
    } else {
        maximum_visible_bars
    };
    let span_seconds = interval_seconds
        .checked_mul(requested_bars as u64)
        .and_then(|span| span.checked_add(MAXIMUM_NON_TRADING_GAP_SECONDS))
        .ok_or_else(|| "Rithmic visible range overflowed".to_string())?;
    let start_seconds = closed_before_seconds
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
        forming,
    })
}

fn explicit_replay_envelope(
    maximum_visible_bars: usize,
    range: HistoryRange,
) -> Result<ReplayEnvelope, String> {
    if range.start_unix_nanos >= range.end_unix_nanos {
        return Err("Rithmic visible history range is empty".to_string());
    }
    let range = align_explicit_replay_range(range)?;
    Ok(ReplayEnvelope {
        range,
        maximum_bars: NonZeroUsize::new(maximum_visible_bars.clamp(1, MAXIMUM_REPLAY_BARS))
            .unwrap_or(NonZeroUsize::MIN),
        // Viewport repairs are behind the installed live edge, so they never
        // invent an open bucket. The existing realtime handoff remains owner
        // of the forming candle while the repair replaces completed history.
        forming: FormingPlan::Closed,
    })
}

fn align_explicit_replay_range(range: HistoryRange) -> Result<HistoryRange, String> {
    let start_seconds = range.start_unix_nanos.div_euclid(NANOS_PER_SECOND);
    let mut end_seconds = range.end_unix_nanos.div_euclid(NANOS_PER_SECOND);
    if range.end_unix_nanos.rem_euclid(NANOS_PER_SECOND) != 0 {
        end_seconds = end_seconds
            .checked_add(1)
            .ok_or_else(|| "Rithmic visible history range overflowed".to_string())?;
    }
    let start_unix_nanos = start_seconds
        .checked_mul(NANOS_PER_SECOND)
        .ok_or_else(|| "Rithmic visible history range overflowed".to_string())?;
    let end_unix_nanos = end_seconds
        .checked_mul(NANOS_PER_SECOND)
        .ok_or_else(|| "Rithmic visible history range overflowed".to_string())?;
    if start_unix_nanos >= end_unix_nanos {
        return Err("Rithmic visible history range is empty".to_string());
    }
    Ok(HistoryRange {
        start_unix_nanos,
        end_unix_nanos,
    })
}

fn aggregate_replay_envelope(
    now_seconds: u64,
    period: RithmicDailyAggregation,
    maximum_visible_bars: usize,
) -> Result<ReplayEnvelope, String> {
    const DAY_SECONDS: u64 = 24 * 60 * 60;
    let maximum_days_per_bar = match period {
        RithmicDailyAggregation::Week => 7,
        RithmicDailyAggregation::Month => 31,
    };
    let source_days = maximum_visible_bars
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
        // Weeks and months are aggregated locally from daily session bars, and
        // the venue calendar — not a clock — decides where their buckets end.
        // Splitting an open one here would need that calendar, so the live
        // handoff opens it from trades as it always has.
        forming: FormingPlan::Closed,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            entitlement_id: "rithmic-test:CME:MNQ".to_string(),
            period: BarPeriod::time(60).expect("period"),
            definition_version: 1,
        }
    }

    fn installed(session_generation: u64) -> InstallProviderInstrument {
        let series = series();
        InstallProviderInstrument {
            provider: series.provider_id,
            session_generation,
            selection_generation: 1,
            instrument_id: series.instrument_id,
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQ Sep 2026".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: series.entitlement_id,
            price_increment: Some(25),
            ..Default::default()
        }
    }

    #[test]
    fn retained_instrument_metadata_is_valid_for_newer_history_generation() {
        assert!(validate_history_identity(&series(), 7, &installed(3)).is_ok());
        assert!(validate_history_identity(&series(), 7, &installed(8)).is_err());
        assert!(validate_history_identity(&series(), 0, &installed(3)).is_err());
    }

    #[test]
    fn identity_rejection_names_the_field_that_disagrees() {
        let newer = validate_history_identity(&series(), 7, &installed(8)).unwrap_err();
        assert!(newer.ends_with("installed session is newer than the request"));
        let mut other = installed(3);
        other.entitlement_id = "rithmic-test:CME:ES".to_string();
        let entitlement = validate_history_identity(&series(), 7, &other).unwrap_err();
        assert!(entitlement.ends_with("installed entitlement differs from the series"));
    }

    #[test]
    fn history_budget_preserves_runtime_requests_up_to_protocol_capacity() {
        assert_eq!(bounded_visible_bars(600), 600);
        assert_eq!(bounded_visible_bars(8_192), 8_192);
        assert_eq!(
            bounded_visible_bars(MAXIMUM_REPLAY_BARS + 1),
            MAXIMUM_REPLAY_BARS
        );
    }

    #[test]
    fn explicit_viewport_range_aligns_to_rithmic_whole_seconds() {
        let replay = explicit_replay_envelope(
            600,
            HistoryRange {
                start_unix_nanos: 1_234_567_890,
                end_unix_nanos: 61_234_567_890,
            },
        )
        .expect("fractional viewport range aligns");
        assert_eq!(replay.range.start_unix_nanos, 1_000_000_000);
        assert_eq!(replay.range.end_unix_nanos, 62_000_000_000);

        let (bars, forming, boundary) = empty_replay_result(replay);
        assert!(bars.is_empty());
        assert!(forming.is_none());
        assert_eq!(boundary, 62_000_000_000);
    }

    #[test]
    fn empty_current_replay_is_reported_as_an_empty_snapshot_not_an_error() {
        let now = UNIX_EPOCH + Duration::from_hours(240);
        let replay =
            replay_envelope(ChartInterval::Minute1, 100, now).expect("current replay envelope");

        let (bars, forming, boundary) = empty_replay_result(replay);
        assert!(bars.is_empty());
        assert!(forming.is_none());
        assert_eq!(boundary, replay.range.end_unix_nanos);
    }
}
