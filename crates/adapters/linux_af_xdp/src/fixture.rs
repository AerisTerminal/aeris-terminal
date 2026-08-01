//! Deterministic software fixture driver for this profile.

use crate::review::PROFILE;
use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, FixtureFrame, FixtureIngestDriver, ReadinessError,
    ReadinessManifest, ReadinessState, software_fixture_capabilities,
};

/// Creates the deterministic lifecycle fixture; it is never an `AF_XDP` mode.
///
/// # Errors
///
/// Returns an error when reviewed software-fixture evidence denies activation.
pub fn fixture_driver(frames: Vec<FixtureFrame>) -> Result<FixtureIngestDriver, ReadinessError> {
    let capabilities = software_fixture_capabilities(PROFILE);
    let permit = ReadinessManifest::authorize_embedded(&ActivationRequest {
        profile: PROFILE,
        requested: ReadinessState::Implemented,
        active_mode: ActiveIngestMode::SoftwareFixture,
        evidence_id: "software_fixture_adapter",
        capabilities: &capabilities,
    })?;
    FixtureIngestDriver::try_new(permit, capabilities, frames)
}

#[cfg(test)]
mod tests {
    use super::fixture_driver;
    use axiusflow_transport::{ActiveIngestMode, IngestDriver};

    #[test]
    fn fixture_driver_is_never_an_af_xdp_mode() {
        let driver = fixture_driver(Vec::new()).expect("the software fixture is authorized");
        assert_eq!(
            driver.capabilities().supported_modes,
            vec![ActiveIngestMode::SoftwareFixture],
            "the fixture must not present itself as a native AF_XDP mode"
        );
        assert!(!driver.capabilities().zero_copy_verified);
    }
}
