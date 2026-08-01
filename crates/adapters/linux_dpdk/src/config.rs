use crate::DpdkError;
use std::num::NonZeroUsize;

/// Bounded parameters that a future approved DPDK backend must honor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DpdkConfig {
    pub port_id: u16,
    pub queue_id: u16,
    pub descriptor_count: NonZeroUsize,
    pub maximum_frame_bytes: NonZeroUsize,
}

impl DpdkConfig {
    /// Creates a bounded DPDK queue request without activating EAL or a PMD.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported frame size or storage overflow.
    pub fn try_new(
        port_id: u16,
        queue_id: u16,
        descriptor_count: NonZeroUsize,
        maximum_frame_bytes: NonZeroUsize,
    ) -> Result<Self, DpdkError> {
        if maximum_frame_bytes.get() > 65_535 {
            return Err(DpdkError::FrameLimitUnsupported(maximum_frame_bytes.get()));
        }
        descriptor_count
            .get()
            .checked_mul(maximum_frame_bytes.get())
            .ok_or(DpdkError::ReceiveStorageOverflow)?;
        Ok(Self {
            port_id,
            queue_id,
            descriptor_count,
            maximum_frame_bytes,
        })
    }
}
