//! Comparative desktop transactional-store evidence for `S2-22`.

mod redb_store;
mod report;
mod sqlite;

use report::{BackendEvidence, EmbeddedStoreReport, LatencySummary};
use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const RUNS: usize = 5;
const WORKSPACE_UPDATES: usize = 64;
const CATALOG_ENTRIES: usize = 4_096;
const ORDER_INTENTS: usize = 128;
const SQLITE_VERSION: &str = "3.53.2";
const RUSQLITE_VERSION: &str = "0.40.1";
const REDB_VERSION: &str = "4.1.0";

pub(super) struct RawRun {
    pub workspace_nanos: u64,
    pub catalog_nanos: u64,
    pub order_commit_nanos: Vec<u64>,
    pub reopen_nanos: u64,
    pub file_bytes: u64,
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn create(root: &Path) -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = root.join(format!(
            "axiusflow-embedded-store-spike-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    if cfg!(debug_assertions) {
        return Err("embedded-store performance evidence requires a release build".into());
    }
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for embedded-store evidence")?;
    let benchmark_root = report_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(benchmark_root)?;
    let benchmark_root = fs::canonicalize(benchmark_root)?;
    let benchmark_filesystem = filesystem_type(&benchmark_root)?;
    let temporary = TemporaryDirectory::create(&benchmark_root)?;

    let sqlite = exercise_backend(
        "sqlite_wal",
        &temporary.path,
        "sqlite",
        sqlite::run_once,
        sqlite::verify_after_crash,
        sqlite::verify_corruption_detection,
        sqlite::verify_disk_full,
    )?;
    let redb = exercise_backend(
        "redb",
        &temporary.path,
        "redb",
        redb_store::run_once,
        redb_store::verify_after_crash,
        redb_store::verify_corruption_detection,
        redb_store::verify_disk_full,
    )?;

    let report = EmbeddedStoreReport {
        schema_version: 2,
        evidence_scope: "stage_2_embedded_store_spike",
        source_revision,
        benchmark_root: benchmark_root.display().to_string(),
        benchmark_filesystem,
        target_os: env::consts::OS,
        target_arch: env::consts::ARCH,
        build_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        runs: RUNS,
        workloads: report::WorkloadShape {
            workspace_updates: WORKSPACE_UPDATES,
            catalog_entries: CATALOG_ENTRIES,
            order_intents: ORDER_INTENTS,
        },
        sqlite_version: rusqlite::version().to_string(),
        expected_sqlite_version: SQLITE_VERSION,
        rusqlite_version: RUSQLITE_VERSION,
        redb_version: REDB_VERSION,
        sqlite,
        redb,
        selection: "sqlite_wal",
        selection_scope: "desktop workspace/catalog and local OMS metadata only",
        history_payload_decision: "separate immutable checksummed segment format; not database rows",
        limitations: [
            "abrupt-process-exit crash simulation is not a power-cut or torn-write harness",
            "disk-full behavior uses deterministic backend/page limits rather than a physically full device",
            "performance results qualify only this host, filesystem, build profile, and workload",
            "Windows and macOS packaging and destructive crash matrices remain release gates",
        ],
    };
    if report.sqlite_version != report.expected_sqlite_version {
        return Err(format!(
            "bundled SQLite version drifted: expected {}, observed {}",
            report.expected_sqlite_version, report.sqlite_version
        )
        .into());
    }
    report::write(report_path, &report)?;
    println!(
        "embedded_store_spike=passed selection={} sqlite_order_p99_ns={} redb_order_p99_ns={} report={}",
        report.selection,
        report.sqlite.order_commit_latency.p99_nanos,
        report.redb.order_commit_latency.p99_nanos,
        report_path.display()
    );
    Ok(())
}

#[cfg(unix)]
fn filesystem_type(path: &Path) -> Result<String, Box<dyn Error>> {
    let arguments: &[&str] = if cfg!(target_os = "macos") {
        &["-f", "%T"]
    } else {
        &["-f", "-c", "%T"]
    };
    let output = Command::new("stat").args(arguments).arg(path).output()?;
    if !output.status.success() {
        return Err(format!("stat failed for benchmark root {}", path.display()).into());
    }
    let filesystem = String::from_utf8(output.stdout)?.trim().to_string();
    if filesystem.is_empty() {
        return Err("stat returned an empty benchmark filesystem".into());
    }
    Ok(filesystem)
}

#[cfg(not(unix))]
fn filesystem_type(_path: &Path) -> Result<String, Box<dyn Error>> {
    Ok("unreported".to_string())
}

fn exercise_backend(
    evidence_name: &'static str,
    root: &Path,
    child_backend: &str,
    run_once: fn(&Path) -> Result<RawRun, Box<dyn Error>>,
    verify_after_crash: fn(&Path) -> Result<(), Box<dyn Error>>,
    verify_corruption: fn(&Path) -> Result<&'static str, Box<dyn Error>>,
    verify_disk_full: fn(&Path) -> Result<(), Box<dyn Error>>,
) -> Result<BackendEvidence, Box<dyn Error>> {
    let backend_root = root.join(evidence_name);
    fs::create_dir(&backend_root)?;
    let mut runs = Vec::with_capacity(RUNS);
    for index in 0..RUNS {
        let path = backend_root.join(format!("workload-{index}.db"));
        runs.push(run_once(&path)?);
    }

    let crash_path = backend_root.join("crash.db");
    run_crash_process(child_backend, &crash_path)?;
    verify_after_crash(&crash_path)?;
    let corruption_detection = verify_corruption(&backend_root.join("corruption.db"))?;
    verify_disk_full(&backend_root.join("disk-full.db"))?;

    let workspace = runs.iter().map(|run| run.workspace_nanos).collect();
    let catalog = runs.iter().map(|run| run.catalog_nanos).collect();
    let reopen = runs.iter().map(|run| run.reopen_nanos).collect();
    let file_sizes = LatencySummary::from_samples(runs.iter().map(|run| run.file_bytes).collect())?;
    let order_commits = runs
        .iter()
        .flat_map(|run| run.order_commit_nanos.iter().copied())
        .collect();
    Ok(BackendEvidence {
        backend: evidence_name,
        workspace_total_latency: LatencySummary::from_samples(workspace)?,
        catalog_total_latency: LatencySummary::from_samples(catalog)?,
        order_commit_latency: LatencySummary::from_samples(order_commits)?,
        reopen_latency: LatencySummary::from_samples(reopen)?,
        file_bytes_p50: file_sizes.p50_nanos,
        file_bytes_maximum: file_sizes.maximum_nanos,
        migration: "passed",
        committed_reopen: "passed",
        uncommitted_rollback: "passed",
        abrupt_process_recovery: "passed",
        corruption_detection,
        disk_full_failure: "passed",
    })
}

fn run_crash_process(backend: &str, path: &Path) -> Result<(), Box<dyn Error>> {
    let status = Command::new(env::current_exe()?)
        .arg("--embedded-store-crash-child")
        .arg(backend)
        .arg(path)
        .status()?;
    if status.code() != Some(91) {
        return Err(format!("{backend} crash child exited with {status}").into());
    }
    Ok(())
}

pub fn run_crash_child(backend: &str, path: &Path) -> Result<(), Box<dyn Error>> {
    match backend {
        "sqlite" => sqlite::crash_child(path),
        "redb" => redb_store::crash_child(path),
        _ => Err(format!("unknown embedded-store backend: {backend}").into()),
    }
}

fn deterministic_payload(seed: usize, bytes: usize) -> Vec<u8> {
    let mut state = u64::try_from(seed).unwrap_or(u64::MAX) ^ 0x9e37_79b9_7f4a_7c15;
    let mut payload = Vec::with_capacity(bytes);
    for _ in 0..bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        payload.push(state.to_le_bytes()[0]);
    }
    payload
}
