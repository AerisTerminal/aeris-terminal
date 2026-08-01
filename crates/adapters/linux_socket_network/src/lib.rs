//! Target-gated tuned Linux UDP ingest adapter.
//!
//! The Linux implementation applies only a bounded receive buffer, address reuse for
//! deterministic restart, and nonblocking bounded polling. Busy polling, affinity,
//! hardware timestamps, RSS steering, and `io_uring` remain disabled until measured.

mod config;
mod errors;
mod fixture;
#[cfg(target_os = "linux")]
mod native;
mod review;
#[cfg(test)]
mod test_fixture;
#[cfg(not(target_os = "linux"))]
mod unsupported;

pub use config::TunedLinuxSocketConfig;
pub use errors::TunedLinuxSocketError;
pub use fixture::fixture_driver;
pub use review::{
    BUSY_POLL_ENABLED, HARDWARE_TIMESTAMP_VERIFIED, PROFILE, TUNED_HOST_MODE_IMPLEMENTED,
    TUNING_REVIEW,
};

#[cfg(target_os = "linux")]
pub use native::{TunedLinuxReceiveBatch, TunedLinuxSocketDriver};
#[cfg(not(target_os = "linux"))]
pub use unsupported::{TunedLinuxReceiveBatch, TunedLinuxSocketDriver};
