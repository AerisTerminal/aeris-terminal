use crate::{DpdkConfig, DpdkError, DpdkPrerequisiteReport, PROFILE, probe_prerequisites};
use axiusflow_transport::{
    ActiveIngestMode, BorrowedFrame, DriverCapabilities, DriverHealth, DriverLifecycle,
    IngestDriver, OverflowReport, QueueBinding, ReceiveBatch,
};

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
