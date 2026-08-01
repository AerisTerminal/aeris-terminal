//! Deterministic fixture driver proving the borrowed/release lifecycle.

use crate::driver::{
    DriverCapabilities, DriverHealth, DriverLifecycle, IngestDriver, QueueBinding,
};
use crate::errors::ReadinessError;
use crate::profile::ActiveIngestMode;
use crate::readiness::ActivationPermit;
use crate::receive_batch::{BorrowedFrame, OverflowReport, ReceiveBatch, ReceiveMetadata};
use core::fmt;
use std::collections::VecDeque;
use std::error::Error;

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
