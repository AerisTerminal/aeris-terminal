//! Ingest profiles and runtime-enforced readiness activation.

use crate::driver::DriverCapabilities;
use crate::errors::ReadinessError;
use crate::profile::{ActiveIngestMode, IngestProfile, ReadinessState, mode_matches_profile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One profile's reviewed maximum claim and evidence gaps.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileReadiness {
    pub profile: IngestProfile,
    pub maximum_readiness: ReadinessState,
    pub evidence: Vec<String>,
    pub limitations: Vec<String>,
}

/// Machine-readable profile evidence used by the activation guard.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessManifest {
    pub schema_version: u32,
    pub profiles: Vec<ProfileReadiness>,
}

impl ReadinessManifest {
    /// Parses and structurally validates a readiness manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON, unsupported schema, missing profiles,
    /// duplicate profile entries, or an evidence-free claim.
    pub fn from_json(json: &str) -> Result<Self, ReadinessError> {
        let manifest: Self = serde_json::from_str(json).map_err(ReadinessError::InvalidJson)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Loads the repository-reviewed Stage 1 readiness manifest.
    ///
    /// # Errors
    ///
    /// Returns an error if the embedded manifest no longer validates.
    pub fn embedded_stage_1() -> Result<Self, ReadinessError> {
        Self::from_json(include_str!("../../../config/ingest_readiness.json"))
    }

    /// Validates immutable runtime evidence against the repository-embedded manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when profile, mode, evidence, capability, or readiness
    /// does not match the reviewed manifest.
    pub fn authorize_embedded(
        request: &ActivationRequest<'_>,
    ) -> Result<ActivationPermit, ReadinessError> {
        Self::embedded_stage_1()?.authorize_reviewed(request)
    }

    fn authorize_reviewed(
        &self,
        request: &ActivationRequest<'_>,
    ) -> Result<ActivationPermit, ReadinessError> {
        let readiness = self
            .profiles
            .iter()
            .find(|entry| entry.profile == request.profile)
            .ok_or(ReadinessError::MissingProfile(request.profile))?;
        if request.capabilities.profile != request.profile {
            return Err(ReadinessError::CapabilityProfileMismatch {
                requested: request.profile,
                reported: request.capabilities.profile,
            });
        }
        if !request
            .capabilities
            .supported_modes
            .contains(&request.active_mode)
        {
            return Err(ReadinessError::ModeUnavailable(request.active_mode));
        }
        if !mode_matches_profile(request.profile, request.active_mode)
            && request.active_mode != ActiveIngestMode::SoftwareFixture
        {
            return Err(ReadinessError::ModeProfileMismatch {
                profile: request.profile,
                mode: request.active_mode,
            });
        }
        if request.active_mode == ActiveIngestMode::SoftwareFixture
            && request.requested > ReadinessState::Implemented
        {
            return Err(ReadinessError::FixtureCannotProveReadiness(
                request.requested,
            ));
        }
        if !readiness
            .evidence
            .iter()
            .any(|evidence| evidence == request.evidence_id)
        {
            return Err(ReadinessError::UnreviewedEvidence(
                request.evidence_id.to_string(),
            ));
        }
        if request.requested > readiness.maximum_readiness {
            return Err(ReadinessError::ClaimExceedsEvidence {
                profile: request.profile,
                requested: request.requested,
                maximum: readiness.maximum_readiness,
            });
        }
        Ok(ActivationPermit {
            profile: request.profile,
            readiness: request.requested,
            active_mode: request.active_mode,
            evidence_id: request.evidence_id.to_string(),
        })
    }

    fn validate(&self) -> Result<(), ReadinessError> {
        if self.schema_version != 1 {
            return Err(ReadinessError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        let mut profiles = BTreeSet::new();
        for entry in &self.profiles {
            if !profiles.insert(entry.profile) {
                return Err(ReadinessError::DuplicateProfile(entry.profile));
            }
            if entry.evidence.is_empty() {
                return Err(ReadinessError::MissingEvidence(entry.profile));
            }
        }
        for required in [
            IngestProfile::PortableSocket,
            IngestProfile::TunedLinuxSocket,
            IngestProfile::LinuxAfXdp,
            IngestProfile::LinuxDpdk,
        ] {
            if !profiles.contains(&required) {
                return Err(ReadinessError::MissingProfile(required));
            }
        }
        Ok(())
    }
}

/// Immutable adapter evidence presented to the activation guard.
#[derive(Clone, Copy, Debug)]
pub struct ActivationRequest<'capabilities> {
    pub profile: IngestProfile,
    pub requested: ReadinessState,
    pub active_mode: ActiveIngestMode,
    pub evidence_id: &'capabilities str,
    pub capabilities: &'capabilities DriverCapabilities,
}

/// Non-forgeable result of passing the runtime readiness guard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationPermit {
    profile: IngestProfile,
    readiness: ReadinessState,
    active_mode: ActiveIngestMode,
    evidence_id: String,
}

impl ActivationPermit {
    #[must_use]
    pub const fn profile(&self) -> IngestProfile {
        self.profile
    }

    #[must_use]
    pub const fn readiness(&self) -> ReadinessState {
        self.readiness
    }

    #[must_use]
    pub const fn active_mode(&self) -> ActiveIngestMode {
        self.active_mode
    }

    #[must_use]
    pub fn evidence_id(&self) -> &str {
        &self.evidence_id
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActivationRequest, ActiveIngestMode, IngestProfile, ReadinessManifest, ReadinessState,
    };
    use crate::driver::{DriverCapabilities, software_fixture_capabilities};
    use crate::errors::ReadinessError;
    use crate::receive_batch::TimestampSource;

    fn native_capabilities(
        profile: IngestProfile,
        mode: ActiveIngestMode,
        zero_copy_verified: bool,
    ) -> DriverCapabilities {
        DriverCapabilities {
            profile,
            supported_modes: vec![mode],
            receive_queues: 1,
            maximum_frame_bytes: 2_048,
            timestamp_sources: vec![TimestampSource::SocketSoftware],
            zero_copy_verified,
        }
    }

    #[test]
    fn embedded_manifest_validates() {
        let manifest = ReadinessManifest::embedded_stage_1().expect("embedded manifest validates");
        assert_eq!(manifest.profiles.len(), 4);
    }

    #[test]
    fn readiness_states_order_from_weakest_to_strongest() {
        assert!(ReadinessState::ContractOnly < ReadinessState::Implemented);
        assert!(ReadinessState::Implemented < ReadinessState::FixtureValidated);
        assert!(ReadinessState::FixtureValidated < ReadinessState::HardwareValidated);
        assert!(ReadinessState::HardwareValidated < ReadinessState::ProviderCertified);
        assert!(ReadinessState::ProviderCertified < ReadinessState::ProductionEnabled);
    }

    #[test]
    fn fixture_mode_cannot_claim_more_than_implemented() {
        let capabilities = software_fixture_capabilities(IngestProfile::PortableSocket);
        let error = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: IngestProfile::PortableSocket,
            requested: ReadinessState::FixtureValidated,
            active_mode: ActiveIngestMode::SoftwareFixture,
            evidence_id: "deterministic_packet_corpus",
            capabilities: &capabilities,
        })
        .expect_err("a fixture must not prove fixture_validated readiness");
        assert!(matches!(
            error,
            ReadinessError::FixtureCannotProveReadiness(ReadinessState::FixtureValidated)
        ));
    }

    #[test]
    fn claim_above_manifest_maximum_is_rejected() {
        let capabilities = native_capabilities(
            IngestProfile::LinuxAfXdp,
            ActiveIngestMode::AfXdpCopy,
            false,
        );
        let error = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: IngestProfile::LinuxAfXdp,
            requested: ReadinessState::ProductionEnabled,
            active_mode: ActiveIngestMode::AfXdpCopy,
            evidence_id: "xsk_rs_0_8_0_af_xdp_copy_driver_review",
            capabilities: &capabilities,
        })
        .expect_err("AF_XDP must not reach production_enabled from the embedded manifest");
        assert!(matches!(
            error,
            ReadinessError::ClaimExceedsEvidence {
                profile: IngestProfile::LinuxAfXdp,
                requested: ReadinessState::ProductionEnabled,
                maximum: ReadinessState::Implemented,
            }
        ));
    }

    #[test]
    fn unreviewed_evidence_identifier_is_rejected() {
        let capabilities = software_fixture_capabilities(IngestProfile::PortableSocket);
        let error = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: IngestProfile::PortableSocket,
            requested: ReadinessState::Implemented,
            active_mode: ActiveIngestMode::SoftwareFixture,
            evidence_id: "evidence_that_was_never_reviewed",
            capabilities: &capabilities,
        })
        .expect_err("an unreviewed evidence identifier must be rejected");
        assert!(matches!(error, ReadinessError::UnreviewedEvidence(_)));
    }

    #[test]
    fn mode_outside_reported_capabilities_is_rejected() {
        let capabilities = native_capabilities(
            IngestProfile::LinuxAfXdp,
            ActiveIngestMode::AfXdpCopy,
            false,
        );
        let error = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: IngestProfile::LinuxAfXdp,
            requested: ReadinessState::Implemented,
            active_mode: ActiveIngestMode::AfXdpZeroCopy,
            evidence_id: "xsk_rs_0_8_0_af_xdp_copy_driver_review",
            capabilities: &capabilities,
        })
        .expect_err("a mode absent from reported capabilities must be rejected");
        assert!(matches!(
            error,
            ReadinessError::ModeUnavailable(ActiveIngestMode::AfXdpZeroCopy)
        ));
    }

    #[test]
    fn capability_profile_mismatch_is_rejected() {
        let capabilities = software_fixture_capabilities(IngestProfile::LinuxDpdk);
        let error = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: IngestProfile::PortableSocket,
            requested: ReadinessState::Implemented,
            active_mode: ActiveIngestMode::SoftwareFixture,
            evidence_id: "deterministic_packet_corpus",
            capabilities: &capabilities,
        })
        .expect_err("capabilities reporting another profile must be rejected");
        assert!(matches!(
            error,
            ReadinessError::CapabilityProfileMismatch {
                requested: IngestProfile::PortableSocket,
                reported: IngestProfile::LinuxDpdk,
            }
        ));
    }

    #[test]
    fn dpdk_poll_mode_is_capped_at_contract_only() {
        let capabilities = native_capabilities(
            IngestProfile::LinuxDpdk,
            ActiveIngestMode::DpdkPollMode,
            false,
        );
        let error = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: IngestProfile::LinuxDpdk,
            requested: ReadinessState::Implemented,
            active_mode: ActiveIngestMode::DpdkPollMode,
            evidence_id: "software_fixture_adapter",
            capabilities: &capabilities,
        })
        .expect_err("DPDK poll mode has no implemented-level evidence");
        assert!(matches!(
            error,
            ReadinessError::ClaimExceedsEvidence {
                profile: IngestProfile::LinuxDpdk,
                maximum: ReadinessState::ContractOnly,
                ..
            }
        ));
    }

    #[test]
    fn malformed_manifest_json_is_rejected() {
        let error = ReadinessManifest::from_json("{ not json }")
            .expect_err("malformed manifest JSON must be rejected");
        assert!(matches!(error, ReadinessError::InvalidJson(_)));
    }
}
