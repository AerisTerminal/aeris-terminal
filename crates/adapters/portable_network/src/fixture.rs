use crate::PROFILE;
use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, FixtureFrame, FixtureIngestDriver, ReadinessError,
    ReadinessManifest, ReadinessState, software_fixture_capabilities,
};

/// Creates the bounded lifecycle fixture only after runtime evidence authorization.
///
/// # Errors
///
/// Returns an error when the manifest or fixture capabilities deny activation.
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
