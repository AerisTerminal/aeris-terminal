//! Explicitly unavailable adapter that never falls back silently.

use crate::config::AfXdpConfig;
use crate::errors::AfXdpError;
use crate::prerequisites::{AfXdpPrerequisiteReport, probe_prerequisites};
use crate::review::PROFILE;
use axiusflow_transport::{
    ActiveIngestMode, BorrowedFrame, DriverCapabilities, DriverHealth, DriverLifecycle,
    IngestDriver, OverflowReport, QueueBinding, ReceiveBatch,
};

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

#[cfg(test)]
mod tests {
    use super::AfXdpDriver;
    use crate::errors::AfXdpError;
    use crate::prerequisites::AfXdpEvidenceStatus;
    use crate::review::NATIVE_DEPENDENCY_SELECTED;
    use crate::test_fixture::{binding, config};
    use axiusflow_transport::{ActiveIngestMode, DriverLifecycle, IngestDriver};

    #[test]
    fn unavailable_driver_reports_no_supported_modes() {
        let driver = AfXdpDriver::unavailable(&config());
        assert!(
            driver.capabilities().supported_modes.is_empty(),
            "an unavailable driver must advertise no active mode"
        );
        assert!(!driver.capabilities().zero_copy_verified);
        assert_eq!(driver.health().active_mode, ActiveIngestMode::Unavailable);
        assert_eq!(driver.health().lifecycle, DriverLifecycle::Unsupported);
    }

    #[test]
    fn unavailable_driver_fails_every_lifecycle_call_explicitly() {
        let mut driver = AfXdpDriver::unavailable(&config());
        assert!(matches!(
            driver.bind_queue(binding()),
            Err(AfXdpError::NativeIntegrationUnavailable)
        ));
        assert!(matches!(
            driver.start(),
            Err(AfXdpError::NativeIntegrationUnavailable)
        ));
        assert!(matches!(
            driver.receive_batch(),
            Err(AfXdpError::NativeIntegrationUnavailable)
        ));
        assert!(matches!(
            driver.shutdown(),
            Err(AfXdpError::NativeIntegrationUnavailable)
        ));
    }

    #[test]
    fn unavailable_driver_never_reports_zero_copy_prerequisite() {
        let driver = AfXdpDriver::unavailable(&config());
        let prerequisites = driver.prerequisites();
        assert_eq!(prerequisites.zero_copy, AfXdpEvidenceStatus::Unverified);
        assert_eq!(prerequisites.copy_mode, AfXdpEvidenceStatus::NotExercised);
        if !NATIVE_DEPENDENCY_SELECTED {
            assert_eq!(
                prerequisites.native_dependency,
                AfXdpEvidenceStatus::NotSelected
            );
        }
    }
}
