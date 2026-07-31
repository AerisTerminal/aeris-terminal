//! macOS portable-network capability boundary.
//!
//! `Network.framework` remains unclaimed until exact dependencies and host evidence exist.

use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, FixtureFrame, FixtureIngestDriver, IngestProfile,
    ReadinessError, ReadinessManifest, ReadinessState, software_fixture_capabilities,
};

pub const PROFILE: IngestProfile = IngestProfile::PortableSocket;
pub const NATIVE_MODE_IMPLEMENTED: bool = false;

/// # Errors
///
/// Returns an error when reviewed software-fixture evidence denies activation.
pub fn fixture_driver(frames: Vec<FixtureFrame>) -> Result<FixtureIngestDriver, ReadinessError> {
    let capabilities = software_fixture_capabilities(PROFILE);
    let permit = ReadinessManifest::authorize_embedded(&ActivationRequest {
        profile: PROFILE,
        requested: ReadinessState::Implemented,
        active_mode: ActiveIngestMode::SoftwareFixture,
        evidence_id: "deterministic_packet_corpus",
        capabilities: &capabilities,
    })?;
    FixtureIngestDriver::try_new(permit, capabilities, frames)
}
