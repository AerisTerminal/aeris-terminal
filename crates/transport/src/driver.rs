//! Bounded ingest driver contract, capabilities, and lifecycle.

use crate::profile::{ActiveIngestMode, IngestProfile};
use crate::receive_batch::{OverflowReport, ReceiveBatch, TimestampSource};
use std::error::Error;
use std::num::NonZeroUsize;

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
