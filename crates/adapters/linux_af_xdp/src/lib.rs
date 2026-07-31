//! Isolated `AF_XDP` activation gate and software-visible prerequisite probe.
//!
//! No reviewed native binding currently meets the Stage 1 maintenance and safety bar.
//! Activation therefore fails explicitly; the fixture remains a separate contract harness.

use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, BorrowedFrame, DriverCapabilities, DriverHealth,
    DriverLifecycle, FixtureFrame, FixtureIngestDriver, IngestDriver, IngestProfile,
    OverflowReport, QueueBinding, ReadinessError, ReadinessManifest, ReadinessState, ReceiveBatch,
    software_fixture_capabilities,
};
use core::fmt;
use std::{error::Error, num::NonZeroUsize};

pub const PROFILE: IngestProfile = IngestProfile::LinuxAfXdp;
pub const NATIVE_DEPENDENCY_SELECTED: bool = false;
pub const ZERO_COPY_VERIFIED: bool = false;
pub const SAFETY_REVIEW: &str = "xdp 0.7.3 and xdpilone 1.3.0 require caller-managed unsafe UMEM/ring lifetimes; xdp-socket 0.1.4 exposes raw ring state and assertion-guarded pointer access";
pub const LICENSE_REVIEW: &str = "xdp 0.7.3 and xdp-socket 0.1.4 are MIT OR Apache-2.0; xdpilone 1.3.0 is EUPL-1.2 and would require compatibility review";
pub const PROVENANCE_REVIEW: &str = "reviewed registry sources: xdp 0.7.3 at 33e03351173588d44ef7122c3d9050e6c8c1956c, xdpilone 1.3.0 at a8d8bc95f2ea3e894bcfe1d45e07675137bbaff2, xdp-socket 0.1.4 at 15ef0efbd802705667907e4295ed73aa32aff37f";
pub const MAINTENANCE_REVIEW: &str = "xdp 0.7.3 is active but explicitly early and use-case-limited; xdpilone is passively maintained; xdp-socket 0.1.4 has an early raw-pointer API";
pub const BUILD_REVIEW: &str = "pure-Rust socket access still requires separate reviewed BPF/XSKMAP provisioning and privileged lifecycle; portable targets must not link it";
pub const MISSING_NATIVE_EVIDENCE: &str = "approved exact-pinned binding, audited UMEM ownership wrapper, fuzzing, privileged veth copy-mode lifecycle, qualified NIC/driver";

/// Bounded parameters that a future approved native backend must honor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AfXdpConfig {
    pub interface_name: String,
    pub queue_id: u16,
    pub frame_count: NonZeroUsize,
    pub maximum_frame_bytes: NonZeroUsize,
}

impl AfXdpConfig {
    /// Creates a bounded queue request without claiming that the host can activate it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty interface or an Ethernet frame limit above 65,535.
    pub fn try_new(
        interface_name: impl Into<String>,
        queue_id: u16,
        frame_count: NonZeroUsize,
        maximum_frame_bytes: NonZeroUsize,
    ) -> Result<Self, AfXdpError> {
        let interface_name = interface_name.into();
        if interface_name.trim().is_empty() {
            return Err(AfXdpError::EmptyInterfaceName);
        }
        if maximum_frame_bytes.get() > 65_535 {
            return Err(AfXdpError::FrameLimitUnsupported(maximum_frame_bytes.get()));
        }
        frame_count
            .get()
            .checked_mul(maximum_frame_bytes.get())
            .ok_or(AfXdpError::ReceiveStorageOverflow)?;
        Ok(Self {
            interface_name,
            queue_id,
            frame_count,
            maximum_frame_bytes,
        })
    }
}

/// Evidence state for one native prerequisite or exercised behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AfXdpEvidenceStatus {
    Present,
    Missing,
    NotSelected,
    NotExercised,
    Unverified,
}

const fn evidence_status(present: bool) -> AfXdpEvidenceStatus {
    if present {
        AfXdpEvidenceStatus::Present
    } else {
        AfXdpEvidenceStatus::Missing
    }
}

/// Read-only host evidence. It does not load XDP, allocate UMEM, or change an interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AfXdpPrerequisiteReport {
    pub linux_target: bool,
    pub interface: AfXdpEvidenceStatus,
    pub receive_queue: AfXdpEvidenceStatus,
    pub kernel_btf: AfXdpEvidenceStatus,
    pub bpf_filesystem: AfXdpEvidenceStatus,
    pub xdp_diagnostics: AfXdpEvidenceStatus,
    pub native_dependency: AfXdpEvidenceStatus,
    pub copy_mode: AfXdpEvidenceStatus,
    pub zero_copy: AfXdpEvidenceStatus,
    pub missing_evidence: &'static str,
}

/// Returns software-visible prerequisites without making privileged host changes.
#[must_use]
pub fn probe_prerequisites(config: &AfXdpConfig) -> AfXdpPrerequisiteReport {
    #[cfg(target_os = "linux")]
    let (
        interface_present,
        receive_queue_present,
        kernel_btf_present,
        bpf_filesystem_mounted,
        xdp_diagnostics_present,
    ) = {
        let interface_path = std::path::Path::new("/sys/class/net").join(&config.interface_name);
        let receive_queue_present = interface_path
            .join("queues")
            .join(format!("rx-{}", config.queue_id))
            .exists();
        let bpf_filesystem_mounted =
            std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|mounts| {
                mounts.lines().any(|line| {
                    let Some((mount, filesystem)) = line.split_once(" - ") else {
                        return false;
                    };
                    mount.split_whitespace().nth(4) == Some("/sys/fs/bpf")
                        && filesystem.split_whitespace().next() == Some("bpf")
                })
            });
        (
            interface_path.exists(),
            receive_queue_present,
            std::path::Path::new("/sys/kernel/btf/vmlinux").exists(),
            bpf_filesystem_mounted,
            std::path::Path::new("/proc/net/xdp").exists()
                || std::path::Path::new("/sys/module/xsk_diag").exists(),
        )
    };
    #[cfg(not(target_os = "linux"))]
    let _ = config;
    #[cfg(not(target_os = "linux"))]
    let (
        interface_present,
        receive_queue_present,
        kernel_btf_present,
        bpf_filesystem_mounted,
        xdp_diagnostics_present,
    ) = (false, false, false, false, false);

    AfXdpPrerequisiteReport {
        linux_target: cfg!(target_os = "linux"),
        interface: evidence_status(interface_present),
        receive_queue: evidence_status(receive_queue_present),
        kernel_btf: evidence_status(kernel_btf_present),
        bpf_filesystem: evidence_status(bpf_filesystem_mounted),
        xdp_diagnostics: evidence_status(xdp_diagnostics_present),
        native_dependency: AfXdpEvidenceStatus::NotSelected,
        copy_mode: AfXdpEvidenceStatus::NotExercised,
        zero_copy: AfXdpEvidenceStatus::Unverified,
        missing_evidence: MISSING_NATIVE_EVIDENCE,
    }
}

/// Explicitly unavailable native adapter. It cannot silently use sockets or fixtures.
#[derive(Debug)]
pub struct AfXdpDriver {
    capabilities: DriverCapabilities,
    prerequisites: AfXdpPrerequisiteReport,
}

impl AfXdpDriver {
    #[must_use]
    pub fn unavailable(config: &AfXdpConfig) -> Self {
        Self {
            capabilities: DriverCapabilities {
                profile: PROFILE,
                supported_modes: Vec::new(),
                receive_queues: 0,
                maximum_frame_bytes: config.maximum_frame_bytes.get(),
                timestamp_sources: Vec::new(),
                zero_copy_verified: false,
            },
            prerequisites: probe_prerequisites(config),
        }
    }

    #[must_use]
    pub const fn prerequisites(&self) -> &AfXdpPrerequisiteReport {
        &self.prerequisites
    }
}

/// Empty batch type required by the common contract while activation is unavailable.
#[derive(Debug)]
pub struct AfXdpReceiveBatch;

impl ReceiveBatch for AfXdpReceiveBatch {
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

impl IngestDriver for AfXdpDriver {
    type Error = AfXdpError;
    type Batch<'driver> = AfXdpReceiveBatch;

    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    fn bind_queue(&mut self, _binding: QueueBinding) -> Result<(), Self::Error> {
        Err(AfXdpError::NativeIntegrationUnavailable)
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        Err(AfXdpError::NativeIntegrationUnavailable)
    }

    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error> {
        Err(AfXdpError::NativeIntegrationUnavailable)
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        Err(AfXdpError::NativeIntegrationUnavailable)
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
pub enum AfXdpError {
    EmptyInterfaceName,
    FrameLimitUnsupported(usize),
    ReceiveStorageOverflow,
    NativeIntegrationUnavailable,
}

impl fmt::Display for AfXdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "AF_XDP ingest error: {self:?}")
    }
}

impl Error for AfXdpError {}

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
