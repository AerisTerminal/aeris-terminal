use axiusflow_coinbase_market_adapter::{
    CoinbaseInterval, aggregate_coinbase_bars, decode_history_segment, encode_history_bar,
    encode_history_segment,
};
use axiusflow_desktop_storage::{
    CatalogKey, DataKind, HistoryRead, HistoryScope, HistorySeriesIdentity, HistoryStore,
    PublicationOutcome, PublicationRequest, RecoveryAction, RetainedRange, RetentionPolicy,
    SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_market_data::MarketBar;
use axiusflow_provider_history::{CoverageClass, HistoryItem, HistoryRange};
use serde::Serialize;
use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{Pid, ProcessesToUpdate, System};

const DEFAULT_BAR_COUNT: usize = 250_000;
const SEGMENT_BARS: usize = 350;
const BAR_SECONDS: i64 = 60;
const NANOS_PER_SECOND: i64 = 1_000_000_000;

#[derive(Serialize)]
struct Evidence {
    schema_version: u32,
    hardware: String,
    operating_system: String,
    bar_count: usize,
    segment_count: usize,
    payload_bytes: u64,
    stored_bytes: u64,
    cold_publish_millis: u64,
    warm_open_millis: u64,
    warm_discovery_millis: u64,
    time_to_first_usable_micros: u64,
    time_to_first_usable_millis: u64,
    warm_read_millis: u64,
    decode_millis: u64,
    timeframe_switches: Vec<TimeframeEvidence>,
    peak_resident_bytes: u64,
    peak_sampled_process_cpu_percent: f32,
    coverage: &'static str,
    missing_ranges: usize,
}

#[derive(Serialize)]
struct TimeframeEvidence {
    interval: &'static str,
    output_bars: usize,
    duration_micros: u64,
    derived_cache_hit: bool,
    duplicate_bars: u64,
    gaps: u64,
}

struct TemporaryRoot(PathBuf);

struct PersistedHistory {
    identities: Vec<SegmentIdentity>,
    payload_bytes: u64,
    stored_bytes: u64,
    duration: Duration,
}

struct WarmHistory {
    open_duration: Duration,
    discovery_duration: Duration,
    first_usable: Duration,
    read_duration: Duration,
    decode_duration: Duration,
    coverage: &'static str,
    missing_ranges: usize,
}

impl TemporaryRoot {
    fn create() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = env::temp_dir().join(format!(
            "axiusflow-market-data-performance-{}-{nonce}",
            process::id()
        ));
        fs::create_dir(&root)?;
        Ok(Self(root))
    }
}

impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let (output, bar_count) = arguments()?;
    let bars = generate_bars(bar_count)?;
    let root = TemporaryRoot::create()?;
    let scope = scope();
    let segment_key = segment_key()?;
    let maximum_entries = bar_count.div_ceil(SEGMENT_BARS).saturating_add(64);
    let mut system = System::new_all();
    let hardware = format!(
        "{}; logical_cpus={}",
        system
            .cpus()
            .first()
            .map_or("unknown-cpu", |cpu| cpu.brand()),
        system.cpus().len()
    );
    let operating_system = format!(
        "{} {}; arch={}",
        System::name().unwrap_or_else(|| env::consts::OS.to_string()),
        System::os_version().unwrap_or_else(|| "unknown-version".to_string()),
        env::consts::ARCH
    );
    let mut peak_resident_bytes = 0;
    let mut peak_cpu_percent = 0.0;
    update_process_peaks(&mut system, &mut peak_resident_bytes, &mut peak_cpu_percent)?;

    let persisted = persist_history(&root.0, &scope, &segment_key, &bars, maximum_entries)?;
    update_process_peaks(&mut system, &mut peak_resident_bytes, &mut peak_cpu_percent)?;

    let warm = read_warm_history(
        &root.0,
        &scope,
        &segment_key,
        &persisted.identities,
        maximum_entries,
        bar_count,
    )?;
    update_process_peaks(&mut system, &mut peak_resident_bytes, &mut peak_cpu_percent)?;

    let timeframe_switches = benchmark_timeframes(&bars)?;
    update_process_peaks(&mut system, &mut peak_resident_bytes, &mut peak_cpu_percent)?;
    let evidence = Evidence {
        schema_version: 2,
        hardware,
        operating_system,
        bar_count,
        segment_count: persisted.identities.len(),
        payload_bytes: persisted.payload_bytes,
        stored_bytes: persisted.stored_bytes,
        cold_publish_millis: millis(persisted.duration),
        warm_open_millis: millis(warm.open_duration),
        warm_discovery_millis: millis(warm.discovery_duration),
        time_to_first_usable_micros: micros(warm.first_usable),
        time_to_first_usable_millis: millis(warm.first_usable),
        warm_read_millis: millis(warm.read_duration),
        decode_millis: millis(warm.decode_duration),
        timeframe_switches,
        peak_resident_bytes,
        peak_sampled_process_cpu_percent: peak_cpu_percent,
        coverage: warm.coverage,
        missing_ranges: warm.missing_ranges,
    };
    validate(&evidence)?;
    write_evidence(&output, &evidence)?;
    println!(
        "market-data performance passed: bars={}, first_usable={} ms, warm_read={} ms, evidence={}",
        evidence.bar_count,
        evidence.time_to_first_usable_millis,
        evidence.warm_read_millis,
        output.display()
    );
    Ok(())
}

fn persist_history(
    root: &Path,
    scope: &HistoryScope,
    segment_key: &SegmentEncryptionKey,
    bars: &[MarketBar],
    maximum_entries: usize,
) -> Result<PersistedHistory, Box<dyn Error>> {
    let started = Instant::now();
    let mut store = HistoryStore::open(root, catalog_key()?, maximum_entries)?;
    let mut identities = Vec::with_capacity(bars.len().div_ceil(SEGMENT_BARS));
    let mut payload_bytes = 0_u64;
    let mut stored_bytes = 0_u64;
    for chunk in bars.chunks(SEGMENT_BARS) {
        let identity = identity_for_chunk(scope, chunk)?;
        let items = chunk
            .iter()
            .map(|bar| HistoryItem {
                sequence: bar.source_sequence,
                event_time_unix_nanos: bar.exchange_timestamp_seconds * NANOS_PER_SECOND,
                payload: encode_history_bar(*bar),
            })
            .collect::<Vec<_>>();
        let payload = encode_history_segment(&items)?;
        payload_bytes = payload_bytes.saturating_add(u64::try_from(payload.len())?);
        if let PublicationOutcome::Published(receipt) = store.publish(PublicationRequest {
            identity: &identity,
            payload: &payload,
            encryption_key: segment_key,
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::ProviderRefetch,
            now_unix_seconds: 1_800_000_000,
        })? {
            stored_bytes = stored_bytes.saturating_add(receipt.stored_bytes);
        }
        identities.push(identity);
    }
    Ok(PersistedHistory {
        identities,
        payload_bytes,
        stored_bytes,
        duration: started.elapsed(),
    })
}

fn read_warm_history(
    root: &Path,
    scope: &HistoryScope,
    segment_key: &SegmentEncryptionKey,
    identities: &[SegmentIdentity],
    maximum_entries: usize,
    expected_bars: usize,
) -> Result<WarmHistory, Box<dyn Error>> {
    let open_started = Instant::now();
    let mut store = HistoryStore::open(root, catalog_key()?, maximum_entries)?;
    let open_duration = open_started.elapsed();
    let requested = RetainedRange {
        start_unix_nanos: identities
            .first()
            .ok_or("benchmark generated no segments")?
            .range_start_unix_nanos,
        end_unix_nanos: identities
            .last()
            .ok_or("benchmark generated no segments")?
            .range_end_unix_nanos,
    };
    let discovery_started = Instant::now();
    let discovered = store.retained_identities_in_range(series(scope), requested, 1_800_000_001)?;
    let coverage = store
        .series_coverage_snapshot(series(scope), 1_800_000_001)?
        .plan(HistoryRange {
            start_unix_nanos: requested.start_unix_nanos,
            end_unix_nanos: requested.end_unix_nanos,
        })?;
    let discovery_duration = discovery_started.elapsed();
    let read_started = Instant::now();
    let mut first_usable = None;
    let mut decode_duration = Duration::ZERO;
    let mut decoded_bars = 0_usize;
    for identity in discovered.iter().rev() {
        let HistoryRead::Hit(payload) = store.read(
            identity,
            segment_key,
            1_800_000_001,
            RecoveryAction::ProviderRefetch,
        )?
        else {
            return Err("retained benchmark segment was unavailable".into());
        };
        let decode_started = Instant::now();
        let decoded = decode_history_segment(&payload)?;
        decode_duration = decode_duration.saturating_add(decode_started.elapsed());
        decoded_bars = decoded_bars.saturating_add(decoded.len());
        first_usable.get_or_insert_with(|| read_started.elapsed());
    }
    if decoded_bars != expected_bars {
        return Err(format!("decoded {decoded_bars} bars, expected {expected_bars}").into());
    }
    Ok(WarmHistory {
        open_duration,
        discovery_duration,
        first_usable: first_usable.ok_or("no usable data")?,
        read_duration: read_started.elapsed(),
        decode_duration,
        coverage: coverage_name(coverage.classification()),
        missing_ranges: coverage.repair_ranges().len(),
    })
}

fn benchmark_timeframes(bars: &[MarketBar]) -> Result<Vec<TimeframeEvidence>, Box<dyn Error>> {
    let mut derived = Vec::new();
    let mut evidence = Vec::new();
    for interval in [
        CoinbaseInterval::Minute5,
        CoinbaseInterval::Minute15,
        CoinbaseInterval::Hour1,
        CoinbaseInterval::Hour4,
        CoinbaseInterval::Day1,
        CoinbaseInterval::Minute5,
    ] {
        let started = Instant::now();
        let cached = derived
            .iter()
            .find(|(cached_interval, _, _)| *cached_interval == interval);
        let (output, diagnostics, derived_cache_hit) =
            if let Some((_, output, diagnostics)) = cached {
                (Arc::clone(output), *diagnostics, true)
            } else {
                let (output, diagnostics) = aggregate_coinbase_bars(bars, interval)?;
                let output = Arc::new(output);
                derived.push((interval, Arc::clone(&output), diagnostics));
                (output, diagnostics, false)
            };
        evidence.push(TimeframeEvidence {
            interval: interval.id(),
            output_bars: output.len(),
            duration_micros: micros(started.elapsed()),
            derived_cache_hit,
            duplicate_bars: diagnostics.duplicate_bars,
            gaps: diagnostics.gaps,
        });
    }
    Ok(evidence)
}

fn arguments() -> Result<(PathBuf, usize), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let output = args.next().map_or_else(
        || PathBuf::from(".cache/evidence/market_data_performance.json"),
        PathBuf::from,
    );
    let bar_count = match args.next() {
        Some(value) => value.to_string_lossy().parse::<usize>()?,
        None => DEFAULT_BAR_COUNT,
    };
    if args.next().is_some() || bar_count == 0 {
        return Err("usage: axiusflow_market_data_performance [output-json] [bar-count]".into());
    }
    Ok((output, bar_count))
}

fn generate_bars(count: usize) -> Result<Vec<MarketBar>, Box<dyn Error>> {
    (0..count)
        .map(|index| {
            let sequence = u64::try_from(index)?.saturating_add(1);
            let timestamp = i64::try_from(index)?.saturating_mul(BAR_SECONDS);
            let price = 100_000_i64.saturating_add(i64::try_from(index % 10_000)?);
            Ok(MarketBar {
                source_sequence: sequence,
                exchange_timestamp_seconds: timestamp,
                open: price,
                high: price.saturating_add(20),
                low: price.saturating_sub(20),
                close: price.saturating_add(5),
                volume: 1_000,
            })
        })
        .collect()
}

fn identity_for_chunk(
    scope: &HistoryScope,
    chunk: &[MarketBar],
) -> Result<SegmentIdentity, Box<dyn Error>> {
    let first = chunk.first().ok_or("empty benchmark segment")?;
    let last = chunk.last().ok_or("empty benchmark segment")?;
    Ok(SegmentIdentity {
        scope: scope.clone(),
        instrument_id: "coinbase:spot:BTC-USD".to_string(),
        data_kind: DataKind::Bars,
        resolution: "1m".to_string(),
        range_start_unix_nanos: first.exchange_timestamp_seconds * NANOS_PER_SECOND,
        range_end_unix_nanos: last.exchange_timestamp_seconds.saturating_add(BAR_SECONDS)
            * NANOS_PER_SECOND,
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    })
}

fn scope() -> HistoryScope {
    HistoryScope {
        provider_id: "coinbase".to_string(),
        account_id: "public".to_string(),
        entitlement_revision: "crypto_public_realtime".to_string(),
    }
}

fn series(scope: &HistoryScope) -> HistorySeriesIdentity<'_> {
    HistorySeriesIdentity {
        scope,
        instrument_id: "coinbase:spot:BTC-USD",
        data_kind: DataKind::Bars,
        resolution: "1m",
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn catalog_key() -> Result<CatalogKey, Box<dyn Error>> {
    Ok(CatalogKey::try_new(
        "benchmark-catalog".to_string(),
        [7; 32],
    )?)
}

fn segment_key() -> Result<SegmentEncryptionKey, Box<dyn Error>> {
    Ok(SegmentEncryptionKey::try_new(
        "benchmark-segments".to_string(),
        [11; 32],
    )?)
}

fn coverage_name(class: CoverageClass) -> &'static str {
    match class {
        CoverageClass::Complete => "complete",
        CoverageClass::Partial => "partial",
        CoverageClass::ConfirmedEmpty => "confirmed_empty",
        CoverageClass::Missing => "missing",
        CoverageClass::Invalidated => "invalidated",
        CoverageClass::Quarantined => "quarantined",
    }
}

fn validate(evidence: &Evidence) -> Result<(), Box<dyn Error>> {
    if evidence.coverage != "complete" || evidence.missing_ranges != 0 {
        return Err("warm-start coverage was incomplete".into());
    }
    if evidence.time_to_first_usable_millis > 2_000 || evidence.warm_read_millis > 15_000 {
        return Err("local history performance exceeded the safety budget".into());
    }
    if evidence
        .timeframe_switches
        .iter()
        .any(|item| item.duration_micros > 2_000_000 || item.gaps != 0)
    {
        return Err("timeframe aggregation exceeded its safety budget".into());
    }
    if !evidence
        .timeframe_switches
        .iter()
        .any(|item| item.derived_cache_hit)
    {
        return Err("warm derived-timeframe lookup was not exercised".into());
    }
    Ok(())
}

fn write_evidence(path: &Path, evidence: &Evidence) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(evidence)?)?;
    Ok(())
}

fn update_process_peaks(
    system: &mut System,
    peak_resident_bytes: &mut u64,
    peak_cpu_percent: &mut f32,
) -> Result<(), Box<dyn Error>> {
    let pid = Pid::from_u32(process::id());
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]));
    let process = system
        .process(pid)
        .ok_or("benchmark process metrics unavailable")?;
    *peak_resident_bytes = (*peak_resident_bytes).max(process.memory());
    *peak_cpu_percent = peak_cpu_percent.max(process.cpu_usage());
    Ok(())
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}
