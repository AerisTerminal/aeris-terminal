//! Candidate-bound evidence emitted by credentialed/public live market gates.
//!
//! The desktop readiness report consumes this schema verbatim. A live gate
//! removes any previous completed report before it starts, copies the exact
//! gate executable beside the report, and records source/binary provenance.
//! Interrupted runs therefore cannot reuse a stale pass.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub const LIVE_MARKET_GATE_SCHEMA_VERSION: u32 = 1;
pub const LIVE_MARKET_GATE_EVIDENCE_SCOPE: &str = "engine_live_market_gate";
pub const LIVE_MARKET_GATE_MAXIMUM_DETAIL_BYTES: usize = 4 * 1_024;
pub const LIVE_MARKET_GATE_MAXIMUM_REPORT_BYTES: u64 = 16 * 1_024;
pub const LIVE_MARKET_GATE_MAXIMUM_BINARY_BYTES: u64 = 512 * 1_024 * 1_024;
const INCOMPLETE_DETAIL: &str = "live gate started and has not completed";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveMarketGateOutcome {
    Passed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveMarketGateCompletion {
    Incomplete,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LiveMarketGateEvidence {
    pub schema_version: u32,
    pub evidence_scope: String,
    pub provider: String,
    pub outcome: LiveMarketGateOutcome,
    pub completion_state: LiveMarketGateCompletion,
    pub recorded_at_unix_seconds: u64,
    pub source_revision: String,
    pub source_clean: bool,
    pub binary_path: PathBuf,
    pub binary_sha256: String,
    pub detail: String,
}

pub struct LiveMarketGateRecorder {
    report_path: PathBuf,
    report: LiveMarketGateEvidence,
}

impl LiveMarketGateRecorder {
    /// Invalidates any previous result and records an incomplete gate bound to
    /// the current source revision and exact executable.
    ///
    /// # Errors
    /// Returns a coarse error when provider identity, source provenance, the
    /// current executable, hashing, or bounded evidence persistence fails.
    pub fn start(provider: &str) -> Result<Self, String> {
        validate_provider(provider)?;
        let repository = repository_root();
        let evidence_directory = repository.join(".cache/evidence");
        let report_path = evidence_directory.join(format!("live_market_gate_{provider}.json"));
        let binary_path = PathBuf::from(format!("live_market_gate_{provider}.bin"));
        let binary_artifact = evidence_directory.join(&binary_path);

        fs::create_dir_all(&evidence_directory)
            .map_err(|_| "live market gate evidence directory is unavailable".to_string())?;
        // Removing the completed report first is the stale-pass fence. If the
        // process dies after this point, readiness sees NotRun, never the prior pass.
        remove_if_present(&report_path)?;

        let (source_revision, source_clean) = source_provenance(&repository)?;
        let executable = std::env::current_exe()
            .map_err(|_| "live market gate executable is unavailable".to_string())?;
        let executable_file = fs::File::open(&executable)
            .map_err(|_| "live market gate executable is unavailable".to_string())?;
        let executable_bytes = executable_file
            .metadata()
            .map_err(|_| "live market gate executable metadata is unavailable".to_string())?
            .len();
        if executable_bytes == 0 || executable_bytes > LIVE_MARKET_GATE_MAXIMUM_BINARY_BYTES {
            return Err("live market gate executable size is invalid".to_string());
        }
        remove_if_present(&binary_artifact)?;
        let mut bounded_executable =
            executable_file.take(LIVE_MARKET_GATE_MAXIMUM_BINARY_BYTES + 1);
        let mut captured_executable = fs::File::create(&binary_artifact)
            .map_err(|_| "live market gate executable could not be captured".to_string())?;
        let copied_bytes = std::io::copy(&mut bounded_executable, &mut captured_executable)
            .map_err(|_| "live market gate executable could not be captured".to_string())?;
        drop(captured_executable);
        let captured_bytes = fs::metadata(&binary_artifact)
            .map_err(|_| {
                "live market gate captured executable metadata is unavailable".to_string()
            })?
            .len();
        if copied_bytes != executable_bytes
            || captured_bytes != executable_bytes
            || captured_bytes == 0
            || captured_bytes > LIVE_MARKET_GATE_MAXIMUM_BINARY_BYTES
        {
            return Err("live market gate captured executable size is invalid".to_string());
        }
        let binary_sha256 = file_sha256_hex(&binary_artifact, captured_bytes)
            .map_err(|_| "live market gate executable could not be hashed".to_string())?;

        let report = LiveMarketGateEvidence {
            schema_version: LIVE_MARKET_GATE_SCHEMA_VERSION,
            evidence_scope: LIVE_MARKET_GATE_EVIDENCE_SCOPE.to_string(),
            provider: provider.to_string(),
            outcome: LiveMarketGateOutcome::Failed,
            completion_state: LiveMarketGateCompletion::Incomplete,
            recorded_at_unix_seconds: unix_now(),
            source_revision,
            source_clean,
            binary_path,
            binary_sha256,
            detail: INCOMPLETE_DETAIL.to_string(),
        };
        write_report_atomically(&report_path, &report)?;
        Ok(Self {
            report_path,
            report,
        })
    }

    /// Finalizes the gate outcome. The report remains candidate-bound and the
    /// binary artifact is not rewritten after the network test starts.
    ///
    /// # Errors
    /// Returns an error when detail exceeds its bound or atomic persistence fails.
    pub fn finish(mut self, outcome: LiveMarketGateOutcome, detail: &str) -> Result<(), String> {
        if detail.len() > LIVE_MARKET_GATE_MAXIMUM_DETAIL_BYTES {
            return Err("live market gate detail exceeds its bound".to_string());
        }
        let (finishing_revision, finishing_clean) = source_provenance(&repository_root())?;
        self.report.source_clean &=
            finishing_clean && finishing_revision == self.report.source_revision;
        self.report.outcome = outcome;
        self.report.completion_state = LiveMarketGateCompletion::Completed;
        self.report.recorded_at_unix_seconds = unix_now();
        self.report.detail = detail.to_string();
        write_report_atomically(&self.report_path, &self.report)
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn validate_provider(provider: &str) -> Result<(), String> {
    if provider.is_empty()
        || provider.len() > 64
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("live market gate provider identity is invalid".to_string());
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("live market gate prior evidence could not be retired".to_string()),
    }
}

fn source_provenance(repository: &Path) -> Result<(String, bool), String> {
    let revision = git_output(repository, &["rev-parse", "HEAD"])
        .filter(|revision| {
            revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| "live market gate source revision is unavailable".to_string())?;
    let status = git_output(
        repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .ok_or_else(|| "live market gate source status is unavailable".to_string())?;
    Ok((revision, status.is_empty()))
}

fn git_output(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn file_sha256_hex(path: &Path, expected_bytes: u64) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1_024];
    let mut hashed_bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hashed_bytes = hashed_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if hashed_bytes > LIVE_MARKET_GATE_MAXIMUM_BINARY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "live market gate binary exceeds its bound",
            ));
        }
        hasher.update(&buffer[..read]);
    }
    if hashed_bytes != expected_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "live market gate binary size changed while hashing",
        ));
    }
    Ok(lower_hex(hasher.finalize()))
}

fn lower_hex(bytes: impl IntoIterator<Item = u8>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.into_iter();
    let mut encoded = String::with_capacity(bytes.size_hint().0.saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn write_report_atomically(path: &Path, report: &LiveMarketGateEvidence) -> Result<(), String> {
    let mut encoded = serde_json::to_vec_pretty(report)
        .map_err(|_| "live market gate evidence could not be encoded".to_string())?;
    encoded.push(b'\n');
    if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > LIVE_MARKET_GATE_MAXIMUM_REPORT_BYTES {
        return Err("live market gate evidence exceeds its bound".to_string());
    }
    let temporary = path.with_extension("json.tmp");
    remove_if_present(&temporary)?;
    fs::write(&temporary, encoded)
        .map_err(|_| "live market gate evidence could not be written".to_string())?;
    remove_if_present(path)?;
    fs::rename(&temporary, path)
        .map_err(|_| "live market gate evidence could not be committed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_schema_is_strict_and_round_trips() {
        let evidence = LiveMarketGateEvidence {
            schema_version: LIVE_MARKET_GATE_SCHEMA_VERSION,
            evidence_scope: LIVE_MARKET_GATE_EVIDENCE_SCOPE.to_string(),
            provider: "hyperliquid".to_string(),
            outcome: LiveMarketGateOutcome::Passed,
            completion_state: LiveMarketGateCompletion::Completed,
            recorded_at_unix_seconds: 42,
            source_revision: "a".repeat(40),
            source_clean: true,
            binary_path: PathBuf::from("live_market_gate_hyperliquid.bin"),
            binary_sha256: "b".repeat(64),
            detail: "fixture".to_string(),
        };
        let encoded = serde_json::to_vec(&evidence).expect("evidence encodes");
        assert_eq!(
            serde_json::from_slice::<LiveMarketGateEvidence>(&encoded).expect("evidence decodes"),
            evidence
        );

        let mut value = serde_json::to_value(&evidence).expect("fixture converts");
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<LiveMarketGateEvidence>(value).is_err());
    }

    #[test]
    fn provider_identity_and_detail_bounds_fail_closed() {
        for invalid in ["", "Hyperliquid", "bad/provider", "bad_provider"] {
            assert!(validate_provider(invalid).is_err());
        }
        assert!(validate_provider("hyperliquid").is_ok());
        assert!(validate_provider("rithmic").is_ok());
        assert!(INCOMPLETE_DETAIL.len() <= LIVE_MARKET_GATE_MAXIMUM_DETAIL_BYTES);
    }
}
