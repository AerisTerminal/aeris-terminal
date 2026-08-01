#![no_main]

use axiusflow_linux_af_xdp_adapter::{AfXdpConfig, AfXdpCopyDriver, AfXdpError};
use axiusflow_transport::{DriverLifecycle, IngestDriver, QueueBinding};
use libfuzzer_sys::fuzz_target;
use std::num::NonZeroUsize;

fn bounded_nonzero(bytes: [u8; 2], maximum: usize) -> NonZeroUsize {
    let value = usize::from(u16::from_le_bytes(bytes)) % maximum + 1;
    NonZeroUsize::new(value).expect("the modulo result is non-zero")
}

fuzz_target!(|data: &[u8]| {
    let Some(input) = data.get(..10) else {
        return;
    };
    let queue_id = u16::from_le_bytes([input[0], input[1]]);
    let frame_count = bounded_nonzero([input[2], input[3]], 8_192);
    let maximum_frame_bytes = bounded_nonzero([input[4], input[5]], 65_536);
    let requested_queue = u16::from_le_bytes([input[6], input[7]]);
    let maximum_batch_items = bounded_nonzero([input[8], input[9]], 8_192);

    let Ok(config) = AfXdpConfig::try_new(
        "fuzz0",
        queue_id,
        frame_count,
        maximum_frame_bytes,
    ) else {
        return;
    };
    let Ok(mut driver) = AfXdpCopyDriver::try_new(config) else {
        return;
    };

    assert_eq!(driver.health().lifecycle, DriverLifecycle::Created);
    assert!(!driver.capabilities().zero_copy_verified);
    let binding = QueueBinding {
        queue_id: requested_queue,
        maximum_batch_items,
        maximum_frame_bytes,
    };
    match driver.bind_queue(binding) {
        Ok(()) => {
            assert_eq!(driver.health().lifecycle, DriverLifecycle::Bound);
            assert!(matches!(
                driver.receive_batch(),
                Err(AfXdpError::InvalidLifecycle {
                    expected: DriverLifecycle::Running,
                    actual: DriverLifecycle::Bound,
                })
            ));
            assert!(matches!(
                driver.shutdown(),
                Err(AfXdpError::InvalidLifecycle {
                    expected: DriverLifecycle::Running,
                    actual: DriverLifecycle::Bound,
                })
            ));
            assert!(matches!(
                driver.bind_queue(binding),
                Err(AfXdpError::InvalidLifecycle {
                    expected: DriverLifecycle::Created,
                    actual: DriverLifecycle::Bound,
                })
            ));
        }
        Err(_) => assert_eq!(driver.health().lifecycle, DriverLifecycle::Created),
    }
});
