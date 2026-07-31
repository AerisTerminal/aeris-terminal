//! Transport-neutral bounded ingest contracts and runtime readiness enforcement.

use core::fmt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};
use std::error::Error;
use std::num::NonZeroUsize;

/// Foundational network profiles. Accelerated profiles never silently fall back.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestProfile {
    PortableSocket,
    TunedLinuxSocket,
    LinuxAfXdp,
    LinuxDpdk,
}

/// Ordered evidence state for one ingest profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    ContractOnly,
    Implemented,
    FixtureValidated,
    HardwareValidated,
    ProviderCertified,
    ProductionEnabled,
}

/// Runtime mode reported separately from requested profile and readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveIngestMode {
    Unavailable,
    SoftwareFixture,
    PortableSocket,
    TunedLinuxSocket,
    AfXdpCopy,
    AfXdpZeroCopy,
    DpdkPollMode,
}

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

fn mode_matches_profile(profile: IngestProfile, mode: ActiveIngestMode) -> bool {
    matches!(
        (profile, mode),
        (
            IngestProfile::PortableSocket,
            ActiveIngestMode::PortableSocket
        ) | (
            IngestProfile::TunedLinuxSocket,
            ActiveIngestMode::TunedLinuxSocket
        ) | (
            IngestProfile::LinuxAfXdp,
            ActiveIngestMode::AfXdpCopy | ActiveIngestMode::AfXdpZeroCopy
        ) | (IngestProfile::LinuxDpdk, ActiveIngestMode::DpdkPollMode)
    )
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

/// Receive timestamp clock and acquisition source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimestampSource {
    SocketSoftware,
    KernelSoftware,
    NicHardware,
    Provider,
}

/// Metadata retained while receive memory is valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveMetadata {
    pub receive_timestamp_unix_nanos: i64,
    pub timestamp_source: TimestampSource,
    pub queue_id: u16,
}

/// One frame borrowing bytes from its receive batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedFrame<'batch> {
    pub bytes: &'batch [u8],
    pub metadata: ReceiveMetadata,
}

/// Explicit overflow evidence from a bounded driver queue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OverflowReport {
    pub dropped_frames: u64,
    pub dropped_bytes: u64,
    pub first_dropped_sequence_hint: Option<u64>,
}

/// A bounded batch that owns receive memory and only lends frame bytes.
///
/// Provider framing and decoding must finish before [`ReceiveBatch::release`].
pub trait ReceiveBatch {
    fn frame_count(&self) -> usize;
    fn frame(&self, index: usize) -> Option<BorrowedFrame<'_>>;
    fn overflow(&self) -> OverflowReport;
    fn release(self);
}

/// Queue selection without exposing socket, UMEM, or mbuf handles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueBinding {
    pub queue_id: u16,
    pub maximum_batch_items: NonZeroUsize,
    pub maximum_frame_bytes: NonZeroUsize,
}

/// Capabilities discovered before queue activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriverCapabilities {
    pub profile: IngestProfile,
    pub supported_modes: Vec<ActiveIngestMode>,
    pub receive_queues: u16,
    pub maximum_frame_bytes: usize,
    pub timestamp_sources: Vec<TimestampSource>,
    pub zero_copy_verified: bool,
}

/// Capabilities of the deterministic lifecycle harness, not a native profile adapter.
#[must_use]
pub fn software_fixture_capabilities(profile: IngestProfile) -> DriverCapabilities {
    DriverCapabilities {
        profile,
        supported_modes: vec![ActiveIngestMode::SoftwareFixture],
        receive_queues: 1,
        maximum_frame_bytes: 65_535,
        timestamp_sources: vec![TimestampSource::SocketSoftware],
        zero_copy_verified: false,
    }
}

/// Explicit ingest lifecycle shared by every adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverLifecycle {
    Created,
    Bound,
    Running,
    Stopped,
    Unsupported,
}

/// Bounded ingest health report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriverHealth {
    pub active_mode: ActiveIngestMode,
    pub lifecycle: DriverLifecycle,
    pub queue_id: u16,
    pub queued_frames: usize,
    pub overflow: OverflowReport,
    pub released_batches: u64,
    pub abandoned_batches: u64,
}

/// Shared Stage 1 ingest port implemented by portable and accelerated adapters.
pub trait IngestDriver {
    type Error: Error + Send + Sync + 'static;
    type Batch<'driver>: ReceiveBatch
    where
        Self: 'driver;

    fn capabilities(&self) -> &DriverCapabilities;

    /// Binds one bounded receive queue and its memory limits.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue or requested limits are unsupported.
    fn bind_queue(&mut self, binding: QueueBinding) -> Result<(), Self::Error>;

    /// Starts receive processing after queue binding.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when startup is unsupported or incomplete.
    fn start(&mut self) -> Result<(), Self::Error>;

    /// Borrows the next bounded receive batch.
    ///
    /// # Errors
    ///
    /// Returns an adapter error for receive or lifecycle failures.
    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error>;

    /// Stops receive processing after all borrowed batches have been released.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when a batch is still outstanding or teardown fails.
    fn shutdown(&mut self) -> Result<(), Self::Error>;

    fn health(&self) -> DriverHealth;
}

/// Owned fixture input used to prove the exact borrowed/release lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureFrame {
    pub bytes: Vec<u8>,
    pub metadata: ReceiveMetadata,
}

/// Deterministic software lifecycle driver used for contract conformance only.
/// It requires a readiness permit, reuses driver-owned receive slots, and never
/// claims socket, kernel-bypass, hardware, or provider behavior.
#[derive(Debug)]
pub struct FixtureIngestDriver {
    permit: ActivationPermit,
    capabilities: DriverCapabilities,
    lifecycle: DriverLifecycle,
    binding: Option<QueueBinding>,
    pending: VecDeque<FixtureFrame>,
    in_flight: Vec<FixtureFrame>,
    batch_outstanding: bool,
    overflow: OverflowReport,
    released_batches: u64,
    abandoned_batches: u64,
}

impl FixtureIngestDriver {
    /// Creates a driver only from evidence authorized for the same capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error when permit profile/mode does not match the adapter.
    pub fn try_new(
        permit: ActivationPermit,
        capabilities: DriverCapabilities,
        frames: Vec<FixtureFrame>,
    ) -> Result<Self, ReadinessError> {
        if permit.profile() != capabilities.profile {
            return Err(ReadinessError::CapabilityProfileMismatch {
                requested: permit.profile(),
                reported: capabilities.profile,
            });
        }
        if permit.active_mode() != ActiveIngestMode::SoftwareFixture
            || !capabilities.supported_modes.contains(&permit.active_mode())
        {
            return Err(ReadinessError::ModeUnavailable(permit.active_mode()));
        }
        Ok(Self {
            permit,
            capabilities,
            lifecycle: DriverLifecycle::Created,
            binding: None,
            pending: frames.into(),
            in_flight: Vec::new(),
            batch_outstanding: false,
            overflow: OverflowReport::default(),
            released_batches: 0,
            abandoned_batches: 0,
        })
    }

    #[must_use]
    pub const fn permit(&self) -> &ActivationPermit {
        &self.permit
    }

    fn finish_batch(&mut self, released: bool) {
        self.in_flight.clear();
        self.batch_outstanding = false;
        if released {
            self.released_batches = self.released_batches.saturating_add(1);
        } else {
            self.abandoned_batches = self.abandoned_batches.saturating_add(1);
        }
    }
}

/// Batch borrowing reusable driver-owned receive slots.
#[derive(Debug)]
pub struct FixtureReceiveBatch<'driver> {
    driver: &'driver mut FixtureIngestDriver,
    overflow: OverflowReport,
    released: bool,
}

impl ReceiveBatch for FixtureReceiveBatch<'_> {
    fn frame_count(&self) -> usize {
        self.driver.in_flight.len()
    }

    fn frame(&self, index: usize) -> Option<BorrowedFrame<'_>> {
        self.driver.in_flight.get(index).map(|frame| BorrowedFrame {
            bytes: &frame.bytes,
            metadata: frame.metadata,
        })
    }

    fn overflow(&self) -> OverflowReport {
        self.overflow
    }

    fn release(mut self) {
        self.driver.finish_batch(true);
        self.released = true;
    }
}

impl Drop for FixtureReceiveBatch<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.driver.finish_batch(false);
        }
    }
}

impl IngestDriver for FixtureIngestDriver {
    type Error = FixtureDriverError;
    type Batch<'driver> = FixtureReceiveBatch<'driver>;

    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    fn bind_queue(&mut self, binding: QueueBinding) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(FixtureDriverError::BatchOutstanding);
        }
        if self.lifecycle == DriverLifecycle::Running {
            return Err(FixtureDriverError::InvalidLifecycle {
                expected: DriverLifecycle::Created,
                actual: self.lifecycle,
            });
        }
        if binding.queue_id >= self.capabilities.receive_queues {
            return Err(FixtureDriverError::QueueUnavailable(binding.queue_id));
        }
        if binding.maximum_frame_bytes.get() > self.capabilities.maximum_frame_bytes {
            return Err(FixtureDriverError::FrameLimitUnsupported(
                binding.maximum_frame_bytes.get(),
            ));
        }
        self.binding = Some(binding);
        self.lifecycle = DriverLifecycle::Bound;
        Ok(())
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        if self.lifecycle != DriverLifecycle::Bound {
            return Err(FixtureDriverError::InvalidLifecycle {
                expected: DriverLifecycle::Bound,
                actual: self.lifecycle,
            });
        }
        self.lifecycle = DriverLifecycle::Running;
        Ok(())
    }

    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error> {
        if self.lifecycle != DriverLifecycle::Running {
            return Err(FixtureDriverError::InvalidLifecycle {
                expected: DriverLifecycle::Running,
                actual: self.lifecycle,
            });
        }
        if self.batch_outstanding {
            return Err(FixtureDriverError::BatchOutstanding);
        }
        let binding = self.binding.ok_or(FixtureDriverError::QueueNotBound)?;
        self.in_flight.clear();
        while self.in_flight.len() < binding.maximum_batch_items.get() {
            let Some(frame) = self.pending.pop_front() else {
                break;
            };
            if frame.bytes.len() > binding.maximum_frame_bytes.get() {
                self.overflow.dropped_frames = self.overflow.dropped_frames.saturating_add(1);
                self.overflow.dropped_bytes = self
                    .overflow
                    .dropped_bytes
                    .saturating_add(u64::try_from(frame.bytes.len()).unwrap_or(u64::MAX));
                continue;
            }
            self.in_flight.push(frame);
        }
        self.batch_outstanding = true;
        let overflow = self.overflow;
        Ok(FixtureReceiveBatch {
            driver: self,
            overflow,
            released: false,
        })
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(FixtureDriverError::BatchOutstanding);
        }
        if self.lifecycle != DriverLifecycle::Running {
            return Err(FixtureDriverError::InvalidLifecycle {
                expected: DriverLifecycle::Running,
                actual: self.lifecycle,
            });
        }
        self.lifecycle = DriverLifecycle::Stopped;
        Ok(())
    }

    fn health(&self) -> DriverHealth {
        DriverHealth {
            active_mode: self.permit.active_mode(),
            lifecycle: self.lifecycle,
            queue_id: self.binding.map_or(0, |binding| binding.queue_id),
            queued_frames: self.pending.len(),
            overflow: self.overflow,
            released_batches: self.released_batches,
            abandoned_batches: self.abandoned_batches,
        }
    }
}

#[derive(Debug)]
pub enum FixtureDriverError {
    QueueNotBound,
    BatchOutstanding,
    QueueUnavailable(u16),
    FrameLimitUnsupported(usize),
    InvalidLifecycle {
        expected: DriverLifecycle,
        actual: DriverLifecycle,
    },
}

impl fmt::Display for FixtureDriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "fixture ingest driver error: {self:?}")
    }
}

impl Error for FixtureDriverError {}

#[derive(Debug)]
pub enum ReadinessError {
    InvalidJson(serde_json::Error),
    UnsupportedSchemaVersion(u32),
    DuplicateProfile(IngestProfile),
    MissingProfile(IngestProfile),
    MissingEvidence(IngestProfile),
    CapabilityProfileMismatch {
        requested: IngestProfile,
        reported: IngestProfile,
    },
    ModeUnavailable(ActiveIngestMode),
    ModeProfileMismatch {
        profile: IngestProfile,
        mode: ActiveIngestMode,
    },
    FixtureCannotProveReadiness(ReadinessState),
    UnreviewedEvidence(String),
    ClaimExceedsEvidence {
        profile: IngestProfile,
        requested: ReadinessState,
        maximum: ReadinessState,
    },
}

impl fmt::Display for ReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ingest readiness rejected: {self:?}")
    }
}

impl Error for ReadinessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            _ => None,
        }
    }
}

/// Bytes in the network-order length prefix used by binary client stream frames.
pub const BINARY_FRAME_LENGTH_BYTES: usize = u32::BITS as usize / 8;

/// Bounded incremental decoder for fragmented or coalesced length-prefixed binary frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedBinaryFrameDecoder {
    maximum_frame_bytes: NonZeroUsize,
    maximum_buffered_bytes: NonZeroUsize,
    buffered: Vec<u8>,
}

impl BoundedBinaryFrameDecoder {
    /// Creates a decoder whose total partial-frame storage is explicitly bounded.
    ///
    /// # Errors
    ///
    /// Returns an error if the buffer cannot hold one maximum-sized framed payload.
    pub fn try_new(
        maximum_frame_bytes: NonZeroUsize,
        maximum_buffered_bytes: NonZeroUsize,
    ) -> Result<Self, BinaryFrameError> {
        let required = maximum_frame_bytes
            .get()
            .checked_add(BINARY_FRAME_LENGTH_BYTES)
            .ok_or(BinaryFrameError::FrameLengthOverflow)?;
        if maximum_buffered_bytes.get() < required {
            return Err(BinaryFrameError::BufferTooSmall {
                required,
                actual: maximum_buffered_bytes.get(),
            });
        }
        Ok(Self {
            maximum_frame_bytes,
            maximum_buffered_bytes,
            buffered: Vec::with_capacity(required),
        })
    }

    /// Accepts one bounded transport chunk and returns every complete frame it contains.
    ///
    /// Partial frames remain bounded until a later chunk completes them. Any invalid
    /// length clears partial state so callers must explicitly restart stream semantics.
    ///
    /// # Errors
    ///
    /// Returns an error for zero/oversized frames or buffer-capacity overflow.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, BinaryFrameError> {
        let next_size = self
            .buffered
            .len()
            .checked_add(chunk.len())
            .ok_or(BinaryFrameError::FrameLengthOverflow)?;
        if next_size > self.maximum_buffered_bytes.get() {
            self.buffered.clear();
            return Err(BinaryFrameError::BufferLimitExceeded {
                requested: next_size,
                maximum: self.maximum_buffered_bytes.get(),
            });
        }
        self.buffered.extend_from_slice(chunk);

        let mut frames = Vec::new();
        let mut consumed = 0;
        while self.buffered.len().saturating_sub(consumed) >= BINARY_FRAME_LENGTH_BYTES {
            let prefix: [u8; BINARY_FRAME_LENGTH_BYTES] = self
                .buffered
                .get(consumed..consumed + BINARY_FRAME_LENGTH_BYTES)
                .ok_or(BinaryFrameError::FrameLengthOverflow)?
                .try_into()
                .map_err(|_| BinaryFrameError::FrameLengthOverflow)?;
            let frame_length = usize::try_from(u32::from_be_bytes(prefix))
                .map_err(|_| BinaryFrameError::FrameLengthOverflow)?;
            if frame_length == 0 {
                self.buffered.clear();
                return Err(BinaryFrameError::EmptyFrame);
            }
            if frame_length > self.maximum_frame_bytes.get() {
                self.buffered.clear();
                return Err(BinaryFrameError::FrameLimitExceeded {
                    requested: frame_length,
                    maximum: self.maximum_frame_bytes.get(),
                });
            }
            let total_frame_bytes = BINARY_FRAME_LENGTH_BYTES
                .checked_add(frame_length)
                .ok_or(BinaryFrameError::FrameLengthOverflow)?;
            if self.buffered.len().saturating_sub(consumed) < total_frame_bytes {
                break;
            }
            let payload_start = consumed + BINARY_FRAME_LENGTH_BYTES;
            frames.push(self.buffered[payload_start..payload_start + frame_length].to_vec());
            consumed = consumed
                .checked_add(total_frame_bytes)
                .ok_or(BinaryFrameError::FrameLengthOverflow)?;
        }
        if consumed > 0 {
            self.buffered.drain(..consumed);
        }
        Ok(frames)
    }

    /// Clears any partial frame after reconnect or semantic recovery.
    pub fn reset(&mut self) {
        self.buffered.clear();
    }

    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.buffered.len()
    }
}

/// Encodes one non-empty payload with a network-order length prefix and a hard size bound.
///
/// # Errors
///
/// Returns an error for an empty, oversized, or unrepresentable payload.
pub fn encode_binary_frame(
    payload: &[u8],
    maximum_frame_bytes: NonZeroUsize,
) -> Result<Vec<u8>, BinaryFrameError> {
    if payload.is_empty() {
        return Err(BinaryFrameError::EmptyFrame);
    }
    if payload.len() > maximum_frame_bytes.get() {
        return Err(BinaryFrameError::FrameLimitExceeded {
            requested: payload.len(),
            maximum: maximum_frame_bytes.get(),
        });
    }
    let frame_length =
        u32::try_from(payload.len()).map_err(|_| BinaryFrameError::FrameLimitExceeded {
            requested: payload.len(),
            maximum: u32::MAX as usize,
        })?;
    let capacity = BINARY_FRAME_LENGTH_BYTES
        .checked_add(payload.len())
        .ok_or(BinaryFrameError::FrameLengthOverflow)?;
    let mut framed = Vec::with_capacity(capacity);
    framed.extend_from_slice(&frame_length.to_be_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

/// Structural failures in the bounded binary client framing layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryFrameError {
    EmptyFrame,
    FrameLengthOverflow,
    FrameLimitExceeded { requested: usize, maximum: usize },
    BufferLimitExceeded { requested: usize, maximum: usize },
    BufferTooSmall { required: usize, actual: usize },
}

impl fmt::Display for BinaryFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "binary frame rejected: {self:?}")
    }
}

impl Error for BinaryFrameError {}
