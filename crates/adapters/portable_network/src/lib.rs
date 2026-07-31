//! Portable bounded UDP ingest adapter and deterministic fixture target.
//!
//! The native driver owns a fixed set of receive slots selected at queue binding.
//! Borrowed datagram bytes remain valid only until the returned batch is released.

use axiusflow_transport::{
    ActivationPermit, ActivationRequest, ActiveIngestMode, BorrowedFrame, DriverCapabilities,
    DriverHealth, DriverLifecycle, FixtureFrame, FixtureIngestDriver, IngestDriver, IngestProfile,
    OverflowReport, QueueBinding, ReadinessError, ReadinessManifest, ReadinessState, ReceiveBatch,
    ReceiveMetadata, TimestampSource, software_fixture_capabilities,
};
use core::fmt;
use std::{
    error::Error,
    io,
    net::{SocketAddr, UdpSocket},
    num::NonZeroUsize,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const PROFILE: IngestProfile = IngestProfile::PortableSocket;
const UDP_MAXIMUM_PAYLOAD_BYTES: usize = 65_507;

/// Bounded portable socket configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortableSocketConfig {
    pub bind_address: SocketAddr,
    pub poll_timeout: Duration,
    pub idle_poll_interval: Duration,
    pub maximum_batch_items: NonZeroUsize,
    pub maximum_frame_bytes: NonZeroUsize,
}

impl PortableSocketConfig {
    /// Creates a validated bounded socket configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized UDP payload, zero timeout, an idle interval
    /// above the timeout, or receive storage whose size cannot be represented.
    pub fn try_new(
        bind_address: SocketAddr,
        poll_timeout: Duration,
        idle_poll_interval: Duration,
        maximum_batch_items: NonZeroUsize,
        maximum_frame_bytes: NonZeroUsize,
    ) -> Result<Self, PortableSocketError> {
        if poll_timeout.is_zero() {
            return Err(PortableSocketError::ZeroPollTimeout);
        }
        if idle_poll_interval.is_zero() || idle_poll_interval > poll_timeout {
            return Err(PortableSocketError::InvalidIdlePollInterval);
        }
        if maximum_frame_bytes.get() > UDP_MAXIMUM_PAYLOAD_BYTES {
            return Err(PortableSocketError::FrameLimitUnsupported(
                maximum_frame_bytes.get(),
            ));
        }
        maximum_batch_items
            .get()
            .checked_mul(maximum_frame_bytes.get().saturating_add(1))
            .ok_or(PortableSocketError::ReceiveStorageOverflow)?;
        Ok(Self {
            bind_address,
            poll_timeout,
            idle_poll_interval,
            maximum_batch_items,
            maximum_frame_bytes,
        })
    }

    /// Deterministic loopback defaults used by cross-platform conformance.
    ///
    /// # Errors
    ///
    /// Returns an error only if the built-in bounds cease to validate.
    pub fn loopback() -> Result<Self, PortableSocketError> {
        Self::try_new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Duration::from_millis(100),
            Duration::from_millis(1),
            NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
        )
    }
}

/// Real portable socket driver. It never delegates receive behavior to the fixture driver.
#[derive(Debug)]
pub struct PortableSocketDriver {
    permit: ActivationPermit,
    capabilities: DriverCapabilities,
    config: PortableSocketConfig,
    lifecycle: DriverLifecycle,
    binding: Option<QueueBinding>,
    socket: Option<UdpSocket>,
    slots: Vec<Vec<u8>>,
    lengths: Vec<usize>,
    metadata: Vec<ReceiveMetadata>,
    batch_outstanding: bool,
    overflow: OverflowReport,
    released_batches: u64,
    abandoned_batches: u64,
}

impl PortableSocketDriver {
    /// Creates a dormant driver from repository-reviewed portable socket evidence.
    ///
    /// # Errors
    ///
    /// Returns an error if the embedded readiness manifest does not authorize the
    /// implemented portable mode.
    pub fn try_new(config: PortableSocketConfig) -> Result<Self, PortableSocketError> {
        let capabilities = DriverCapabilities {
            profile: PROFILE,
            supported_modes: vec![ActiveIngestMode::PortableSocket],
            receive_queues: 1,
            maximum_frame_bytes: config.maximum_frame_bytes.get(),
            timestamp_sources: vec![TimestampSource::SocketSoftware],
            zero_copy_verified: false,
        };
        let permit = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: PROFILE,
            requested: ReadinessState::Implemented,
            active_mode: ActiveIngestMode::PortableSocket,
            evidence_id: "portable_socket_loopback",
            capabilities: &capabilities,
        })?;
        Ok(Self {
            permit,
            capabilities,
            config,
            lifecycle: DriverLifecycle::Created,
            binding: None,
            socket: None,
            slots: Vec::new(),
            lengths: Vec::new(),
            metadata: Vec::new(),
            batch_outstanding: false,
            overflow: OverflowReport::default(),
            released_batches: 0,
            abandoned_batches: 0,
        })
    }

    /// Returns the bound local endpoint after startup.
    ///
    /// # Errors
    ///
    /// Returns an error before startup or when the operating system rejects the query.
    pub fn local_addr(&self) -> Result<SocketAddr, PortableSocketError> {
        self.socket
            .as_ref()
            .ok_or(PortableSocketError::NotStarted)?
            .local_addr()
            .map_err(PortableSocketError::Io)
    }

    #[must_use]
    pub const fn permit(&self) -> &ActivationPermit {
        &self.permit
    }

    fn finish_batch(&mut self, released: bool) {
        self.batch_outstanding = false;
        self.lengths.clear();
        self.metadata.clear();
        if released {
            self.released_batches = self.released_batches.saturating_add(1);
        } else {
            self.abandoned_batches = self.abandoned_batches.saturating_add(1);
        }
    }

    fn receive_timestamp() -> i64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        i64::try_from(nanos).unwrap_or(i64::MAX)
    }
}

/// One borrowed portable receive batch.
#[derive(Debug)]
pub struct PortableReceiveBatch<'driver> {
    driver: &'driver mut PortableSocketDriver,
    overflow: OverflowReport,
    released: bool,
}

impl ReceiveBatch for PortableReceiveBatch<'_> {
    fn frame_count(&self) -> usize {
        self.driver.lengths.len()
    }

    fn frame(&self, index: usize) -> Option<BorrowedFrame<'_>> {
        let length = *self.driver.lengths.get(index)?;
        let bytes = self.driver.slots.get(index)?.get(..length)?;
        let metadata = *self.driver.metadata.get(index)?;
        Some(BorrowedFrame { bytes, metadata })
    }

    fn overflow(&self) -> OverflowReport {
        self.overflow
    }

    fn release(mut self) {
        self.driver.finish_batch(true);
        self.released = true;
    }
}

impl Drop for PortableReceiveBatch<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.driver.finish_batch(false);
        }
    }
}

impl IngestDriver for PortableSocketDriver {
    type Error = PortableSocketError;
    type Batch<'driver> = PortableReceiveBatch<'driver>;

    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    fn bind_queue(&mut self, binding: QueueBinding) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(PortableSocketError::BatchOutstanding);
        }
        if self.lifecycle == DriverLifecycle::Running {
            return Err(PortableSocketError::InvalidLifecycle {
                expected: DriverLifecycle::Created,
                actual: self.lifecycle,
            });
        }
        if binding.queue_id != 0 {
            return Err(PortableSocketError::QueueUnavailable(binding.queue_id));
        }
        if binding.maximum_batch_items.get() > self.config.maximum_batch_items.get() {
            return Err(PortableSocketError::BatchLimitUnsupported(
                binding.maximum_batch_items.get(),
            ));
        }
        if binding.maximum_frame_bytes.get() > self.config.maximum_frame_bytes.get() {
            return Err(PortableSocketError::FrameLimitUnsupported(
                binding.maximum_frame_bytes.get(),
            ));
        }
        let slot_bytes = binding.maximum_frame_bytes.get().saturating_add(1);
        self.slots = (0..binding.maximum_batch_items.get())
            .map(|_| vec![0_u8; slot_bytes])
            .collect();
        self.lengths = Vec::with_capacity(binding.maximum_batch_items.get());
        self.metadata = Vec::with_capacity(binding.maximum_batch_items.get());
        self.binding = Some(binding);
        self.lifecycle = DriverLifecycle::Bound;
        Ok(())
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        if self.lifecycle != DriverLifecycle::Bound {
            return Err(PortableSocketError::InvalidLifecycle {
                expected: DriverLifecycle::Bound,
                actual: self.lifecycle,
            });
        }
        let socket = UdpSocket::bind(self.config.bind_address).map_err(PortableSocketError::Io)?;
        socket
            .set_nonblocking(true)
            .map_err(PortableSocketError::Io)?;
        self.socket = Some(socket);
        self.lifecycle = DriverLifecycle::Running;
        Ok(())
    }

    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error> {
        if self.lifecycle != DriverLifecycle::Running {
            return Err(PortableSocketError::InvalidLifecycle {
                expected: DriverLifecycle::Running,
                actual: self.lifecycle,
            });
        }
        if self.batch_outstanding {
            return Err(PortableSocketError::BatchOutstanding);
        }
        let binding = self.binding.ok_or(PortableSocketError::QueueNotBound)?;
        self.lengths.clear();
        self.metadata.clear();
        let deadline = Instant::now() + self.config.poll_timeout;
        while self.lengths.len() < binding.maximum_batch_items.get() {
            let index = self.lengths.len();
            let socket = self
                .socket
                .as_ref()
                .ok_or(PortableSocketError::NotStarted)?;
            match socket.recv(&mut self.slots[index]) {
                Ok(received) if received <= binding.maximum_frame_bytes.get() => {
                    self.lengths.push(received);
                    self.metadata.push(ReceiveMetadata {
                        receive_timestamp_unix_nanos: Self::receive_timestamp(),
                        timestamp_source: TimestampSource::SocketSoftware,
                        queue_id: binding.queue_id,
                    });
                }
                Ok(received) => {
                    self.overflow.dropped_frames = self.overflow.dropped_frames.saturating_add(1);
                    self.overflow.dropped_bytes = self
                        .overflow
                        .dropped_bytes
                        .saturating_add(u64::try_from(received).unwrap_or(u64::MAX));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if !self.lengths.is_empty() || Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(self.config.idle_poll_interval);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if is_datagram_too_large(&error) => {
                    self.overflow.dropped_frames = self.overflow.dropped_frames.saturating_add(1);
                    self.overflow.dropped_bytes = self.overflow.dropped_bytes.saturating_add(
                        u64::try_from(binding.maximum_frame_bytes.get().saturating_add(1))
                            .unwrap_or(u64::MAX),
                    );
                }
                Err(error) => return Err(PortableSocketError::Io(error)),
            }
        }
        self.batch_outstanding = true;
        let overflow = self.overflow;
        Ok(PortableReceiveBatch {
            driver: self,
            overflow,
            released: false,
        })
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(PortableSocketError::BatchOutstanding);
        }
        if self.lifecycle != DriverLifecycle::Running {
            return Err(PortableSocketError::InvalidLifecycle {
                expected: DriverLifecycle::Running,
                actual: self.lifecycle,
            });
        }
        self.socket = None;
        self.lifecycle = DriverLifecycle::Stopped;
        Ok(())
    }

    fn health(&self) -> DriverHealth {
        DriverHealth {
            active_mode: self.permit.active_mode(),
            lifecycle: self.lifecycle,
            queue_id: self.binding.map_or(0, |binding| binding.queue_id),
            queued_frames: self.lengths.len(),
            overflow: self.overflow,
            released_batches: self.released_batches,
            abandoned_batches: self.abandoned_batches,
        }
    }
}

fn is_datagram_too_large(error: &io::Error) -> bool {
    // WSAEMSGSIZE on Windows, EMSGSIZE on Linux, and EMSGSIZE on BSD/macOS.
    matches!(error.raw_os_error(), Some(10_040 | 90 | 40))
}

#[derive(Debug)]
pub enum PortableSocketError {
    Readiness(ReadinessError),
    Io(io::Error),
    ZeroPollTimeout,
    InvalidIdlePollInterval,
    ReceiveStorageOverflow,
    QueueNotBound,
    NotStarted,
    BatchOutstanding,
    QueueUnavailable(u16),
    BatchLimitUnsupported(usize),
    FrameLimitUnsupported(usize),
    InvalidLifecycle {
        expected: DriverLifecycle,
        actual: DriverLifecycle,
    },
}

impl fmt::Display for PortableSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "portable socket ingest error: {self:?}")
    }
}

impl Error for PortableSocketError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Readiness(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ReadinessError> for PortableSocketError {
    fn from(error: ReadinessError) -> Self {
        Self::Readiness(error)
    }
}

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
