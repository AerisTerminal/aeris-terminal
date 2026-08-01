//! Borrowed receive batches with explicit release and overflow accounting.

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
