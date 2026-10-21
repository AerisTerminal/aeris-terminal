//! Shared probe-envelope helpers for the non-shipping live validation binaries.

use std::{
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use serde_json::{Value, json};

pub mod feed;

/// One probe failure class: caller misuse, a runtime service error, local I/O, or a
/// measured invariant that did not hold.
#[derive(Debug)]
pub enum ProbeError {
    Argument(&'static str),
    Runtime(String),
    Io(std::io::Error),
    Invariant(String),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument(message) => formatter.write_str(message),
            Self::Invariant(message) | Self::Runtime(message) => formatter.write_str(message),
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

pub type ProbeResult<T> = Result<T, ProbeError>;

#[must_use]
pub fn utc_now() -> chrono::DateTime<Utc> {
    SystemTime::now().into()
}

/// Wall-clock Unix nanoseconds for probe-side timestamps; clamps instead of panicking.
#[must_use]
pub fn unix_nanos_now() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX)
}

/// Maps a runtime service error into the probe failure type.
///
/// # Errors
/// Always returns the mapped service error.
pub fn market<T>(result: Result<T, String>) -> ProbeResult<T> {
    result.map_err(ProbeError::Runtime)
}

/// Polls one condition inside a bounded deadline, sleeping between attempts.
///
/// # Errors
/// Propagates the poller's error, or an invariant failure once the deadline passes.
pub fn wait_for<T>(
    what: &'static str,
    timeout: Duration,
    mut poll: impl FnMut() -> ProbeResult<Option<T>>,
) -> ProbeResult<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = poll()? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(ProbeError::Invariant(format!(
                "timed out waiting for {what}"
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Ordered probe checks plus free-form run metrics, serialized into the envelope.
pub struct Evidence {
    pub checks: Vec<Value>,
    pub metrics: Value,
}
impl Default for Evidence {
    fn default() -> Self {
        Self::new()
    }
}

impl Evidence {
    #[must_use]
    pub fn new() -> Self {
        Self {
            checks: Vec::new(),
            metrics: json!({}),
        }
    }

    /// Records one measured check; a failed check aborts the scenario with its identity.
    ///
    /// # Errors
    /// Returns the check identity as an invariant failure when the check did not pass.
    pub fn check(
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
            Err(ProbeError::Invariant(id.to_string()))
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
        return Err(ProbeError::Invariant(
            "git revision could not be verified".to_string(),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|_| ProbeError::Invariant("git output was not UTF-8".to_string()))
}

/// Rejects unsafe roots and outputs before any evidence collection or service start.
///
/// # Errors
/// Returns a static reason when a path is unsafe or outside the validation folder.
pub fn validate_paths(root: &Path, out: &Path) -> Result<(), &'static str> {
    if !root.is_absolute()
        || !out.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || out
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("probe paths must be absolute without parent traversal");
    }
    let allowed = std::env::temp_dir().join("aeris_validation");
    if !root.starts_with(&allowed)
        || out.parent() != Some(root)
        || out == root
        || out.extension().is_none_or(|extension| extension != "json")
    {
        return Err("probe root and JSON output must be inside the isolated validation folder");
    }
    // Windows `canonicalize` returns verbatim paths; both sides must share the
    // representation before the prefix check. `root` lives under `allowed`, so the
    // canonicalized parent exists whenever the canonicalized root does.
    if let Ok(canonical) = std::fs::canonicalize(root) {
        let allowed_canonical = std::fs::canonicalize(&allowed).unwrap_or_else(|_| allowed.clone());
        if !canonical.starts_with(&allowed_canonical) {
            return Err("probe root resolves outside the isolated validation folder");
        }
    }
    Ok(())
}

/// The `aeris.probe.v1` envelope identity and provenance of one probe run.
pub struct Envelope {
    probe: &'static str,
    root: PathBuf,
    out: PathBuf,
    gate_source_clean: bool,
    git_revision: String,
    source_clean: bool,
}

impl Envelope {
    /// Captures repository provenance before the scenario starts; the worktree-clean
    /// check gates the outcome (the established behavior for shipped-path probes).
    ///
    /// # Errors
    /// Returns an error when the git revision or worktree state cannot be verified.
    pub fn collect(probe: &'static str, root: PathBuf, out: PathBuf) -> ProbeResult<Self> {
        Self::collect_with_policy(probe, root, out, true)
    }

    /// Captures repository provenance with an explicit cleanliness policy. Probes under
    /// development, whose own sources are not committed yet, record the worktree state
    /// as evidence without gating the outcome on it.
    ///
    /// # Errors
    /// Returns an error when the git revision or worktree state cannot be verified.
    pub fn collect_with_policy(
        probe: &'static str,
        root: PathBuf,
        out: PathBuf,
        gate_source_clean: bool,
    ) -> ProbeResult<Self> {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| ProbeError::Invariant("repository root missing".to_string()))?;
        let git_revision = git(repo, &["rev-parse", "HEAD"])?;
        let source_clean =
            git(repo, &["status", "--porcelain", "--untracked-files=normal"])?.is_empty();
        Ok(Self {
            probe,
            root,
            out,
            gate_source_clean,
            git_revision,
            source_clean,
        })
    }

    #[must_use]
    pub fn git_revision(&self) -> &str {
        &self.git_revision
    }

    #[must_use]
    pub const fn source_clean(&self) -> bool {
        self.source_clean
    }

    /// Appends the closing checks, writes the envelope durably, and reports pass/fail.
    ///
    /// # Errors
    /// Returns an error when the envelope cannot be serialized or durably written.
    pub fn finish(
        &self,
        started_at: &str,
        evidence: &mut Evidence,
        result: &ProbeResult<()>,
    ) -> ProbeResult<bool> {
        if self.gate_source_clean {
            let _ = evidence.check(
                "source_clean",
                self.source_clean,
                json!({"clean": self.source_clean}),
            );
        }
        if let Err(error) = &result {
            evidence
                .checks
                .push(json!({"id":"probe_execution", "outcome":"failed",
            "detail":error.to_string(), "measured":{}}));
        }
        let passed = result.is_ok()
            && (!self.gate_source_clean || self.source_clean)
            && evidence
                .checks
                .iter()
                .all(|check| check["outcome"] == "passed");
        let payload = json!({
            "schema":"aeris.probe.v1",
            "probe":self.probe,
            "git_revision":self.git_revision,
            "source_clean":self.source_clean,
            "started_at":started_at,
            "finished_at":utc_now().to_rfc3339(),
            "validation_root":self.root,
            "vault_namespace":"validation",
            "outcome":if passed { "passed" } else { "failed" },
            "checks":evidence.checks,
            "metrics":evidence.metrics,
        });
        let bytes = serde_json::to_vec_pretty(&payload)
            .map_err(|error| ProbeError::Runtime(error.to_string()))?;
        aeris_platform_runtime::fs::write_file_durably(&self.out, &bytes)?;
        Ok(passed)
    }
}

/// Exact fixed-point price shaping shared by the trading probes.
pub mod prices {
    use aeris_trading::FixedPoint;

    /// Multiplies a positive price by `numerator / denominator` in i128, truncating
    /// toward zero; the scale is preserved.
    #[must_use]
    pub fn scale_fraction(
        price: FixedPoint,
        numerator: u32,
        denominator: u32,
    ) -> Option<FixedPoint> {
        if denominator == 0 {
            return None;
        }
        let units = i128::from(price.units()) * i128::from(numerator) / i128::from(denominator);
        FixedPoint::try_new(i64::try_from(units).ok()?, price.scale()).ok()
    }

    /// Snaps a positive price to `figures` significant decimal figures, rounding the
    /// retained digits down or up. The result stays safely inside any provider
    /// significant-figure grid with at least `figures` figures.
    #[must_use]
    pub fn snap_significant(price: FixedPoint, figures: u32, round_up: bool) -> Option<FixedPoint> {
        let units = price.units();
        if figures == 0 || units <= 0 {
            return None;
        }
        let digits = units.checked_ilog10()? + 1;
        let quantum = 10_i64.checked_pow(digits.saturating_sub(figures))?;
        let remainder = units % quantum;
        let snapped = if round_up && remainder != 0 {
            units.checked_add(quantum - remainder)?
        } else {
            units - remainder
        };
        if snapped <= 0 {
            return None;
        }
        FixedPoint::try_new(snapped, price.scale()).ok()
    }

    /// The next UTC midnight boundary, matching the trading owner's DAY expiry rule for
    /// always-open sessions.
    #[must_use]
    pub fn next_utc_midnight(now_unix_nanos: i64) -> Option<i64> {
        const DAY_NANOS: i64 = 86_400_000_000_000;
        now_unix_nanos
            .checked_div_euclid(DAY_NANOS)?
            .checked_add(1)?
            .checked_mul(DAY_NANOS)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn price(units: i64) -> FixedPoint {
            FixedPoint::try_new(units, 8).expect("price")
        }

        #[test]
        fn snap_rounds_to_three_significant_figures() {
            assert_eq!(
                snap_significant(price(9_765_430_000_000), 3, false),
                Some(price(9_760_000_000_000))
            );
            assert_eq!(
                snap_significant(price(9_765_430_000_000), 3, true),
                Some(price(9_770_000_000_000))
            );
            assert_eq!(
                snap_significant(price(9_277_158_500_000), 3, false),
                Some(price(9_270_000_000_000))
            );
            assert_eq!(
                snap_significant(price(10_000_000_000_000), 3, true),
                Some(price(10_000_000_000_000))
            );
            assert_eq!(
                snap_significant(price(5_000_000), 3, false),
                Some(price(5_000_000))
            );
            assert_eq!(snap_significant(price(0), 3, false), None);
            assert_eq!(snap_significant(price(100), 0, false), None);
        }

        #[test]
        fn scale_fraction_preserves_scale_and_truncates() {
            assert_eq!(
                scale_fraction(price(9_765_430_000_000), 95, 100),
                Some(price(9_277_158_500_000))
            );
            assert_eq!(scale_fraction(price(101), 1, 2), Some(price(50)));
            assert_eq!(scale_fraction(price(100), 1, 0), None);
        }

        #[test]
        fn next_utc_midnight_matches_owner_rule() {
            let midnight = 1_760_313_600_000_000_000_i64; // 2025-10-13T00:00:00Z
            assert_eq!(next_utc_midnight(midnight - 1_000_000_000), Some(midnight));
            assert_eq!(
                next_utc_midnight(midnight),
                Some(midnight + 86_400_000_000_000)
            );
        }
    }
}
