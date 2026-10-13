//! Deterministic engine workload. Timing is evidence, correctness is a hard gate.
use aeris_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use aeris_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, EngineError, GenerationId, MarketEngine,
    MarketEngineConfig, ProviderCapabilities, ProviderConfig, ProviderGeneration,
    StreamRequirements, Viewport, WorkspaceId,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    hint::black_box,
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const WARM_UP: usize = 10_000;
const SAMPLES: usize = 100_000;
const GENERATION: ProviderGeneration = ProviderGeneration(NonZeroU64::MIN);
const HISTORY_BARS: usize = 50_000;
const TRADES_PER_SECOND: usize = 10_000;
const PUBLISH_INTERVAL_MS: usize = 16;

fn identity(value: usize) -> Result<NonZeroU64> {
    NonZeroU64::new(u64::try_from(value)?).ok_or_else(|| "zero identity".into())
}

fn bound(value: usize) -> Result<NonZeroUsize> {
    NonZeroUsize::new(value).ok_or_else(|| "zero capacity".into())
}

fn bar(sequence: u64, revision: usize) -> Result<MarketBar> {
    let seconds = 1_700_000_000 + i64::try_from(sequence)? * 60;
    Ok(MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: seconds,
        exchange_timestamp_unix_nanos: seconds * 1_000_000_000,
        open: 100,
        high: 110,
        low: 90,
        close: 100 + i64::try_from(revision % 10)?,
        volume: 7,
    })
}

fn configured_engine(
    consumers: usize,
    series_count: usize,
) -> Result<(MarketEngine, Vec<BarSeriesKey>)> {
    let maximum_bars = series_count * 4096;
    let mut engine = MarketEngine::new(MarketEngineConfig {
        maximum_consumers: bound(consumers)?,
        maximum_series: bound(series_count)?,
        maximum_bars: bound(maximum_bars)?,
    });
    engine.register_provider(
        "synthetic".into(),
        ProviderConfig {
            account_id: "synthetic:public".into(),
            capabilities: ProviderCapabilities {
                historical_bars: true,
                realtime_bars: true,
                streams: StreamRequirements::BARS,
            },
            reconnect_delay: Duration::from_millis(250),
        },
    )?;
    engine.begin_provider_session("synthetic", GENERATION)?;
    let keys: Vec<_> = (0..series_count)
        .map(|i| BarSeriesKey {
            provider_id: "synthetic".into(),
            instrument_id: format!("synthetic:{i}"),
            entitlement_id: "public".into(),
            period: BarPeriod::Time { seconds: 60 },
            definition_version: 1,
        })
        .collect();
    for i in 0..consumers {
        engine.register_consumer(
            ConsumerIdentity {
                client_id: ClientId(NonZeroU64::MIN),
                workspace_id: WorkspaceId(NonZeroU64::MIN),
                consumer_id: ConsumerId(identity(i + 1)?),
            },
            true,
        )?;
        engine.set_series_demand(
            ConsumerId(identity(i + 1)?),
            GenerationId(NonZeroU64::MIN),
            &keys[i % series_count],
        )?;
    }
    for key in &keys {
        engine.install_realtime(GENERATION, key, 2, 0, vec![bar(1, 0)?], true)?;
    }
    assert_eq!(engine.metrics().active_subscriptions, series_count);
    assert!(matches!(
        engine.register_consumer(
            ConsumerIdentity {
                client_id: ClientId(NonZeroU64::MIN),
                workspace_id: WorkspaceId(NonZeroU64::MIN),
                consumer_id: ConsumerId(identity(consumers + 1)?),
            },
            true
        ),
        Err(EngineError::ConsumerLimitExceeded { .. })
    ));

    Ok((engine, keys))
}

fn profile(consumers: usize, series_count: usize) -> Result<Value> {
    let maximum_bars = series_count * 4096;
    let (mut engine, keys) = configured_engine(consumers, series_count)?;
    let mut revisions = vec![0_usize; series_count];
    let mut generations = vec![1_usize; consumers];
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut measured_start = Instant::now();
    for operation in 0..WARM_UP + SAMPLES {
        if operation == WARM_UP {
            measured_start = Instant::now();
        }
        let started = Instant::now();
        let index = operation % series_count;
        revisions[index] += 1;
        let sequence = 1 + u64::try_from(revisions[index] / 100)?;
        black_box(engine.install_realtime_tail(
            GENERATION,
            &keys[index],
            2,
            0,
            bar(sequence, operation)?,
            true,
        )?);
        if operation % 100 == 0 {
            let consumer = (operation / 100) % consumers;
            generations[consumer] += 1;
            let id = ConsumerId(identity(consumer + 1)?);
            let generation = GenerationId(identity(generations[consumer])?);
            black_box(engine.set_series_demand(
                id,
                generation,
                &keys[(consumer + generations[consumer]) % series_count],
            )?);
            engine.set_viewport(
                id,
                generation,
                Viewport::try_new(1_700_000_000_000_000_000, 1_800_000_000_000_000_000)?,
            )?;
            assert!(matches!(
                engine.set_viewport(id, GenerationId(NonZeroU64::MIN), Viewport::try_new(1, 2)?),
                Err(EngineError::StaleConsumerGeneration { .. })
            ));
        }
        if operation >= WARM_UP {
            samples.push(u64::try_from(started.elapsed().as_nanos())?);
        }
    }
    let elapsed = measured_start.elapsed();
    samples.sort_unstable();
    let metrics = engine.metrics();
    assert!(metrics.stored_bars <= maximum_bars);
    assert!(metrics.stored_series <= series_count);
    assert!(metrics.active_subscriptions <= series_count);
    assert!(
        engine
            .install_realtime_tail(
                ProviderGeneration(identity(2)?),
                &keys[0],
                2,
                0,
                bar(1, 0)?,
                true
            )
            .is_err()
    );
    assert_eq!(
        engine.detach_client(ClientId(NonZeroU64::MIN)).len(),
        consumers
    );
    assert_eq!(engine.metrics().active_subscriptions, 0);
    assert_eq!(engine.metrics().active_consumers, 0);
    Ok(json!({
        "consumers": consumers, "series": series_count, "warm_up_operations": WARM_UP,
        "measured_operations": SAMPLES, "elapsed_nanos": u64::try_from(elapsed.as_nanos())?,
        "operations_per_second": f64::from(u32::try_from(SAMPLES)?) / elapsed.as_secs_f64(),
        "p50_nanos": samples[SAMPLES / 2], "p95_nanos": samples[SAMPLES * 95 / 100],
        "p99_nanos": samples[SAMPLES * 99 / 100], "stored_bars": metrics.stored_bars,
        "stored_series": metrics.stored_series, "approximate_series_bytes": metrics.approximate_series_bytes,
        "correctness_passed": true,
    }))
}

/// Capacity-based lower bound: Arc's contiguous bar allocation contains at
/// least this many bytes. This excludes its header and allocator overhead.
fn bar_payload_bytes(snapshot: &aeris_market_engine::SeriesSnapshot) -> Result<usize> {
    snapshot
        .bars
        .len()
        .checked_mul(size_of::<MarketBar>())
        .ok_or_else(|| "bar allocation size overflowed".into())
}

fn snapshot_publish_profile() -> Result<Value> {
    let (mut engine, key) = large_history_engine()?;
    let history: Vec<_> = (1..=HISTORY_BARS)
        .map(|sequence| bar(u64::try_from(sequence)?, 0))
        .collect::<Result<_>>()?;
    engine.install_history(GENERATION, &key, 2, 0, history)?;
    let mut total_publish_nanos = 0_u128;
    let mut total_payload_bytes = 0_usize;
    let mut publishes = 0_usize;
    let started = Instant::now();
    for trade in 0..TRADES_PER_SECOND {
        let tail = bar(u64::try_from(HISTORY_BARS + 1)?, trade)?;
        black_box(engine.install_realtime_tail(GENERATION, &key, 2, 0, tail, true)?);
        if trade % (TRADES_PER_SECOND * PUBLISH_INTERVAL_MS / 1_000) == 0 {
            let publish_start = Instant::now();
            let snapshot = engine
                .series_snapshot(&key)
                .ok_or("published series missing")?;
            let payload_bytes = bar_payload_bytes(&snapshot)?;
            black_box(snapshot);
            total_publish_nanos += publish_start.elapsed().as_nanos();
            total_payload_bytes += payload_bytes;
            publishes += 1;
        }
    }
    let elapsed = started.elapsed();
    if publishes == 0 {
        return Err("snapshot workload produced no publications".into());
    }
    Ok(json!({
        "completed_bars": HISTORY_BARS,
        "trades_per_second_input": TRADES_PER_SECOND,
        "publish_interval_millis": PUBLISH_INTERVAL_MS,
        "trades_processed": TRADES_PER_SECOND,
        "publish_count": publishes,
        "elapsed_nanos": u64::try_from(elapsed.as_nanos())?,
        "nanos_per_trade": elapsed.as_nanos() / TRADES_PER_SECOND as u128,
        "nanos_per_publish": total_publish_nanos / publishes as u128,
        "bar_payload_bytes_per_publish_lower_bound": total_payload_bytes / publishes,
        "allocation_measurement": "Arc<[MarketBar]> payload capacity; excludes Arc header and allocator overhead",
    }))
}

fn large_history_engine() -> Result<(MarketEngine, BarSeriesKey)> {
    let mut engine = MarketEngine::new(MarketEngineConfig {
        maximum_consumers: bound(1)?,
        maximum_series: bound(1)?,
        maximum_bars: bound(HISTORY_BARS + 2)?,
    });
    engine.register_provider(
        "synthetic".into(),
        ProviderConfig {
            account_id: "synthetic:public".into(),
            capabilities: ProviderCapabilities {
                historical_bars: true,
                realtime_bars: true,
                streams: StreamRequirements::BARS,
            },
            reconnect_delay: Duration::from_millis(250),
        },
    )?;
    engine.begin_provider_session("synthetic", GENERATION)?;
    engine.register_consumer(
        ConsumerIdentity {
            client_id: ClientId(NonZeroU64::MIN),
            workspace_id: WorkspaceId(NonZeroU64::MIN),
            consumer_id: ConsumerId(NonZeroU64::MIN),
        },
        true,
    )?;
    let key = BarSeriesKey {
        provider_id: "synthetic".into(),
        instrument_id: "synthetic:0".into(),
        entitlement_id: "public".into(),
        period: BarPeriod::Time { seconds: 60 },
        definition_version: 1,
    };
    engine.set_series_demand(
        ConsumerId(NonZeroU64::MIN),
        GenerationId(NonZeroU64::MIN),
        &key,
    )?;
    Ok((engine, key))
}

fn study_copy_profile() -> Result<Value> {
    let (mut engine, key) = large_history_engine()?;
    // A full study input copy on each tick: the next milestone replaces this
    // with a segmented view. Bound the sample to avoid excessive allocations.
    let history: Vec<_> = (1..=HISTORY_BARS)
        .map(|sequence| bar(u64::try_from(sequence)?, 0))
        .collect::<Result<_>>()?;
    engine.install_history(GENERATION, &key, 2, 0, history)?;
    let iterations = 256_usize;
    let started = Instant::now();
    for _ in 0..iterations {
        let snapshot = engine.series_snapshot(&key).ok_or("study series missing")?;
        black_box(snapshot.bars.to_vec());
    }
    let elapsed = started.elapsed();
    let snapshot = engine.series_snapshot(&key).ok_or("study series missing")?;
    Ok(json!({
        "completed_bars": HISTORY_BARS,
        "ticks_sampled": iterations,
        "nanos_per_tick_copy": elapsed.as_nanos() / iterations as u128,
        "bar_payload_bytes_per_tick_copy_lower_bound": bar_payload_bytes(&snapshot)?,
    }))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err("could not read source provenance".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn sha256(bytes: impl AsRef<[u8]>) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for &byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 15)]));
    }
    output
}

fn source_hash(root: &Path) -> Result<String> {
    // Include new modules too: git diff alone omits untracked source during refactoring.
    let paths = git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "apps",
            "crates",
            "Cargo.toml",
            "Cargo.lock",
        ],
    )?;
    let mut paths: Vec<_> = paths.split('\0').filter(|path| !path.is_empty()).collect();
    paths.sort_unstable();
    paths.dedup();
    let mut digest = Sha256::new();
    for path in paths {
        if ![".rs", ".toml", ".lock", ".css"]
            .iter()
            .any(|extension| path.ends_with(extension))
        {
            continue;
        }
        let file = root.join(path);
        if !file.is_file() {
            continue;
        }
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(std::fs::read(file)?);
        digest.update([0]);
    }
    Ok(hex(&digest.finalize()))
}

fn main() -> Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .ok_or("usage: market_data_performance <new-report.json>")?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report = json!({
        "schema_version": 1, "workload": "engine_tail_rollover_demand_v1",
        "source_revision": git(&root, &["rev-parse", "HEAD"])?,
        "worktree_dirty": !git(&root, &["status", "--porcelain"])?.is_empty(),
        "source_diff_sha256": sha256(git(&root, &["diff", "HEAD", "--", "apps", "crates", "Cargo.toml", "Cargo.lock"])?),
        "source_tree_sha256": source_hash(&root)?,
        "executable_sha256": sha256(std::fs::read(std::env::current_exe()?)?),
        "processor": std::env::var("PROCESSOR_IDENTIFIER").ok(),
        "lockfile_sha256": sha256(std::fs::read(root.join("Cargo.lock"))?),
        "operating_system": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "logical_processors": std::thread::available_parallelism()?.get(),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "profiles": [profile(1, 1)?, profile(16, 8)?, profile(256, 128)?],
        "snapshot_per_publish": snapshot_publish_profile()?,
        "study_per_tick_copies": study_copy_profile()?,
    });
    let path = Path::new(&output);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(file, &report)?;
    println!("{}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{HISTORY_BARS, Result, snapshot_publish_profile, study_copy_profile};

    #[test]
    fn snapshot_workload_copies_fifty_thousand_bars_at_publish_cadence() -> Result<()> {
        let profile = snapshot_publish_profile()?;
        assert_eq!(profile["completed_bars"], HISTORY_BARS);
        assert_eq!(profile["trades_processed"], 10_000);
        assert_eq!(profile["publish_count"], 63);
        assert!(profile["bar_payload_bytes_per_publish_lower_bound"].as_u64() > Some(3_000_000));
        Ok(())
    }

    #[test]
    fn study_workload_materializes_a_full_copy_each_tick() -> Result<()> {
        let profile = study_copy_profile()?;
        assert_eq!(profile["ticks_sampled"], 256);
        assert!(profile["bar_payload_bytes_per_tick_copy_lower_bound"].as_u64() > Some(3_000_000));
        Ok(())
    }
}
