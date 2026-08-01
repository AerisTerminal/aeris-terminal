//! Shared bounded configuration values for this crate's unit tests.

use crate::config::AfXdpConfig;
use axiusflow_transport::QueueBinding;
use std::num::NonZeroUsize;

pub(crate) fn limit(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test limit is non-zero")
}

pub(crate) fn config() -> AfXdpConfig {
    AfXdpConfig::try_new("axrx0", 0, limit(64), limit(2_048))
        .expect("a bounded AF_XDP configuration is accepted")
}

pub(crate) fn binding() -> QueueBinding {
    QueueBinding {
        queue_id: 0,
        maximum_batch_items: limit(2),
        maximum_frame_bytes: limit(2_048),
    }
}
