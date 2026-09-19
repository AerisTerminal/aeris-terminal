//! Deterministic engine workload. Timing is evidence, correctness is a hard gate.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    hint::black_box,
    num::{NonZeroU64, NonZeroUsize},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
use tradingplot_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use tradingplot_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, EngineError, GenerationId, MarketEngine,
    MarketEngineConfig, ProviderCapabilities, ProviderConfig, ProviderGeneration,
    StreamRequirements, Viewport, WorkspaceId,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const WARM_UP: usize = 10_000;
const SAMPLES: usize = 100_000;
const GENERATION: ProviderGeneration = ProviderGeneration(NonZeroU64::MIN);

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
