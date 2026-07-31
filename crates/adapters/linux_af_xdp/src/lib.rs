#![deny(unsafe_code)]
//! Linux `AF_XDP` copy-mode driver and explicit unavailable activation gate.
//!
//! The native dependency is Linux-only. All calls into its unsafe UMEM and ring API are
//! quarantined in the private `native` module; the public driver owns every descriptor
//! until it is submitted, receives ownership back from RX, and recycles it on batch
//! release or drop.

use axiusflow_transport::{
    ActivationPermit, ActivationRequest, ActiveIngestMode, BorrowedFrame, DriverCapabilities,
    DriverHealth, DriverLifecycle, FixtureFrame, FixtureIngestDriver, IngestDriver, IngestProfile,
    OverflowReport, QueueBinding, ReadinessError, ReadinessManifest, ReadinessState, ReceiveBatch,
    software_fixture_capabilities,
};
#[cfg(target_os = "linux")]
use axiusflow_transport::{ReceiveMetadata, TimestampSource};
use core::fmt;
use std::{error::Error, num::NonZeroUsize};

pub const PROFILE: IngestProfile = IngestProfile::LinuxAfXdp;
pub const NATIVE_DEPENDENCY_SELECTED: bool = true;
pub const ZERO_COPY_VERIFIED: bool = false;
pub const SAFETY_REVIEW: &str = "xsk-rs 0.8.0 unsafe socket, UMEM-data, RX, and fill-ring calls are isolated in one private Linux module; the safe driver enforces single-UMEM descriptor provenance and retains user ownership until each descriptor is submitted";
pub const LICENSE_REVIEW: &str = "xsk-rs 0.8.0 is MIT licensed; Cargo.lock pins libxdp-sys 0.2.4+1.6.0, which builds vendored libxdp/libbpf and links system libelf/zlib under their respective terms";
pub const PROVENANCE_REVIEW: &str = "xsk-rs 0.8.0 registry checksum d1fef46e3505c5055082f52ada0a7f8e5dcaebdbb9eccf8e978c32382c159270; upstream tag v0.8.0 commit c0b110cd3b6763fdcfc41996b3cea8c9f259614f; Cargo.lock pins libxdp-sys 0.2.4+1.6.0 checksum 6098c8281e42ed6f46240af889297dae1e37f70ee505dd26fe5c7199563e4d86";
pub const MAINTENANCE_REVIEW: &str = "xsk-rs 0.8.0 was published 2025-09-17 and documents testing on Linux 6.5; privileged copy-mode lifecycle, independent unsafe-boundary audit, and fuzz evidence are still missing";
pub const BUILD_REVIEW: &str = "Linux-only xsk-rs 0.8.0 requires the native libxdp/libbpf build stack and privileges for socket/program activation; non-Linux targets neither compile nor link the dependency";
pub const MISSING_NATIVE_EVIDENCE: &str = "independent unsafe-boundary audit and fuzzing, privileged veth copy-mode lifecycle, qualified NIC/driver, zero-copy, authorized packet feed";
pub const COPY_DRIVER_EVIDENCE_ID: &str = "xsk_rs_0_8_0_af_xdp_copy_driver_review";

#[cfg(target_os = "linux")]
const NATIVE_RING_ENTRIES_MAXIMUM: usize = 4_096;
#[cfg(target_os = "linux")]
const NATIVE_UMEM_FRAME_BYTES: usize = 4_096;
#[cfg(target_os = "linux")]
const NATIVE_XDP_HEADROOM_BYTES: usize = 256;
#[cfg(target_os = "linux")]
const ETHERNET_HEADER_BYTES: usize = 14;
#[cfg(target_os = "linux")]
const AXIUSFLOW_EXPERIMENTAL_ETHERTYPE: [u8; 2] = [0x88, 0xb5];
#[cfg(target_os = "linux")]
const NATIVE_PACKET_BYTES_MAXIMUM: usize =
    NATIVE_UMEM_FRAME_BYTES - NATIVE_XDP_HEADROOM_BYTES - ETHERNET_HEADER_BYTES;
#[cfg(target_os = "linux")]
const RECEIVE_POLL_TIMEOUT_MILLIS: i32 = 100;

/// Bounded parameters for one fixed UMEM and queue.
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

    #[cfg(target_os = "linux")]
    fn validate_native_copy(&self) -> Result<(), AfXdpError> {
        if self.queue_id == u16::MAX {
            return Err(AfXdpError::QueueUnavailable(self.queue_id));
        }
        if !self.frame_count.get().is_power_of_two()
            || self.frame_count.get() > NATIVE_RING_ENTRIES_MAXIMUM
        {
            return Err(AfXdpError::RingSizeUnsupported(self.frame_count.get()));
        }
        if self.maximum_frame_bytes.get() > NATIVE_PACKET_BYTES_MAXIMUM {
            return Err(AfXdpError::FrameLimitUnsupported(
                self.maximum_frame_bytes.get(),
            ));
        }
        Ok(())
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
        native_dependency: if cfg!(target_os = "linux") {
            AfXdpEvidenceStatus::Present
        } else {
            AfXdpEvidenceStatus::NotSelected
        },
        copy_mode: AfXdpEvidenceStatus::NotExercised,
        zero_copy: AfXdpEvidenceStatus::Unverified,
        missing_evidence: MISSING_NATIVE_EVIDENCE,
    }
}

/// Explicitly unavailable adapter. It cannot silently use sockets or fixtures.
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

/// Real Linux `AF_XDP` copy-mode ingest with one fixed UMEM and fixed rings.
#[derive(Debug)]
pub struct AfXdpCopyDriver {
    config: AfXdpConfig,
    permit: ActivationPermit,
    capabilities: DriverCapabilities,
    lifecycle: DriverLifecycle,
    binding: Option<QueueBinding>,
    batch_outstanding: bool,
    overflow: OverflowReport,
    released_batches: u64,
    abandoned_batches: u64,
    recycle_failure: Option<String>,
    #[cfg(target_os = "linux")]
    native: Option<native::CopySocket>,
}

impl AfXdpCopyDriver {
    /// Creates an authorized copy-mode driver without opening a socket.
    ///
    /// Native resources are opened by [`IngestDriver::start`] after an exact queue bind.
    /// Non-Linux targets return [`AfXdpError::NativeIntegrationUnavailable`].
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported fixed-ring configuration, a readiness-policy
    /// rejection, or a non-Linux target.
    pub fn try_new(config: AfXdpConfig) -> Result<Self, AfXdpError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = config;
            return Err(AfXdpError::NativeIntegrationUnavailable);
        }
        #[cfg(target_os = "linux")]
        {
            config.validate_native_copy()?;
            let capabilities = DriverCapabilities {
                profile: PROFILE,
                supported_modes: vec![ActiveIngestMode::AfXdpCopy],
                receive_queues: config.queue_id.saturating_add(1),
                maximum_frame_bytes: config.maximum_frame_bytes.get(),
                timestamp_sources: vec![TimestampSource::SocketSoftware],
                zero_copy_verified: false,
            };
            let permit = ReadinessManifest::authorize_embedded(&ActivationRequest {
                profile: PROFILE,
                requested: ReadinessState::Implemented,
                active_mode: ActiveIngestMode::AfXdpCopy,
                evidence_id: COPY_DRIVER_EVIDENCE_ID,
                capabilities: &capabilities,
            })?;
            Ok(Self {
                config,
                permit,
                capabilities,
                lifecycle: DriverLifecycle::Created,
                binding: None,
                batch_outstanding: false,
                overflow: OverflowReport::default(),
                released_batches: 0,
                abandoned_batches: 0,
                recycle_failure: None,
                native: None,
            })
        }
    }

    #[must_use]
    pub const fn permit(&self) -> &ActivationPermit {
        &self.permit
    }

    fn finish_batch(&mut self, released: bool) {
        #[cfg(target_os = "linux")]
        if let Some(native) = self.native.as_mut()
            && let Err(error) = native.recycle_batch()
        {
            self.recycle_failure = Some(error);
        }
        self.batch_outstanding = false;
        if released {
            self.released_batches = self.released_batches.saturating_add(1);
        } else {
            self.abandoned_batches = self.abandoned_batches.saturating_add(1);
        }
    }
}

/// Batch that retains userspace ownership of its RX descriptors until release or drop.
#[derive(Debug)]
pub struct AfXdpCopyReceiveBatch<'driver> {
    driver: &'driver mut AfXdpCopyDriver,
    overflow: OverflowReport,
    #[cfg(target_os = "linux")]
    timestamp_unix_nanos: i64,
    released: bool,
}

impl ReceiveBatch for AfXdpCopyReceiveBatch<'_> {
    fn frame_count(&self) -> usize {
        #[cfg(target_os = "linux")]
        {
            self.driver
                .native
                .as_ref()
                .map_or(0, native::CopySocket::batch_len)
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }

    fn frame(&self, index: usize) -> Option<BorrowedFrame<'_>> {
        #[cfg(target_os = "linux")]
        {
            self.driver
                .native
                .as_ref()?
                .frame_bytes(index)
                .map(|bytes| BorrowedFrame {
                    bytes,
                    metadata: ReceiveMetadata {
                        receive_timestamp_unix_nanos: self.timestamp_unix_nanos,
                        timestamp_source: TimestampSource::SocketSoftware,
                        queue_id: self.driver.config.queue_id,
                    },
                })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = index;
            None
        }
    }

    fn overflow(&self) -> OverflowReport {
        self.overflow
    }

    fn release(mut self) {
        self.driver.finish_batch(true);
        self.released = true;
    }
}

impl Drop for AfXdpCopyReceiveBatch<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.driver.finish_batch(false);
        }
    }
}

impl IngestDriver for AfXdpCopyDriver {
    type Error = AfXdpError;
    type Batch<'driver> = AfXdpCopyReceiveBatch<'driver>;

    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    fn bind_queue(&mut self, binding: QueueBinding) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(AfXdpError::BatchOutstanding);
        }
        if self.lifecycle != DriverLifecycle::Created {
            return Err(AfXdpError::InvalidLifecycle {
                expected: DriverLifecycle::Created,
                actual: self.lifecycle,
            });
        }
        if binding.queue_id != self.config.queue_id {
            return Err(AfXdpError::QueueUnavailable(binding.queue_id));
        }
        if binding.maximum_batch_items.get() > self.config.frame_count.get() {
            return Err(AfXdpError::BatchLimitUnsupported(
                binding.maximum_batch_items.get(),
            ));
        }
        if binding.maximum_frame_bytes != self.config.maximum_frame_bytes {
            return Err(AfXdpError::FrameLimitUnsupported(
                binding.maximum_frame_bytes.get(),
            ));
        }
        self.binding = Some(binding);
        self.lifecycle = DriverLifecycle::Bound;
        Ok(())
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        if self.lifecycle != DriverLifecycle::Bound {
            return Err(AfXdpError::InvalidLifecycle {
                expected: DriverLifecycle::Bound,
                actual: self.lifecycle,
            });
        }
        let binding = self.binding.ok_or(AfXdpError::QueueNotBound)?;
        #[cfg(target_os = "linux")]
        {
            let native =
                native::CopySocket::open(&self.config, binding).map_err(AfXdpError::NativeOpen)?;
            self.native = Some(native);
            self.lifecycle = DriverLifecycle::Running;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = binding;
            Err(AfXdpError::NativeIntegrationUnavailable)
        }
    }

    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error> {
        if self.lifecycle != DriverLifecycle::Running {
            return Err(AfXdpError::InvalidLifecycle {
                expected: DriverLifecycle::Running,
                actual: self.lifecycle,
            });
        }
        if self.batch_outstanding {
            return Err(AfXdpError::BatchOutstanding);
        }
        if let Some(error) = self.recycle_failure.take() {
            return Err(AfXdpError::DescriptorRecycle(error));
        }
        let binding = self.binding.ok_or(AfXdpError::QueueNotBound)?;
        #[cfg(target_os = "linux")]
        {
            let outcome = self
                .native
                .as_mut()
                .ok_or_else(|| AfXdpError::NativeReceive("native socket is not open".to_string()))?
                .receive(
                    binding.maximum_batch_items.get(),
                    binding.maximum_frame_bytes.get(),
                )
                .map_err(AfXdpError::NativeReceive)?;
            self.overflow.dropped_frames = self
                .overflow
                .dropped_frames
                .saturating_add(outcome.dropped_frames);
            self.overflow.dropped_bytes = self
                .overflow
                .dropped_bytes
                .saturating_add(outcome.dropped_bytes);
            self.batch_outstanding = true;
            let overflow = self.overflow;
            Ok(AfXdpCopyReceiveBatch {
                driver: self,
                overflow,
                timestamp_unix_nanos: unix_timestamp_nanos(),
                released: false,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = binding;
            Err(AfXdpError::NativeIntegrationUnavailable)
        }
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(AfXdpError::BatchOutstanding);
        }
        if self.lifecycle != DriverLifecycle::Running {
            return Err(AfXdpError::InvalidLifecycle {
                expected: DriverLifecycle::Running,
                actual: self.lifecycle,
            });
        }
        if let Some(error) = self.recycle_failure.take() {
            return Err(AfXdpError::DescriptorRecycle(error));
        }
        #[cfg(target_os = "linux")]
        if let Some(native) = self.native.as_mut() {
            native
                .flush_recycle()
                .map_err(AfXdpError::DescriptorRecycle)?;
        }
        #[cfg(target_os = "linux")]
        {
            self.native = None;
        }
        self.lifecycle = DriverLifecycle::Stopped;
        Ok(())
    }

    fn health(&self) -> DriverHealth {
        DriverHealth {
            active_mode: ActiveIngestMode::AfXdpCopy,
            lifecycle: self.lifecycle,
            queue_id: self.config.queue_id,
            queued_frames: {
                #[cfg(target_os = "linux")]
                {
                    self.native
                        .as_ref()
                        .map_or(0, native::CopySocket::owned_len)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    0
                }
            },
            overflow: self.overflow,
            released_batches: self.released_batches,
            abandoned_batches: self.abandoned_batches,
        }
    }
}

#[cfg(target_os = "linux")]
fn unix_timestamp_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

#[derive(Debug)]
pub enum AfXdpError {
    EmptyInterfaceName,
    FrameLimitUnsupported(usize),
    ReceiveStorageOverflow,
    RingSizeUnsupported(usize),
    QueueUnavailable(u16),
    BatchLimitUnsupported(usize),
    QueueNotBound,
    BatchOutstanding,
    InvalidLifecycle {
        expected: DriverLifecycle,
        actual: DriverLifecycle,
    },
    NativeIntegrationUnavailable,
    Readiness(ReadinessError),
    NativeOpen(String),
    NativeReceive(String),
    DescriptorRecycle(String),
}

impl From<ReadinessError> for AfXdpError {
    fn from(error: ReadinessError) -> Self {
        Self::Readiness(error)
    }
}

impl fmt::Display for AfXdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "AF_XDP ingest error: {self:?}")
    }
}

impl Error for AfXdpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Readiness(error) => Some(error),
            _ => None,
        }
    }
}

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

#[cfg(target_os = "linux")]
mod native {
    #![allow(unsafe_code)]

    use super::{
        AXIUSFLOW_EXPERIMENTAL_ETHERTYPE, AfXdpConfig, ETHERNET_HEADER_BYTES,
        NATIVE_UMEM_FRAME_BYTES, QueueBinding, RECEIVE_POLL_TIMEOUT_MILLIS,
    };
    use std::num::NonZeroU32;
    use xsk_rs::{
        CompQueue, FillQueue, FrameDesc, RxQueue, TxQueue, Umem,
        config::{BindFlags, FrameSize, Interface, QueueSize, SocketConfig, UmemConfig, XdpFlags},
        socket::Socket,
    };

    #[derive(Debug)]
    pub(super) struct ReceiveOutcome {
        pub(super) dropped_frames: u64,
        pub(super) dropped_bytes: u64,
    }

    #[derive(Debug)]
    pub(super) struct CopySocket {
        umem: Umem,
        _tx: TxQueue,
        rx: RxQueue,
        fill: FillQueue,
        _completion: CompQueue,
        scratch: Vec<FrameDesc>,
        in_flight: Vec<FrameDesc>,
        pending_recycle: Vec<FrameDesc>,
    }

    impl CopySocket {
        pub(super) fn open(config: &AfXdpConfig, binding: QueueBinding) -> Result<Self, String> {
            let frame_count = NonZeroU32::new(
                u32::try_from(config.frame_count.get())
                    .map_err(|_| "frame count does not fit the native API".to_string())?,
            )
            .ok_or_else(|| "frame count cannot be zero".to_string())?;
            let ring_size = QueueSize::new(frame_count.get()).map_err(|error| error.to_string())?;
            let frame_size =
                FrameSize::new(u32::try_from(NATIVE_UMEM_FRAME_BYTES).map_err(|_| {
                    "fixed UMEM frame size does not fit the native API".to_string()
                })?)
                .map_err(|error| error.to_string())?;
            let umem_config = UmemConfig::builder()
                .frame_size(frame_size)
                .fill_queue_size(ring_size)
                .comp_queue_size(ring_size)
                .build()
                .map_err(|error| error.to_string())?;
            if binding.maximum_frame_bytes.get()
                > usize::try_from(umem_config.mtu()).unwrap_or(usize::MAX)
            {
                return Err("configured packet limit exceeds the fixed UMEM MTU".to_string());
            }
            let (umem, descriptors) =
                Umem::new(umem_config, frame_count, false).map_err(|error| error.to_string())?;
            let socket_config = SocketConfig::builder()
                .rx_queue_size(ring_size)
                .tx_queue_size(ring_size)
                .xdp_flags(XdpFlags::XDP_FLAGS_SKB_MODE)
                .bind_flags(BindFlags::XDP_COPY)
                .build();
            let interface: Interface = config
                .interface_name
                .parse()
                .map_err(|error: std::ffi::NulError| error.to_string())?;
            // SAFETY: `umem` is newly allocated, is not shared with another socket, and
            // remains owned by this `CopySocket` for longer than every returned queue.
            let (tx, rx, queues) = unsafe {
                Socket::new(socket_config, &umem, &interface, u32::from(config.queue_id))
            }
            .map_err(|error| error.to_string())?;
            let (mut fill, completion) = queues.ok_or_else(|| {
                "new non-shared UMEM did not return fill/completion rings".to_string()
            })?;
            // SAFETY: every descriptor was created by this exact `umem`, descriptors are
            // unique, and userspace has not submitted or otherwise aliased any of them.
            let submitted = unsafe { fill.produce(&descriptors) };
            if submitted != descriptors.len() {
                return Err(format!(
                    "fill ring accepted {submitted} of {} initial descriptors",
                    descriptors.len()
                ));
            }
            Ok(Self {
                umem,
                _tx: tx,
                rx,
                fill,
                _completion: completion,
                scratch: vec![FrameDesc::default(); binding.maximum_batch_items.get()],
                in_flight: Vec::with_capacity(binding.maximum_batch_items.get()),
                pending_recycle: Vec::with_capacity(config.frame_count.get()),
            })
        }

        pub(super) fn receive(
            &mut self,
            maximum_batch_items: usize,
            maximum_frame_bytes: usize,
        ) -> Result<ReceiveOutcome, String> {
            if !self.in_flight.is_empty() {
                return Err("receive attempted while descriptors remain in flight".to_string());
            }
            self.flush_recycle()?;
            let scratch = self
                .scratch
                .get_mut(..maximum_batch_items)
                .ok_or_else(|| "batch limit exceeds fixed receive storage".to_string())?;
            // SAFETY: `scratch` is only descriptor output storage. `rx` and every
            // descriptor it returns are tied to this `umem`; consumed descriptors are
            // moved to either `in_flight` or `pending_recycle` before another poll.
            let received = unsafe {
                self.rx
                    .poll_and_consume(scratch, RECEIVE_POLL_TIMEOUT_MILLIS)
            }
            .map_err(|error| error.to_string())?;
            let mut dropped_frames = 0_u64;
            let mut dropped_bytes = 0_u64;
            for descriptor in scratch.iter().copied().take(received) {
                // SAFETY: this descriptor was just consumed from this socket's RX
                // ring and has not been returned to the fill ring. The immutable
                // view is dropped before descriptor ownership is moved below.
                let packet = unsafe { self.umem.data(&descriptor) }.contents();
                let payload_length = packet
                    .get(12..14)
                    .filter(|ether_type| **ether_type == AXIUSFLOW_EXPERIMENTAL_ETHERTYPE)
                    .and_then(|_| packet.len().checked_sub(ETHERNET_HEADER_BYTES));
                if payload_length.is_none_or(|length| length > maximum_frame_bytes) {
                    dropped_frames = dropped_frames.saturating_add(1);
                    dropped_bytes = dropped_bytes
                        .saturating_add(u64::try_from(packet.len()).unwrap_or(u64::MAX));
                    self.pending_recycle.push(descriptor);
                } else {
                    self.in_flight.push(descriptor);
                }
            }
            if let Err(error) = self.flush_recycle() {
                self.pending_recycle.append(&mut self.in_flight);
                return Err(error);
            }
            Ok(ReceiveOutcome {
                dropped_frames,
                dropped_bytes,
            })
        }

        pub(super) fn batch_len(&self) -> usize {
            self.in_flight.len()
        }

        pub(super) fn owned_len(&self) -> usize {
            self.in_flight
                .len()
                .saturating_add(self.pending_recycle.len())
        }

        pub(super) fn frame_bytes(&self, index: usize) -> Option<&[u8]> {
            let descriptor = self.in_flight.get(index)?;
            // SAFETY: the descriptor originated from this socket's RX ring and remains
            // in `in_flight`, so userspace exclusively owns it and it has not been
            // resubmitted to the fill ring. Only an immutable view is returned.
            let packet = unsafe { self.umem.data(descriptor) }.contents();
            if packet.get(12..14)? != AXIUSFLOW_EXPERIMENTAL_ETHERTYPE {
                return None;
            }
            packet.get(ETHERNET_HEADER_BYTES..)
        }

        pub(super) fn recycle_batch(&mut self) -> Result<(), String> {
            self.pending_recycle.append(&mut self.in_flight);
            self.flush_recycle()
        }

        pub(super) fn flush_recycle(&mut self) -> Result<(), String> {
            if self.pending_recycle.is_empty() {
                return Ok(());
            }
            // SAFETY: every pending descriptor originated from this socket's RX ring,
            // belongs to this `umem`, is uniquely held by `pending_recycle`, and has not
            // been submitted since userspace regained ownership from RX.
            let submitted = unsafe { self.fill.produce(&self.pending_recycle) };
            if submitted > 0 {
                self.pending_recycle.drain(..submitted);
            }
            if self.pending_recycle.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "fill ring retained {} descriptors after recycle",
                    self.pending_recycle.len()
                ))
            }
        }
    }
}
