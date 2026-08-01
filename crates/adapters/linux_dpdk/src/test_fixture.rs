use crate::{
    DpdkConfig, DpdkDriver, DpdkError, DpdkEvidenceStatus, NATIVE_DEPENDENCY_SELECTED, PROFILE,
    fixture_driver,
};
use axiusflow_transport::{
    ActiveIngestMode, DriverLifecycle, IngestDriver, IngestProfile, QueueBinding,
};
use std::num::NonZeroUsize;

fn limit(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test limit is non-zero")
}

fn config() -> DpdkConfig {
    DpdkConfig::try_new(0, 0, limit(64), limit(2_048))
        .expect("a bounded DPDK configuration is accepted")
}

#[test]
fn profile_is_linux_dpdk_and_no_native_dependency_is_selected() {
    assert_eq!(PROFILE, IngestProfile::LinuxDpdk);
    const {
        assert!(
            !NATIVE_DEPENDENCY_SELECTED,
            "no reviewed DPDK binding is selected, so this must stay false"
        );
    }
}

#[test]
fn frame_limit_above_ethernet_maximum_is_rejected() {
    let error = DpdkConfig::try_new(0, 0, limit(64), limit(65_536))
        .expect_err("a frame limit above 65,535 must be rejected");
    assert!(matches!(error, DpdkError::FrameLimitUnsupported(65_536)));
}

#[test]
fn unavailable_driver_reports_no_supported_modes() {
    let driver = DpdkDriver::unavailable(config());
    assert!(
        driver.capabilities().supported_modes.is_empty(),
        "an unavailable DPDK driver must advertise no active mode"
    );
    assert_eq!(driver.health().active_mode, ActiveIngestMode::Unavailable);
    assert_eq!(driver.health().lifecycle, DriverLifecycle::Unsupported);
}

#[test]
fn activation_fails_explicitly_and_never_falls_back_to_a_kernel_socket() {
    let mut driver = DpdkDriver::unavailable(config());
    let binding = QueueBinding {
        queue_id: 0,
        maximum_batch_items: limit(2),
        maximum_frame_bytes: limit(2_048),
    };
    assert!(matches!(
        driver.bind_queue(binding),
        Err(DpdkError::NativeIntegrationUnavailable)
    ));
    assert!(matches!(
        driver.start(),
        Err(DpdkError::NativeIntegrationUnavailable)
    ));
    assert!(matches!(
        driver.receive_batch(),
        Err(DpdkError::NativeIntegrationUnavailable)
    ));
    assert_eq!(
        driver.health().active_mode,
        ActiveIngestMode::Unavailable,
        "a failed activation must never report an active poll mode"
    );
}

#[test]
fn poll_mode_driver_is_never_reported_as_exercised() {
    let driver = DpdkDriver::unavailable(config());
    let prerequisites = driver.prerequisites();
    assert_eq!(
        prerequisites.poll_mode_driver,
        DpdkEvidenceStatus::Unverified
    );
    assert_eq!(
        prerequisites.software_device,
        DpdkEvidenceStatus::NotExercised
    );
    assert_eq!(
        prerequisites.native_dependency,
        DpdkEvidenceStatus::NotSelected
    );
    assert!(
        prerequisites.huge_pages_free <= prerequisites.huge_pages_total,
        "free huge pages must never exceed the reported total"
    );
}

#[test]
fn fixture_driver_is_never_a_poll_mode() {
    let driver = fixture_driver(Vec::new()).expect("the software fixture is authorized");
    assert_eq!(
        driver.capabilities().supported_modes,
        vec![ActiveIngestMode::SoftwareFixture],
        "the fixture must not present itself as a native DPDK poll mode"
    );
}
