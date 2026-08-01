use crate::PortableSocketError;
use std::{net::SocketAddr, num::NonZeroUsize, time::Duration};

pub(crate) const UDP_MAXIMUM_PAYLOAD_BYTES: usize = 65_507;

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
