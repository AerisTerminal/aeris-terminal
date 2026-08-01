//! Shared bounded configuration values for this crate's unit tests.

use crate::config::TunedLinuxSocketConfig;
use crate::errors::TunedLinuxSocketError;
use std::{net::SocketAddr, num::NonZeroUsize, time::Duration};

pub(crate) const UDP_MAXIMUM_PAYLOAD_BYTES: usize = 65_507;

pub(crate) fn limit(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test limit is non-zero")
}

pub(crate) fn loopback_address() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

pub(crate) fn config(
    poll_timeout: Duration,
    idle_poll_interval: Duration,
    maximum_frame_bytes: usize,
) -> Result<TunedLinuxSocketConfig, TunedLinuxSocketError> {
    TunedLinuxSocketConfig::try_new(
        loopback_address(),
        poll_timeout,
        idle_poll_interval,
        limit(8),
        limit(maximum_frame_bytes),
        limit(1 << 20),
    )
}
