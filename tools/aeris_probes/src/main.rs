//! Non-shipping, public-feed validation entry point.

mod cli;
mod live;

use std::{
    error::Error,
    fmt,
    path::Path,
    process::Command,
    time::{Instant, SystemTime},
};

use chrono::Utc;
use serde_json::{Value, json};

#[derive(Debug)]
enum ProbeError {
    Argument(&'static str),
    Runtime(String),
    Io(std::io::Error),
    Invariant(&'static str),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument(message) | Self::Invariant(message) => formatter.write_str(message),
            Self::Runtime(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}
impl Error for ProbeError {}
impl From<std::io::Error> for ProbeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

type ProbeResult<T> = Result<T, ProbeError>;
fn utc_now() -> chrono::DateTime<Utc> {
    SystemTime::now().into()
}
fn market<T>(result: Result<T, String>) -> ProbeResult<T> {
    result.map_err(ProbeError::Runtime)
}

struct Evidence {
    checks: Vec<Value>,
    metrics: Value,
}
impl Evidence {
    fn new() -> Self {
        Self {
            checks: Vec::new(),
            metrics: json!({}),
        }
    }

    fn check(
        &mut self,
        id: &'static str,
        passed: bool,
        measured: impl Into<Value>,
    ) -> ProbeResult<()> {
        self.checks.push(json!({
            "id": id,
            "outcome": if passed { "passed" } else { "failed" },
            "detail": if passed { "measured live runtime behavior" } else { "invariant not met" },
            "measured": measured.into(),
        }));
        if passed {
            Ok(())
        } else {
            Err(ProbeError::Invariant(id))
        }
    }
}

fn git(root: &Path, arguments: &[&str]) -> ProbeResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(ProbeError::Invariant("git revision could not be verified"));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|_| ProbeError::Invariant("git output was not UTF-8"))
}

fn run(
    root: &aeris_platform_runtime::data_root::DataRoot,
    options: &cli::Options,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    use aeris_platform_runtime::data_root::InstanceLock;
    let _lock =
        InstanceLock::acquire(root).map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let service = market(aeris_market_runtime::MarketService::start())?;
    let result = live::run(&service, options, evidence);
    let started = Instant::now();
    let shutdown = service.shutdown(std::time::Duration::from_secs(2));
    let elapsed = started.elapsed().as_millis();
    let shutdown_check = evidence.check(
        "shutdown_latency_ms",
        shutdown.is_ok() && elapsed < 2_000,
        json!({"elapsed_ms": elapsed}),
    );
    result?;
    market(shutdown)?;
    shutdown_check
}

fn main() -> Result<(), Box<dyn Error>> {
    use aeris_platform_runtime::data_root::{DataRootMode, initialize_data_root};

    let options = cli::Options::parse(std::env::args().skip(1)).map_err(ProbeError::Argument)?;
    // Reject unsafe roots before collecting evidence or attempting to write output.
    let root = initialize_data_root(DataRootMode::Validation {
        root: options.root.clone(),
        borrow_stored_ctrader: false,
    })
    .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or(ProbeError::Invariant("repository root missing"))?;
    let revision = git(repo, &["rev-parse", "HEAD"])?;
    let clean = git(repo, &["status", "--porcelain", "--untracked-files=normal"])?.is_empty();
    let started_at = utc_now().to_rfc3339();
    let mut evidence = Evidence::new();
    let result = run(root, &options, &mut evidence);
    let _ = evidence.check("source_clean", clean, json!({"clean": clean}));
    if let Err(error) = &result {
        evidence
            .checks
            .push(json!({"id":"probe_execution", "outcome":"failed",
            "detail":error.to_string(), "measured":{}}));
    }
    let outcome = if result.is_ok()
        && clean
        && evidence
            .checks
            .iter()
            .all(|check| check["outcome"] == "passed")
    {
        "passed"
    } else {
        "failed"
    };
    let payload = json!({
        "schema":"aeris.probe.v1",
        "probe":"market_lifecycle",
        "git_revision":revision,
        "source_clean":clean,
        "started_at":started_at,
        "finished_at":utc_now().to_rfc3339(),
        "validation_root":options.root,
        "vault_namespace":"validation",
        "borrowed_stored_ctrader":false,
        "outcome":outcome,
        "checks":evidence.checks,
        "metrics":evidence.metrics,
    });
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    aeris_platform_runtime::fs::write_file_durably(&options.out, &bytes)?;
    if outcome != "passed" {
        return Err(Box::new(ProbeError::Invariant(
            "market lifecycle probe failed; see JSON evidence",
        )));
    }
    println!("Market lifecycle probe passed: {}", options.out.display());
    Ok(())
}
