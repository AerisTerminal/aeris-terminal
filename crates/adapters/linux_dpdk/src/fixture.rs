use crate::PROFILE;
use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, FixtureFrame, FixtureIngestDriver, ReadinessError,
    ReadinessManifest, ReadinessState, software_fixture_capabilities,
};

/// Creates the deterministic lifecycle fixture; it is never a DPDK poll-mode driver.
///
/// # Errors
///
/// Returns an error when reviewed software-fixture evidence denies activation.
pub fn fixture_driver(frames: Vec<FixtureFrame>) -> Result<FixtureIngestDriver, ReadinessError> {
    let capabilities = software_fixture_capabilities(PROFILE);
    let permit = ReadinessManifest::authorize_embedded(&ActivationRequest {
        profile: PROFILE,
        requested: ReadinessState::ContractOnly,
        active_mode: ActiveIngestMode::SoftwareFixture,
        evidence_id: "software_fixture_adapter",
        capabilities: &capabilities,
    })?;
    FixtureIngestDriver::try_new(permit, capabilities, frames)
}
