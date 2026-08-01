//! Real Linux `AF_XDP` copy-mode ingest over one fixed `UMEM`.

use crate::config::AfXdpConfig;
use crate::errors::AfXdpError;
#[cfg(all(target_os = "linux", feature = "native-copy"))]
use crate::review::{COPY_DRIVER_EVIDENCE_ID, PROFILE};
use axiusflow_transport::{
    ActivationPermit, ActiveIngestMode, BorrowedFrame, DriverCapabilities, DriverHealth,
    DriverLifecycle, IngestDriver, OverflowReport, QueueBinding, ReceiveBatch,
};
#[cfg(all(target_os = "linux", feature = "native-copy"))]
use axiusflow_transport::{ActivationRequest, ReadinessManifest, ReadinessState};
#[cfg(all(target_os = "linux", feature = "native-copy"))]
use axiusflow_transport::{ReceiveMetadata, TimestampSource};

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
    #[cfg(all(target_os = "linux", feature = "native-copy"))]
    native: Option<crate::native::CopySocket>,
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
        #[cfg(not(all(target_os = "linux", feature = "native-copy")))]
        {
            drop(config.interface_name);
            Err(AfXdpError::NativeIntegrationUnavailable)
        }
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
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
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
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
    #[cfg(all(target_os = "linux", feature = "native-copy"))]
    timestamp_unix_nanos: i64,
    released: bool,
}

impl ReceiveBatch for AfXdpCopyReceiveBatch<'_> {
    fn frame_count(&self) -> usize {
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
        {
            self.driver
                .native
                .as_ref()
                .map_or(0, crate::native::CopySocket::batch_len)
        }
        #[cfg(not(all(target_os = "linux", feature = "native-copy")))]
        {
            0
        }
    }

    fn frame(&self, index: usize) -> Option<BorrowedFrame<'_>> {
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
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
        #[cfg(not(all(target_os = "linux", feature = "native-copy")))]
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
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
        {
            let native = crate::native::CopySocket::open(&self.config, binding)
                .map_err(AfXdpError::NativeOpen)?;
            self.native = Some(native);
            self.lifecycle = DriverLifecycle::Running;
            Ok(())
        }
        #[cfg(not(all(target_os = "linux", feature = "native-copy")))]
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
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
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
        #[cfg(not(all(target_os = "linux", feature = "native-copy")))]
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
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
        if let Some(native) = self.native.as_mut() {
            native
                .flush_recycle()
                .map_err(AfXdpError::DescriptorRecycle)?;
        }
        #[cfg(all(target_os = "linux", feature = "native-copy"))]
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
                #[cfg(all(target_os = "linux", feature = "native-copy"))]
                {
                    self.native
                        .as_ref()
                        .map_or(0, crate::native::CopySocket::owned_len)
                }
                #[cfg(not(all(target_os = "linux", feature = "native-copy")))]
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

#[cfg(all(target_os = "linux", feature = "native-copy"))]
fn unix_timestamp_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}
