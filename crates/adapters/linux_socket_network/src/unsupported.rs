//! Non-Linux representation that reports unavailability without fallback.

use crate::config::TunedLinuxSocketConfig;
use crate::errors::TunedLinuxSocketError;
use crate::review::PROFILE;
use axiusflow_transport::{
    ActiveIngestMode, BorrowedFrame, DriverCapabilities, DriverHealth, DriverLifecycle,
    IngestDriver, OverflowReport, QueueBinding, ReceiveBatch,
};

/// Non-Linux representation that reports unavailability without fallback.
#[derive(Debug)]
pub struct TunedLinuxSocketDriver {
    capabilities: DriverCapabilities,
}

impl TunedLinuxSocketDriver {
    #[must_use]
    pub fn unavailable(_config: TunedLinuxSocketConfig) -> Self {
        Self {
            capabilities: DriverCapabilities {
                profile: PROFILE,
                supported_modes: Vec::new(),
                receive_queues: 0,
                maximum_frame_bytes: 0,
                timestamp_sources: Vec::new(),
                zero_copy_verified: false,
            },
        }
    }
}

/// Empty batch type required by the transport-neutral trait on unsupported targets.
#[derive(Debug)]
pub struct TunedLinuxReceiveBatch;

impl ReceiveBatch for TunedLinuxReceiveBatch {
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

impl IngestDriver for TunedLinuxSocketDriver {
    type Error = TunedLinuxSocketError;
    type Batch<'driver> = TunedLinuxReceiveBatch;

    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    fn bind_queue(&mut self, _binding: QueueBinding) -> Result<(), Self::Error> {
        Err(TunedLinuxSocketError::UnsupportedPlatform)
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        Err(TunedLinuxSocketError::UnsupportedPlatform)
    }

    fn receive_batch(&mut self) -> Result<Self::Batch<'_>, Self::Error> {
        Err(TunedLinuxSocketError::UnsupportedPlatform)
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        Err(TunedLinuxSocketError::UnsupportedPlatform)
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

#[cfg(test)]
mod tests {
    use super::TunedLinuxSocketDriver;
    use crate::config::TunedLinuxSocketConfig;
    use axiusflow_transport::IngestDriver;

    #[test]
    fn non_linux_target_reports_unavailable_without_fallback() {
        let driver = TunedLinuxSocketDriver::unavailable(
            TunedLinuxSocketConfig::loopback().expect("built-in bounds validate"),
        );
        assert!(
            driver.capabilities().supported_modes.is_empty(),
            "a non-Linux target must never advertise a tuned socket mode"
        );
    }
}
