use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAXIMUM_EVIDENCE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedSilence {
    HeartbeatSilence,
    MessageSilence,
}

impl ExpectedSilence {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "heartbeat-silence" => Some(Self::HeartbeatSilence),
            "message-silence" => Some(Self::MessageSilence),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSilenceEvidence {
    pub schema: u8,
    pub provenance: EvidenceProvenance,
    pub status: EvidenceStatus,
    pub scope: EvidenceScope,
    pub boundary: EvidenceBoundary,
    pub expected_invalidation: ExpectedSilence,
    pub observed_invalidation: Option<ExpectedSilence>,
    pub observed_invalidation_generation: Option<u64>,
    pub observed_retry: Option<ObservedRetry>,
    pub observation_method: ObservationMethod,
    pub local_interference: LocalInterference,
    pub timing_metadata_scope: TimingMetadataScope,
    pub qualification_limitation: Option<QualificationLimitation>,
    pub credentials_source: CredentialSource,
    pub observation_generation: u64,
    pub recovery_generation: u64,
    pub observation: SessionEvidence,
    pub recovery: SessionEvidence,
    pub clean_protocol_close: bool,
    pub observation_limit_ms: u64,
    pub observation_elapsed_ms: u64,
    pub provider_causation_claimed: bool,
    pub failure: Option<EvidenceFailure>,
    pub qualified: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Incomplete,
    Passed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceScope {
    RawProviderPathPassiveObservation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceBoundary {
    RawRithmicAdapter,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceProvenance {
    pub source_revision: String,
    pub executable_sha256: String,
    pub cargo_lock_sha256: String,
}

impl EvidenceProvenance {
    pub fn try_new(
        source_revision: &str,
        executable_sha256: &str,
        cargo_lock_sha256: &str,
    ) -> Result<Self, String> {
        let provenance = Self {
            source_revision: source_revision.to_ascii_lowercase(),
            executable_sha256: executable_sha256.to_ascii_lowercase(),
            cargo_lock_sha256: cargo_lock_sha256.to_ascii_lowercase(),
        };
        if !provenance.valid() {
            return Err("provider_silence_provenance_invalid".to_string());
        }
        Ok(provenance)
    }

    pub fn for_current_executable(
        source_revision: &str,
        cargo_lock_sha256: &str,
    ) -> Result<Self, String> {
        Self::try_new(
            source_revision,
            &current_executable_sha256()?,
            cargo_lock_sha256,
        )
    }

    pub fn verify_current_executable(&self) -> Result<(), String> {
        if current_executable_sha256()? != self.executable_sha256 {
            return Err("provider_silence_executable_hash_mismatch".to_string());
        }
        Ok(())
    }

    fn valid(&self) -> bool {
        valid_lower_hex(&self.source_revision, 40)
            && valid_lower_hex(&self.executable_sha256, 64)
            && valid_lower_hex(&self.cargo_lock_sha256, 64)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationMethod {
    pub provider_path_observed: bool,
    pub production_receive_loop: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalInterference {
    pub client_local_suppression: bool,
    pub local_fault_injection: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEvidence {
    pub authenticated: bool,
    pub instruments_discovered: bool,
    pub stopped: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedRetry {
    Transient,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingMetadataScope {
    LocalMonotonicDetectorOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationLimitation {
    MessageSilenceTimingNotProven,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    NativeVault,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceFailure {
    ObservationStart,
    ObservationTimeout,
    ObservationInvalidationMismatch,
    ObservationStop,
    RecoveryStart,
    RecoveryTimeout,
    RecoveryInvalidated,
    RecoveryStop,
    CleanClose,
    TimingMetadataInsufficient,
}

impl ProviderSilenceEvidence {
    pub fn incomplete(
        expected: ExpectedSilence,
        observation_limit_ms: u64,
        provenance: EvidenceProvenance,
    ) -> Self {
        Self {
            schema: 2,
            provenance,
            status: EvidenceStatus::Incomplete,
            scope: EvidenceScope::RawProviderPathPassiveObservation,
            boundary: EvidenceBoundary::RawRithmicAdapter,
            expected_invalidation: expected,
            observed_invalidation: None,
            observed_invalidation_generation: None,
            observed_retry: None,
            observation_method: ObservationMethod {
                provider_path_observed: false,
                production_receive_loop: true,
            },
            local_interference: LocalInterference {
                client_local_suppression: false,
                local_fault_injection: false,
            },
            timing_metadata_scope: TimingMetadataScope::LocalMonotonicDetectorOnly,
            qualification_limitation: (expected == ExpectedSilence::MessageSilence)
                .then_some(QualificationLimitation::MessageSilenceTimingNotProven),
            credentials_source: CredentialSource::NativeVault,
            observation_generation: 1,
            recovery_generation: 2,
            observation: SessionEvidence {
                authenticated: false,
                instruments_discovered: false,
                stopped: false,
            },
            recovery: SessionEvidence {
                authenticated: false,
                instruments_discovered: false,
                stopped: false,
            },
            clean_protocol_close: false,
            observation_limit_ms,
            observation_elapsed_ms: 0,
            provider_causation_claimed: false,
            failure: Some(EvidenceFailure::ObservationStart),
            qualified: false,
        }
    }

    pub fn qualify(&mut self) {
        self.qualified = self.schema == 2
            && self.provenance.valid()
            && self.status == EvidenceStatus::Passed
            && self.scope == EvidenceScope::RawProviderPathPassiveObservation
            && self.boundary == EvidenceBoundary::RawRithmicAdapter
            && self.expected_invalidation == ExpectedSilence::HeartbeatSilence
            && self.observed_invalidation == Some(self.expected_invalidation)
            && self.observed_invalidation_generation == Some(self.observation_generation)
            && self.observed_retry == Some(ObservedRetry::Transient)
            && self.observation_method.provider_path_observed
            && self.observation_method.production_receive_loop
            && self.credentials_source == CredentialSource::NativeVault
            && !self.local_interference.client_local_suppression
            && !self.local_interference.local_fault_injection
            && self.timing_metadata_scope == TimingMetadataScope::LocalMonotonicDetectorOnly
            && self.qualification_limitation.is_none()
            && self.observation_generation == 1
            && self.recovery_generation == 2
            && self.observation.authenticated
            && self.observation.instruments_discovered
            && self.observation.stopped
            && self.recovery.authenticated
            && self.recovery.instruments_discovered
            && self.recovery.stopped
            && self.clean_protocol_close
            && self.observation_limit_ms > 0
            && self.observation_elapsed_ms <= self.observation_limit_ms
            && !self.provider_causation_claimed
            && self.failure.is_none();
    }
}

pub fn write_new_atomically(path: &Path, evidence: &ProviderSilenceEvidence) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if path.exists() || path.file_name().is_none() || !parent.is_dir() {
        return Err("provider_silence_evidence_path_invalid".to_string());
    }
    let encoded = serde_json::to_vec_pretty(evidence)
        .map_err(|_| "provider_silence_evidence_encode_failed".to_string())?;
    if encoded.len() > MAXIMUM_EVIDENCE_BYTES {
        return Err("provider_silence_evidence_too_large".to_string());
    }
    let mut temporary = temporary_path(path)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| "provider_silence_evidence_create_failed".to_string())?;
    let result = (|| {
        file.write_all(&encoded)
            .map_err(|_| "provider_silence_evidence_write_failed".to_string())?;
        file.sync_all()
            .map_err(|_| "provider_silence_evidence_sync_failed".to_string())?;
        drop(file);
        fs::hard_link(&temporary, path)
            .map_err(|_| "provider_silence_evidence_publish_failed".to_string())?;
        let _ = fs::remove_file(&temporary);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    } else {
        temporary.clear();
    }
    result
}

pub fn verify(
    path: &Path,
    expected_provenance: &EvidenceProvenance,
) -> Result<ProviderSilenceEvidence, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "provider_silence_evidence_missing".to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAXIMUM_EVIDENCE_BYTES as u64
    {
        return Err("provider_silence_evidence_size_invalid".to_string());
    }
    let bytes = fs::read(path).map_err(|_| "provider_silence_evidence_read_failed".to_string())?;
    let mut evidence: ProviderSilenceEvidence = serde_json::from_slice(&bytes)
        .map_err(|_| "provider_silence_evidence_schema_invalid".to_string())?;
    expected_provenance.verify_current_executable()?;
    if evidence.provenance != *expected_provenance {
        return Err("provider_silence_evidence_provenance_mismatch".to_string());
    }
    let claimed = evidence.qualified;
    evidence.qualify();
    if !claimed || !evidence.qualified {
        return Err("provider_silence_evidence_not_qualified".to_string());
    }
    Ok(evidence)
}

fn current_executable_sha256() -> Result<String, String> {
    let path = std::env::current_exe()
        .map_err(|_| "provider_silence_current_executable_unavailable".to_string())?;
    hash_file_sha256(&path)
}

fn hash_file_sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|_| "provider_silence_provenance_file_unavailable".to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "provider_silence_provenance_hash_failed".to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn valid_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn temporary_path(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "provider_silence_evidence_path_invalid".to_string())?;
    Ok(path.with_file_name(format!(".{name}.{}.tmp", std::process::id())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn qualified() -> ProviderSilenceEvidence {
        let mut evidence = ProviderSilenceEvidence::incomplete(
            ExpectedSilence::HeartbeatSilence,
            60_000,
            test_provenance(),
        );
        evidence.status = EvidenceStatus::Passed;
        evidence.observed_invalidation = Some(ExpectedSilence::HeartbeatSilence);
        evidence.observed_invalidation_generation = Some(evidence.observation_generation);
        evidence.observed_retry = Some(ObservedRetry::Transient);
        evidence.observation_method.provider_path_observed = true;
        evidence.observation.authenticated = true;
        evidence.observation.instruments_discovered = true;
        evidence.observation.stopped = true;
        evidence.recovery.authenticated = true;
        evidence.recovery.instruments_discovered = true;
        evidence.recovery.stopped = true;
        evidence.clean_protocol_close = true;
        evidence.observation_elapsed_ms = 30_000;
        evidence.failure = None;
        evidence.qualify();
        evidence
    }

    fn test_provenance() -> EvidenceProvenance {
        EvidenceProvenance::for_current_executable(&"a".repeat(40), &"b".repeat(64))
            .expect("test executable hashes")
    }

    fn test_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock is valid")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "axiusflow-{name}-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[test]
    fn verifier_accepts_only_complete_provider_observed_evidence() {
        let path = test_path("provider-silence-qualified");
        write_new_atomically(&path, &qualified()).expect("evidence writes");
        assert!(verify(&path, &test_provenance()).is_ok());
        fs::remove_file(path).expect("test evidence removes");
    }

    #[test]
    fn verifier_rejects_false_provenance_and_generation_claims() {
        for (name, mutate) in [
            ("local", 0_u8),
            ("generation", 1_u8),
            ("reason", 2_u8),
            ("causation", 3_u8),
            ("observed-generation", 4_u8),
        ] {
            let path = test_path(name);
            let mut evidence = qualified();
            match mutate {
                0 => evidence.local_interference.client_local_suppression = true,
                1 => evidence.recovery_generation = 3,
                2 => evidence.observed_invalidation = Some(ExpectedSilence::MessageSilence),
                3 => evidence.provider_causation_claimed = true,
                _ => evidence.observed_invalidation_generation = Some(9),
            }
            write_new_atomically(&path, &evidence).expect("evidence writes");
            assert!(verify(&path, &test_provenance()).is_err());
            fs::remove_file(path).expect("test evidence removes");
        }
    }

    #[test]
    fn message_silence_remains_unqualified_without_negotiated_timing() {
        let path = test_path("message-silence-timing-limited");
        let mut evidence = qualified();
        evidence.expected_invalidation = ExpectedSilence::MessageSilence;
        evidence.observed_invalidation = Some(ExpectedSilence::MessageSilence);
        evidence.qualification_limitation =
            Some(QualificationLimitation::MessageSilenceTimingNotProven);
        evidence.qualify();
        assert!(!evidence.qualified);
        write_new_atomically(&path, &evidence).expect("evidence writes");
        assert!(verify(&path, &test_provenance()).is_err());
        fs::remove_file(path).expect("test evidence removes");
    }

    #[test]
    fn atomic_writer_refuses_to_replace_prior_evidence() {
        let path = test_path("provider-silence-no-replace");
        write_new_atomically(&path, &qualified()).expect("first evidence writes");
        assert!(write_new_atomically(&path, &qualified()).is_err());
        assert!(verify(&path, &test_provenance()).is_ok());
        fs::remove_file(path).expect("test evidence removes");
    }

    #[test]
    fn verifier_rejects_malformed_or_mismatched_immutable_provenance() {
        assert!(EvidenceProvenance::try_new("short", &"1".repeat(64), &"2".repeat(64)).is_err());
        let wrong_executable =
            EvidenceProvenance::try_new(&"a".repeat(40), &"d".repeat(64), &"b".repeat(64))
                .expect("formatted provenance is accepted");
        assert!(wrong_executable.verify_current_executable().is_err());
        let path = test_path("provider-silence-provenance");
        write_new_atomically(&path, &qualified()).expect("evidence writes");
        let mismatch = EvidenceProvenance::for_current_executable(&"c".repeat(40), &"b".repeat(64))
            .expect("test executable hashes");
        assert!(verify(&path, &mismatch).is_err());
        fs::remove_file(path).expect("test evidence removes");
    }
}
