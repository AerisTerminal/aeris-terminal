//! Isolated DPDK activation gate and software-visible prerequisite probe.
//!
//! No reviewed Rust binding currently meets the Stage 1 compatibility, maintenance,
//! and safety bar. Activation fails explicitly and never falls back to a kernel socket.

use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, BorrowedFrame, DriverCapabilities, DriverHealth,
    DriverLifecycle, FixtureFrame, FixtureIngestDriver, IngestDriver, IngestProfile,
    OverflowReport, QueueBinding, ReadinessError, ReadinessManifest, ReadinessState, ReceiveBatch,
    software_fixture_capabilities,
};
use core::fmt;
use std::{error::Error, num::NonZeroUsize};

pub const PROFILE: IngestProfile = IngestProfile::LinuxDpdk;
pub const NATIVE_DEPENDENCY_SELECTED: bool = false;
pub const POLL_MODE_DRIVER_VERIFIED: bool = false;
pub const SAFETY_REVIEW: &str = "dpdk-stdlib 0.2.0 wraps mbufs but its RX/TX queue methods are placeholders and its sys crate defaults to behaviorally successful stubs";
pub const LICENSE_REVIEW: &str = "dpdk-stdlib 0.2.0 and DPDK userspace licensing are acceptable, but license acceptance does not make placeholder I/O selectable";
pub const PROVENANCE_REVIEW: &str = "dpdk-stdlib 0.2.0 registry source is 2bfbb7f20f1410bc11fea71014f218282dcda9e6; real bindgen accepts any libdpdk >=21.0 instead of one exact reviewed DPDK release";
pub const MAINTENANCE_REVIEW: &str = "dpdk-stdlib 0.2.0 is current but incomplete: queue setup, receive, and transmit explicitly remain placeholders";
pub const BUILD_REVIEW: &str = "dpdk-stdlib-sys 0.2.0 silently compiles stubs unless both DPDK and bindgen are available; that mode can report a fake device and is forbidden for Axiusflow activation";
pub const MISSING_NATIVE_EVIDENCE: &str = "approved exact-pinned binding and DPDK release, audited mbuf ownership wrapper, fuzzing, huge-page/EAL virtual-device lifecycle, PMD queue isolation";

/// Bounded parameters that a future approved DPDK backend must honor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DpdkConfig {
    pub port_id: u16,
    pub queue_id: u16,
    pub descriptor_count: NonZeroUsize,
    pub maximum_frame_bytes: NonZeroUsize,
}

impl DpdkConfig {
    /// Creates a bounded DPDK queue request without activating EAL or a PMD.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported frame size or storage overflow.
    pub fn try_new(
        port_id: u16,
        queue_id: u16,
        descriptor_count: NonZeroUsize,
        maximum_frame_bytes: NonZeroUsize,
    ) -> Result<Self, DpdkError> {
        if maximum_frame_bytes.get() > 65_535 {
            return Err(DpdkError::FrameLimitUnsupported(maximum_frame_bytes.get()));
        }
        descriptor_count
            .get()
            .checked_mul(maximum_frame_bytes.get())
            .ok_or(DpdkError::ReceiveStorageOverflow)?;
        Ok(Self {
            port_id,
            queue_id,
            descriptor_count,
            maximum_frame_bytes,
        })
    }
}

/// Evidence state for one native prerequisite or exercised behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpdkEvidenceStatus {
    Present,
    Missing,
    NotSelected,
    NotExercised,
    Unverified,
}

const fn evidence_status(present: bool) -> DpdkEvidenceStatus {
    if present {
        DpdkEvidenceStatus::Present
    } else {
        DpdkEvidenceStatus::Missing
    }
}

/// Read-only host evidence. It does not initialize EAL, reserve pages, or bind a NIC.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DpdkPrerequisiteReport {
    pub linux_target: bool,
    pub huge_pages_total: u64,
    pub huge_pages_free: u64,
    pub vfio_driver: DpdkEvidenceStatus,
    pub vfio_control: DpdkEvidenceStatus,
    pub pkg_config_metadata: DpdkEvidenceStatus,
    pub native_dependency: DpdkEvidenceStatus,
    pub software_device: DpdkEvidenceStatus,
    pub poll_mode_driver: DpdkEvidenceStatus,
    pub missing_evidence: &'static str,
}

/// Returns software-visible prerequisites without changing host configuration.
#[must_use]
pub fn probe_prerequisites() -> DpdkPrerequisiteReport {
    #[cfg(target_os = "linux")]
    let (
        huge_pages_total,
        huge_pages_free,
        vfio_driver_present,
        vfio_control_present,
        pkg_config_metadata_present,
    ) = {
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let meminfo_value = |name: &str| {
            meminfo.lines().find_map(|line| {
                line.strip_prefix(name)
                    .and_then(|value| value.trim().parse::<u64>().ok())
            })
        };
        let package_metadata_in_common_path = [
            "/usr/lib/pkgconfig/libdpdk.pc",
            "/usr/lib/x86_64-linux-gnu/pkgconfig/libdpdk.pc",
            "/usr/local/lib/pkgconfig/libdpdk.pc",
            "/usr/local/lib64/pkgconfig/libdpdk.pc",
        ]
        .iter()
        .any(|path| std::path::Path::new(path).exists());
        let package_metadata_in_environment =
            std::env::var_os("PKG_CONFIG_PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|path| path.join("libdpdk.pc").exists())
            });
        (
            meminfo_value("HugePages_Total:").unwrap_or(0),
            meminfo_value("HugePages_Free:").unwrap_or(0),
            std::path::Path::new("/sys/bus/pci/drivers/vfio-pci").exists(),
            std::path::Path::new("/dev/vfio/vfio").exists(),
            package_metadata_in_common_path || package_metadata_in_environment,
        )
    };
    #[cfg(not(target_os = "linux"))]
    let (
        huge_pages_total,
        huge_pages_free,
        vfio_driver_present,
        vfio_control_present,
        pkg_config_metadata_present,
    ) = (0, 0, false, false, false);

    DpdkPrerequisiteReport {
        linux_target: cfg!(target_os = "linux"),
        huge_pages_total,
        huge_pages_free,
        vfio_driver: evidence_status(vfio_driver_present),
        vfio_control: evidence_status(vfio_control_present),
        pkg_config_metadata: evidence_status(pkg_config_metadata_present),
        native_dependency: DpdkEvidenceStatus::NotSelected,
        software_device: DpdkEvidenceStatus::NotExercised,
        poll_mode_driver: DpdkEvidenceStatus::Unverified,
        missing_evidence: MISSING_NATIVE_EVIDENCE,
    }
}

/// Explicitly unavailable native adapter. It cannot report poll mode as active.
#[derive(Debug)]
pub struct DpdkDriver {
    capabilities: DriverCapabilities,
    prerequisites: DpdkPrerequisiteReport,
}

impl DpdkDriver {
    #[must_use]
    pub fn unavailable(config: DpdkConfig) -> Self {
        Self {
            capabilities: DriverCapabilities {
                profile: PROFILE,
                supported_modes: Vec::new(),
                receive_queues: 0,
                maximum_frame_bytes: config.maximum_frame_bytes.get(),
                timestamp_sources: Vec::new(),
                zero_copy_verified: false,
            },
            prerequisites: probe_prerequisites(),
        }
    }

    #[must_use]
    pub const fn prerequisites(&self) -> &DpdkPrerequisiteReport {
        &self.prerequisites
    }
}

/// Empty batch type required by the common contract while activation is unavailable.
#[derive(Debug)]
pub struct DpdkReceiveBatch;

impl ReceiveBatch for DpdkReceiveBatch {
    fn frame_count(&self) -> usize {
        0
    }

    fn frame(&self, _index: usize) -> Option<BorrowedFrame<'_>> {
        None
    }

    fn overflow(&self) -> OverflowReport {
        OverflowReport::default()
    }

    fn release(self) {}
}

impl IngestDriver for DpdkDriver {
    type Error = DpdkError;
    type Batch<'driver> = DpdkReceiveBatch;

    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    fn bind_queue(&mut self, _binding: QueueBinding) -> Result<(), Self::Error> {
        Err(DpdkError::NativeIntegrationUnavailable)
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        Err(DpdkError::NativeIntegrationUnavailable)
    }

    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error> {
        Err(DpdkError::NativeIntegrationUnavailable)
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        Err(DpdkError::NativeIntegrationUnavailable)
    }

    fn health(&self) -> DriverHealth {
        DriverHealth {
            active_mode: ActiveIngestMode::Unavailable,
            lifecycle: DriverLifecycle::Unsupported,
            queue_id: 0,
            queued_frames: 0,
            overflow: OverflowReport::default(),
            released_batches: 0,
            abandoned_batches: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpdkError {
    FrameLimitUnsupported(usize),
    ReceiveStorageOverflow,
    NativeIntegrationUnavailable,
}

impl fmt::Display for DpdkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "DPDK ingest error: {self:?}")
    }
}

impl Error for DpdkError {}

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
