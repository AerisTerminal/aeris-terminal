//! Bounded `AF_XDP` queue configuration and native ring limits.

use crate::errors::AfXdpError;
use std::num::NonZeroUsize;

#[cfg(all(target_os = "linux", feature = "native-copy"))]
pub(crate) const NATIVE_RING_ENTRIES_MAXIMUM: usize = 4_096;
#[cfg(all(target_os = "linux", feature = "native-copy"))]
pub(crate) const NATIVE_UMEM_FRAME_BYTES: usize = 4_096;
#[cfg(all(target_os = "linux", feature = "native-copy"))]
pub(crate) const NATIVE_XDP_HEADROOM_BYTES: usize = 256;
#[cfg(all(target_os = "linux", feature = "native-copy"))]
pub(crate) const ETHERNET_HEADER_BYTES: usize = 14;
#[cfg(all(target_os = "linux", feature = "native-copy"))]
pub(crate) const AXIUSFLOW_EXPERIMENTAL_ETHERTYPE: [u8; 2] = [0x88, 0xb5];
#[cfg(all(target_os = "linux", feature = "native-copy"))]
pub(crate) const NATIVE_PACKET_BYTES_MAXIMUM: usize =
    NATIVE_UMEM_FRAME_BYTES - NATIVE_XDP_HEADROOM_BYTES - ETHERNET_HEADER_BYTES;
#[cfg(all(target_os = "linux", feature = "native-copy"))]
pub(crate) const RECEIVE_POLL_TIMEOUT_MILLIS: i32 = 100;

/// Bounded parameters for one fixed UMEM and queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AfXdpConfig {
    pub interface_name: String,
    pub queue_id: u16,
    pub frame_count: NonZeroUsize,
    pub maximum_frame_bytes: NonZeroUsize,
}

impl AfXdpConfig {
    /// Creates a bounded queue request without claiming that the host can activate it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty interface or an Ethernet frame limit above 65,535.
    pub fn try_new(
        interface_name: impl Into<String>,
        queue_id: u16,
        frame_count: NonZeroUsize,
        maximum_frame_bytes: NonZeroUsize,
    ) -> Result<Self, AfXdpError> {
        let interface_name = interface_name.into();
        if interface_name.trim().is_empty() {
            return Err(AfXdpError::EmptyInterfaceName);
        }
        if maximum_frame_bytes.get() > 65_535 {
            return Err(AfXdpError::FrameLimitUnsupported(maximum_frame_bytes.get()));
        }
        frame_count
            .get()
            .checked_mul(maximum_frame_bytes.get())
            .ok_or(AfXdpError::ReceiveStorageOverflow)?;
        Ok(Self {
            interface_name,
            queue_id,
            frame_count,
            maximum_frame_bytes,
        })
    }

    #[cfg(all(target_os = "linux", feature = "native-copy"))]
    pub(crate) fn validate_native_copy(&self) -> Result<(), AfXdpError> {
        if self.queue_id == u16::MAX {
            return Err(AfXdpError::QueueUnavailable(self.queue_id));
        }
        if !self.frame_count.get().is_power_of_two()
            || self.frame_count.get() > NATIVE_RING_ENTRIES_MAXIMUM
        {
            return Err(AfXdpError::RingSizeUnsupported(self.frame_count.get()));
        }
        if self.maximum_frame_bytes.get() > NATIVE_PACKET_BYTES_MAXIMUM {
            return Err(AfXdpError::FrameLimitUnsupported(
                self.maximum_frame_bytes.get(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AfXdpConfig;
    use crate::errors::AfXdpError;
    #[cfg(all(target_os = "linux", feature = "native-copy"))]
    use crate::test_fixture::binding;
    use crate::test_fixture::limit;
    #[cfg(all(target_os = "linux", feature = "native-copy"))]
    use axiusflow_transport::{DriverLifecycle, IngestDriver, QueueBinding};

    #[test]
    fn empty_interface_name_is_rejected() {
        let error = AfXdpConfig::try_new("   ", 0, limit(64), limit(2_048))
            .expect_err("a blank interface name must be rejected");
        assert!(matches!(error, AfXdpError::EmptyInterfaceName));
    }

    #[test]
    fn frame_limit_above_ethernet_maximum_is_rejected() {
        let error = AfXdpConfig::try_new("axrx0", 0, limit(64), limit(65_536))
            .expect_err("a frame limit above 65,535 must be rejected");
        assert!(matches!(error, AfXdpError::FrameLimitUnsupported(65_536)));
    }

    #[cfg(all(target_os = "linux", feature = "native-copy"))]
    #[test]
    fn native_configuration_boundary_matrix_is_total_and_bounded() {
        let frame_limits = [
            1,
            64,
            1_500,
            2_048,
            super::NATIVE_PACKET_BYTES_MAXIMUM,
            super::NATIVE_PACKET_BYTES_MAXIMUM + 1,
            65_535,
        ];
        for queue_id in [0, 1, u16::MAX - 1, u16::MAX] {
            for frame_count in 1..=super::NATIVE_RING_ENTRIES_MAXIMUM.saturating_mul(2) {
                for frame_limit in frame_limits {
                    let config = AfXdpConfig::try_new(
                        "axrx0",
                        queue_id,
                        limit(frame_count),
                        limit(frame_limit),
                    )
                    .expect("the generic Ethernet bounds accept this matrix entry");
                    let result = config.validate_native_copy();
                    let expected_valid = queue_id != u16::MAX
                        && frame_count.is_power_of_two()
                        && frame_count <= super::NATIVE_RING_ENTRIES_MAXIMUM
                        && frame_limit <= super::NATIVE_PACKET_BYTES_MAXIMUM;
                    assert_eq!(
                        result.is_ok(),
                        expected_valid,
                        "unexpected native validation for queue={queue_id}, frames={frame_count}, limit={frame_limit}"
                    );
                }
            }
        }
    }

    #[cfg(all(target_os = "linux", feature = "native-copy"))]
    #[test]
    fn pre_open_lifecycle_matrix_never_allocates_native_resources() {
        for iteration in 0..2_048_u16 {
            let queue_id = iteration % 8;
            let config = AfXdpConfig::try_new("axrx0", queue_id, limit(64), limit(2_048))
                .expect("the stress configuration is bounded");

            let mut wrong_queue = crate::copy_driver::AfXdpCopyDriver::try_new(config.clone())
                .expect("authorization succeeds");
            let error = wrong_queue
                .bind_queue(QueueBinding {
                    queue_id: queue_id.saturating_add(1),
                    ..binding()
                })
                .expect_err("a mismatched queue must fail before native open");
            assert!(matches!(error, AfXdpError::QueueUnavailable(_)));

            let mut oversized_batch = crate::copy_driver::AfXdpCopyDriver::try_new(config.clone())
                .expect("authorization succeeds");
            let error = oversized_batch
                .bind_queue(QueueBinding {
                    queue_id,
                    maximum_batch_items: limit(65),
                    maximum_frame_bytes: limit(2_048),
                })
                .expect_err("a batch larger than the UMEM frame count must fail");
            assert!(matches!(error, AfXdpError::BatchLimitUnsupported(65)));

            let mut wrong_frame_limit =
                crate::copy_driver::AfXdpCopyDriver::try_new(config.clone())
                    .expect("authorization succeeds");
            let error = wrong_frame_limit
                .bind_queue(QueueBinding {
                    queue_id,
                    maximum_batch_items: limit(2),
                    maximum_frame_bytes: limit(1_500),
                })
                .expect_err("a mismatched frame limit must fail before native open");
            assert!(matches!(error, AfXdpError::FrameLimitUnsupported(1_500)));

            let mut bound = crate::copy_driver::AfXdpCopyDriver::try_new(config)
                .expect("authorization succeeds");
            bound
                .bind_queue(QueueBinding {
                    queue_id,
                    ..binding()
                })
                .expect("the exact bounded queue binds without opening native resources");
            assert_eq!(bound.health().lifecycle, DriverLifecycle::Bound);
            assert!(matches!(
                bound.bind_queue(QueueBinding {
                    queue_id,
                    ..binding()
                }),
                Err(AfXdpError::InvalidLifecycle {
                    expected: DriverLifecycle::Created,
                    actual: DriverLifecycle::Bound,
                })
            ));
            assert!(matches!(
                bound.receive_batch(),
                Err(AfXdpError::InvalidLifecycle {
                    expected: DriverLifecycle::Running,
                    actual: DriverLifecycle::Bound,
                })
            ));
            assert!(matches!(
                bound.shutdown(),
                Err(AfXdpError::InvalidLifecycle {
                    expected: DriverLifecycle::Running,
                    actual: DriverLifecycle::Bound,
                })
            ));
        }
    }
}
