//! Bounded configuration shared by native and unsupported targets.

use crate::errors::TunedLinuxSocketError;
use std::{net::SocketAddr, num::NonZeroUsize, time::Duration};

/// Configuration shared by native and unsupported target representations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TunedLinuxSocketConfig {
    pub bind_address: SocketAddr,
    pub poll_timeout: Duration,
    pub idle_poll_interval: Duration,
    pub maximum_batch_items: NonZeroUsize,
    pub maximum_frame_bytes: NonZeroUsize,
    pub receive_buffer_bytes: NonZeroUsize,
}

impl TunedLinuxSocketConfig {
    /// Creates a bounded configuration without enabling speculative tuning.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid poll intervals, UDP limits, or storage overflow.
    pub fn try_new(
        bind_address: SocketAddr,
        poll_timeout: Duration,
        idle_poll_interval: Duration,
        maximum_batch_items: NonZeroUsize,
        maximum_frame_bytes: NonZeroUsize,
        receive_buffer_bytes: NonZeroUsize,
    ) -> Result<Self, TunedLinuxSocketError> {
        if poll_timeout.is_zero() {
            return Err(TunedLinuxSocketError::ZeroPollTimeout);
        }
        if idle_poll_interval.is_zero() || idle_poll_interval > poll_timeout {
            return Err(TunedLinuxSocketError::InvalidIdlePollInterval);
        }
        if maximum_frame_bytes.get() > 65_507 {
            return Err(TunedLinuxSocketError::FrameLimitUnsupported(
                maximum_frame_bytes.get(),
            ));
        }
        maximum_batch_items
            .get()
            .checked_mul(maximum_frame_bytes.get().saturating_add(1))
            .ok_or(TunedLinuxSocketError::ReceiveStorageOverflow)?;
        Ok(Self {
            bind_address,
            poll_timeout,
            idle_poll_interval,
            maximum_batch_items,
            maximum_frame_bytes,
            receive_buffer_bytes,
        })
    }

    /// Linux loopback configuration used by unprivileged CI conformance.
    ///
    /// # Errors
    ///
    /// Returns an error only if built-in bounds cease to validate.
    pub fn loopback() -> Result<Self, TunedLinuxSocketError> {
        Self::try_new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Duration::from_millis(100),
            Duration::from_millis(1),
            NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
            NonZeroUsize::new(1_048_576).unwrap_or(NonZeroUsize::MIN),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::TunedLinuxSocketConfig;
    use crate::errors::TunedLinuxSocketError;
    use crate::test_fixture::{UDP_MAXIMUM_PAYLOAD_BYTES, config};
    use std::time::Duration;

    #[test]
    fn loopback_defaults_validate() {
        let config = TunedLinuxSocketConfig::loopback().expect("built-in bounds validate");
        assert!(config.maximum_frame_bytes.get() <= UDP_MAXIMUM_PAYLOAD_BYTES);
    }

    #[test]
    fn zero_poll_timeout_is_rejected() {
        let error = config(Duration::ZERO, Duration::from_millis(1), 2_048)
            .expect_err("a zero poll timeout must be rejected");
        assert!(matches!(error, TunedLinuxSocketError::ZeroPollTimeout));
    }

    #[test]
    fn idle_interval_above_poll_timeout_is_rejected() {
        let error = config(Duration::from_millis(10), Duration::from_millis(50), 2_048)
            .expect_err("an idle interval above the poll timeout must be rejected");
        assert!(matches!(
            error,
            TunedLinuxSocketError::InvalidIdlePollInterval
        ));
    }

    #[test]
    fn frame_limit_above_udp_payload_maximum_is_rejected() {
        let oversized = UDP_MAXIMUM_PAYLOAD_BYTES + 1;
        let error = config(
            Duration::from_millis(100),
            Duration::from_millis(1),
            oversized,
        )
        .expect_err("a frame limit above the UDP payload maximum must be rejected");
        assert!(matches!(
            error,
            TunedLinuxSocketError::FrameLimitUnsupported(value) if value == oversized
        ));
    }
}
