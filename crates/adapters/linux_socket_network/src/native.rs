//! Real Linux `socket2` UDP ingest with bounded preallocated ownership.

use crate::config::TunedLinuxSocketConfig;
use crate::errors::TunedLinuxSocketError;
use crate::review::PROFILE;
use axiusflow_transport::{ActivationPermit, ReceiveMetadata, TimestampSource};
use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, BorrowedFrame, DriverCapabilities, DriverHealth,
    DriverLifecycle, IngestDriver, OverflowReport, QueueBinding, ReadinessManifest, ReadinessState,
    ReceiveBatch,
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// Real Linux socket driver with bounded preallocated receive ownership.
#[derive(Debug)]
pub struct TunedLinuxSocketDriver {
    permit: ActivationPermit,
    capabilities: DriverCapabilities,
    config: TunedLinuxSocketConfig,
    lifecycle: DriverLifecycle,
    binding: Option<QueueBinding>,
    socket: Option<UdpSocket>,
    applied_receive_buffer_bytes: Option<usize>,
    slots: Vec<Vec<u8>>,
    lengths: Vec<usize>,
    metadata: Vec<ReceiveMetadata>,
    batch_outstanding: bool,
    overflow: OverflowReport,
    released_batches: u64,
    abandoned_batches: u64,
}

impl TunedLinuxSocketDriver {
    /// Creates a dormant Linux driver authorized at its reviewed readiness cap.
    ///
    /// # Errors
    ///
    /// Returns an error if readiness evidence is not authorized.
    pub fn try_new(config: TunedLinuxSocketConfig) -> Result<Self, TunedLinuxSocketError> {
        let capabilities = DriverCapabilities {
            profile: PROFILE,
            supported_modes: vec![ActiveIngestMode::TunedLinuxSocket],
            receive_queues: 1,
            maximum_frame_bytes: config.maximum_frame_bytes.get(),
            timestamp_sources: vec![TimestampSource::SocketSoftware],
            zero_copy_verified: false,
        };
        let permit = ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile: PROFILE,
            requested: ReadinessState::FixtureValidated,
            active_mode: ActiveIngestMode::TunedLinuxSocket,
            evidence_id: "tuned_linux_socket_loopback_conformance",
            capabilities: &capabilities,
        })?;
        Ok(Self {
            permit,
            capabilities,
            config,
            lifecycle: DriverLifecycle::Created,
            binding: None,
            socket: None,
            applied_receive_buffer_bytes: None,
            slots: Vec::new(),
            lengths: Vec::new(),
            metadata: Vec::new(),
            batch_outstanding: false,
            overflow: OverflowReport::default(),
            released_batches: 0,
            abandoned_batches: 0,
        })
    }

    /// Returns the repository-reviewed activation permit.
    #[must_use]
    pub const fn permit(&self) -> &ActivationPermit {
        &self.permit
    }

    /// Returns the active loopback or feed endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error before startup or if the OS rejects the query.
    pub fn local_addr(&self) -> Result<SocketAddr, TunedLinuxSocketError> {
        self.socket
            .as_ref()
            .ok_or(TunedLinuxSocketError::NotStarted)?
            .local_addr()
            .map_err(TunedLinuxSocketError::Io)
    }

    /// Reports the effective kernel receive-buffer size after startup.
    #[must_use]
    pub const fn applied_receive_buffer_bytes(&self) -> Option<usize> {
        self.applied_receive_buffer_bytes
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

/// Borrowed batch over Linux-driver-owned slots.
#[derive(Debug)]
pub struct TunedLinuxReceiveBatch<'driver> {
    driver: &'driver mut TunedLinuxSocketDriver,
    overflow: OverflowReport,
    released: bool,
}

impl ReceiveBatch for TunedLinuxReceiveBatch<'_> {
    fn frame_count(&self) -> usize {
        self.driver.lengths.len()
    }

    fn frame(&self, index: usize) -> Option<BorrowedFrame<'_>> {
        let length = *self.driver.lengths.get(index)?;
        Some(BorrowedFrame {
            bytes: self.driver.slots.get(index)?.get(..length)?,
            metadata: *self.driver.metadata.get(index)?,
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

impl Drop for TunedLinuxReceiveBatch<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.driver.finish_batch(false);
        }
    }
}

impl IngestDriver for TunedLinuxSocketDriver {
    type Error = TunedLinuxSocketError;
    type Batch<'driver> = TunedLinuxReceiveBatch<'driver>;

    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    fn bind_queue(&mut self, binding: QueueBinding) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(TunedLinuxSocketError::BatchOutstanding);
        }
        if self.lifecycle == DriverLifecycle::Running {
            return Err(TunedLinuxSocketError::InvalidLifecycle {
                expected: DriverLifecycle::Created,
                actual: self.lifecycle,
            });
        }
        if binding.queue_id != 0 {
            return Err(TunedLinuxSocketError::QueueUnavailable(binding.queue_id));
        }
        if binding.maximum_batch_items.get() > self.config.maximum_batch_items.get() {
            return Err(TunedLinuxSocketError::BatchLimitUnsupported(
                binding.maximum_batch_items.get(),
            ));
        }
        if binding.maximum_frame_bytes.get() > self.config.maximum_frame_bytes.get() {
            return Err(TunedLinuxSocketError::FrameLimitUnsupported(
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
            return Err(TunedLinuxSocketError::InvalidLifecycle {
                expected: DriverLifecycle::Bound,
                actual: self.lifecycle,
            });
        }
        let domain = if self.config.bind_address.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))
            .map_err(TunedLinuxSocketError::Io)?;
        socket
            .set_reuse_address(true)
            .map_err(TunedLinuxSocketError::Io)?;
        socket
            .set_recv_buffer_size(self.config.receive_buffer_bytes.get())
            .map_err(TunedLinuxSocketError::Io)?;
        socket
            .bind(&self.config.bind_address.into())
            .map_err(TunedLinuxSocketError::Io)?;
        socket
            .set_nonblocking(true)
            .map_err(TunedLinuxSocketError::Io)?;
        self.applied_receive_buffer_bytes = Some(
            socket
                .recv_buffer_size()
                .map_err(TunedLinuxSocketError::Io)?,
        );
        self.socket = Some(socket.into());
        self.lifecycle = DriverLifecycle::Running;
        Ok(())
    }

    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error> {
        if self.lifecycle != DriverLifecycle::Running {
            return Err(TunedLinuxSocketError::InvalidLifecycle {
                expected: DriverLifecycle::Running,
                actual: self.lifecycle,
            });
        }
        if self.batch_outstanding {
            return Err(TunedLinuxSocketError::BatchOutstanding);
        }
        let binding = self.binding.ok_or(TunedLinuxSocketError::QueueNotBound)?;
        self.lengths.clear();
        self.metadata.clear();
        let deadline = Instant::now() + self.config.poll_timeout;
        while self.lengths.len() < binding.maximum_batch_items.get() {
            let index = self.lengths.len();
            let socket = self
                .socket
                .as_ref()
                .ok_or(TunedLinuxSocketError::NotStarted)?;
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
                Err(error) if error.raw_os_error() == Some(90) => {
                    self.overflow.dropped_frames = self.overflow.dropped_frames.saturating_add(1);
                    self.overflow.dropped_bytes = self.overflow.dropped_bytes.saturating_add(
                        u64::try_from(binding.maximum_frame_bytes.get().saturating_add(1))
                            .unwrap_or(u64::MAX),
                    );
                }
                Err(error) => return Err(TunedLinuxSocketError::Io(error)),
            }
        }
        self.batch_outstanding = true;
        let overflow = self.overflow;
        Ok(TunedLinuxReceiveBatch {
            driver: self,
            overflow,
            released: false,
        })
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        if self.batch_outstanding {
            return Err(TunedLinuxSocketError::BatchOutstanding);
        }
        if self.lifecycle != DriverLifecycle::Running {
            return Err(TunedLinuxSocketError::InvalidLifecycle {
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
