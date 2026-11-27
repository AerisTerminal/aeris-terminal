//! Phase L Sandboxed Execution Boundary: versioned transport-safe ABI contract,
//! columnized market/output payloads, explicit resource limits, and admission validation.
//!
//! User-authored studies are never loaded as arbitrary native libraries (DLL/dylib/so) and
//! never receive Rust references, trait objects, function pointers, or ambient OS authority.
//! Any future WebAssembly component runtime binds to this versioned transport contract.

use crate::project::{StudyProjectCapability, StudyProjectValidationError};
use crate::{
    MAXIMUM_STUDY_ALERTS_PER_WAVE, MAXIMUM_STUDY_IDENTIFIER_BYTES, MAXIMUM_STUDY_SCENE_OBJECTS,
    MAXIMUM_STUDY_TABLE_CELLS, STUDY_SDK_COMPATIBILITY_EPOCH,
};
use std::collections::BTreeSet;

/// Current version of the transport-safe sandbox guest/host ABI.
pub const STUDY_SANDBOX_ABI_VERSION: u32 = 1;
/// Maximum admitted sandbox artifact payload size (4 MiB).
pub const MAXIMUM_SANDBOX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;
/// Default per-invocation instruction fuel ceiling for sandboxed studies.
pub const DEFAULT_SANDBOX_FUEL_LIMIT: u64 = 5_000_000;
/// Default per-invocation wall deadline in milliseconds (matches native worker deadline).
pub const DEFAULT_SANDBOX_DEADLINE_MS: u64 = 50;
/// Default linear memory ceiling per sandboxed study instance (16 MiB).
pub const DEFAULT_SANDBOX_MEMORY_BYTES: usize = 16 * 1024 * 1024;
/// Default stack ceiling per sandboxed study instance (512 KiB).
pub const DEFAULT_SANDBOX_STACK_BYTES: usize = 512 * 1024;

/// Explicit resource ceilings enforced on one sandboxed study instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudySandboxLimits {
    pub max_instruction_fuel: u64,
    pub max_deadline_ms: u64,
    pub max_memory_bytes: usize,
    pub max_stack_bytes: usize,
    pub max_output_points: usize,
    pub max_scene_objects: usize,
    pub max_table_cells: usize,
    pub max_alerts_per_wave: usize,
}

impl Default for StudySandboxLimits {
    fn default() -> Self {
        Self {
            max_instruction_fuel: DEFAULT_SANDBOX_FUEL_LIMIT,
            max_deadline_ms: DEFAULT_SANDBOX_DEADLINE_MS,
            max_memory_bytes: DEFAULT_SANDBOX_MEMORY_BYTES,
            max_stack_bytes: DEFAULT_SANDBOX_STACK_BYTES,
            max_output_points: 16_384,
            max_scene_objects: MAXIMUM_STUDY_SCENE_OBJECTS,
            max_table_cells: MAXIMUM_STUDY_TABLE_CELLS,
            max_alerts_per_wave: MAXIMUM_STUDY_ALERTS_PER_WAVE,
        }
    }
}

/// Versioned package header validated before admitting a compiled user study into the runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudySandboxPackageHeader {
    pub abi_version: u32,
    pub sdk_compatibility_epoch: u32,
    pub identifier: String,
    pub implementation_revision: u32,
    pub capabilities: BTreeSet<StudyProjectCapability>,
    pub requested_limits: StudySandboxLimits,
    /// SHA-256 hex digest of the compiled sandbox component bytes.
    pub artifact_sha256: String,
    pub artifact_bytes: usize,
}

/// Admission rejection reason for a sandboxed user-study package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudySandboxAdmissionError {
    UnsupportedAbiVersion { package_abi: u32, host_abi: u32 },
    IncompatibleSdkEpoch { package_epoch: u32, host_epoch: u32 },
    InvalidIdentifier,
    ReservedIdentifier,
    InvalidImplementationRevision,
    EmptyCapabilities,
    CapabilityDenied(StudyProjectCapability),
    ArtifactEmpty,
    ArtifactTooLarge { maximum: usize, actual: usize },
    InvalidArtifactDigest,
    ResourceLimitExceeded(&'static str),
}

impl From<StudyProjectValidationError> for StudySandboxAdmissionError {
    fn from(error: StudyProjectValidationError) -> Self {
        match error {
            StudyProjectValidationError::ReservedIdentifier => Self::ReservedIdentifier,
            StudyProjectValidationError::InvalidImplementationRevision => {
                Self::InvalidImplementationRevision
            }
            StudyProjectValidationError::EmptyCapabilities => Self::EmptyCapabilities,
            StudyProjectValidationError::InvalidDigest => Self::InvalidArtifactDigest,
            _ => Self::InvalidIdentifier,
        }
    }
}

/// Validates a compiled sandbox package header against host ABI, SDK epoch,
/// allowed capabilities, digest format, and resource ceilings.
///
/// # Errors
/// Returns [`StudySandboxAdmissionError`] if any admission invariant is violated.
pub fn validate_sandbox_package(
    header: &StudySandboxPackageHeader,
    allowed_capabilities: &BTreeSet<StudyProjectCapability>,
    host_limits: &StudySandboxLimits,
) -> Result<(), StudySandboxAdmissionError> {
    if header.abi_version != STUDY_SANDBOX_ABI_VERSION {
        return Err(StudySandboxAdmissionError::UnsupportedAbiVersion {
            package_abi: header.abi_version,
            host_abi: STUDY_SANDBOX_ABI_VERSION,
        });
    }
    if header.sdk_compatibility_epoch != STUDY_SDK_COMPATIBILITY_EPOCH {
        return Err(StudySandboxAdmissionError::IncompatibleSdkEpoch {
            package_epoch: header.sdk_compatibility_epoch,
            host_epoch: STUDY_SDK_COMPATIBILITY_EPOCH,
        });
    }
    let id = header.identifier.trim();
    if id.is_empty()
        || id != header.identifier
        || id.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(StudySandboxAdmissionError::InvalidIdentifier);
    }
    if id.starts_with("builtin.") {
        return Err(StudySandboxAdmissionError::ReservedIdentifier);
    }
    if header.implementation_revision == 0 {
        return Err(StudySandboxAdmissionError::InvalidImplementationRevision);
    }
    if header.capabilities.is_empty() {
        return Err(StudySandboxAdmissionError::EmptyCapabilities);
    }
    for &capability in &header.capabilities {
        if !allowed_capabilities.contains(&capability) {
            return Err(StudySandboxAdmissionError::CapabilityDenied(capability));
        }
    }
    if header.artifact_bytes == 0 {
        return Err(StudySandboxAdmissionError::ArtifactEmpty);
    }
    if header.artifact_bytes > MAXIMUM_SANDBOX_ARTIFACT_BYTES {
        return Err(StudySandboxAdmissionError::ArtifactTooLarge {
            maximum: MAXIMUM_SANDBOX_ARTIFACT_BYTES,
            actual: header.artifact_bytes,
        });
    }
    if header.artifact_sha256.len() != 64
        || !header
            .artifact_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(StudySandboxAdmissionError::InvalidArtifactDigest);
    }
    let req = &header.requested_limits;
    if req.max_instruction_fuel == 0 || req.max_instruction_fuel > host_limits.max_instruction_fuel
    {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "instruction_fuel",
        ));
    }
    if req.max_deadline_ms == 0 || req.max_deadline_ms > host_limits.max_deadline_ms {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "deadline_ms",
        ));
    }
    if req.max_memory_bytes == 0 || req.max_memory_bytes > host_limits.max_memory_bytes {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "memory_bytes",
        ));
    }
    if req.max_stack_bytes == 0 || req.max_stack_bytes > host_limits.max_stack_bytes {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "stack_bytes",
        ));
    }
    if req.max_output_points > host_limits.max_output_points {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "output_points",
        ));
    }
    if req.max_scene_objects > host_limits.max_scene_objects {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "scene_objects",
        ));
    }
    if req.max_table_cells > host_limits.max_table_cells {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "table_cells",
        ));
    }
    if req.max_alerts_per_wave > host_limits.max_alerts_per_wave {
        return Err(StudySandboxAdmissionError::ResourceLimitExceeded(
            "alerts_per_wave",
        ));
    }
    Ok(())
}

/// Columnized fixed-point bar batch for crossing a sandbox boundary in one bulk transfer
/// rather than paying per-bar host-call overhead.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StudySandboxColumnarBars {
    pub price_scale: u8,
    pub quantity_scale: u8,
    pub timestamps_unix_nanos: Vec<i64>,
    pub open: Vec<i64>,
    pub high: Vec<i64>,
    pub low: Vec<i64>,
    pub close: Vec<i64>,
    pub volume: Vec<i64>,
}

impl StudySandboxColumnarBars {
    /// Returns the number of bars in this columnized batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.timestamps_unix_nanos.len()
    }

    /// Returns whether this columnized batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.timestamps_unix_nanos.is_empty()
    }

    /// Validates that all columns share the exact same length and strictly increasing timestamps.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        let n = self.timestamps_unix_nanos.len();
        self.open.len() == n
            && self.high.len() == n
            && self.low.len() == n
            && self.close.len() == n
            && self.volume.len() == n
            && self
                .timestamps_unix_nanos
                .windows(2)
                .all(|pair| pair[0] < pair[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_header() -> StudySandboxPackageHeader {
        StudySandboxPackageHeader {
            abi_version: STUDY_SANDBOX_ABI_VERSION,
            sdk_compatibility_epoch: STUDY_SDK_COMPATIBILITY_EPOCH,
            identifier: "user.ksr6".to_string(),
            implementation_revision: 1,
            capabilities: BTreeSet::from([
                StudyProjectCapability::MarketBars,
                StudyProjectCapability::SemanticScene,
            ]),
            requested_limits: StudySandboxLimits::default(),
            artifact_sha256: "f".repeat(64),
            artifact_bytes: 128 * 1024,
        }
    }

    #[test]
    fn sandbox_admission_accepts_valid_package_and_enforces_policy_and_limits() {
        let allowed = BTreeSet::from([
            StudyProjectCapability::MarketBars,
            StudyProjectCapability::SemanticScene,
            StudyProjectCapability::StudyAlerts,
        ]);
        let limits = StudySandboxLimits::default();
        let header = valid_header();
        validate_sandbox_package(&header, &allowed, &limits).expect("valid package admits");

        // Deny ungranted capability.
        let mut denied = valid_header();
        denied
            .capabilities
            .insert(StudyProjectCapability::MarketMicrostructure);
        assert_eq!(
            validate_sandbox_package(&denied, &allowed, &limits),
            Err(StudySandboxAdmissionError::CapabilityDenied(
                StudyProjectCapability::MarketMicrostructure
            ))
        );

        // Reject excessive memory request.
        let mut bloated = valid_header();
        bloated.requested_limits.max_memory_bytes = limits.max_memory_bytes + 1;
        assert_eq!(
            validate_sandbox_package(&bloated, &allowed, &limits),
            Err(StudySandboxAdmissionError::ResourceLimitExceeded(
                "memory_bytes"
            ))
        );
    }

    #[test]
    fn columnar_bar_batch_validates_alignment_and_monotonic_timestamps() {
        let batch = StudySandboxColumnarBars {
            price_scale: 2,
            quantity_scale: 0,
            timestamps_unix_nanos: vec![1_000, 2_000],
            open: vec![100, 101],
            high: vec![105, 106],
            low: vec![99, 100],
            close: vec![101, 105],
            volume: vec![50, 75],
        };
        assert!(batch.is_consistent());
        assert_eq!(batch.len(), 2);
    }
}
