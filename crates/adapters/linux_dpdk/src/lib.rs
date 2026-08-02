//! Isolated DPDK activation gate and software-visible prerequisite probe.
//!
//! No reviewed Rust binding currently meets the Stage 1 compatibility, maintenance,
//! and safety bar. Activation fails explicitly and never falls back to a kernel socket.

mod config;
mod errors;
mod fixture;
#[cfg(all(target_os = "linux", feature = "native"))]
mod native_lifecycle;
#[cfg(all(target_os = "linux", feature = "native"))]
mod native_sys;
mod prerequisites;
mod review;
#[cfg(test)]
mod test_fixture;
mod unavailable_driver;

pub use config::DpdkConfig;
pub use errors::DpdkError;
pub use fixture::fixture_driver;
#[cfg(all(target_os = "linux", feature = "native"))]
pub use native_lifecycle::{
    EXPECTED_DPDK_VERSION, NativeEalLifecycle, RingLoopbackReport, VdevLifecycleReport,
};
pub use prerequisites::{DpdkEvidenceStatus, DpdkPrerequisiteReport, probe_prerequisites};
pub use review::{
    BUILD_REVIEW, CANDIDATE_SURVEY, DEPENDENCY_DECISION, LICENSE_REVIEW, MAINTENANCE_REVIEW,
    MISSING_NATIVE_EVIDENCE, NATIVE_DEPENDENCY_SELECTED, POLL_MODE_DRIVER_VERIFIED, PROFILE,
    PROVENANCE_REVIEW, SAFETY_REVIEW,
};
pub use unavailable_driver::{DpdkDriver, DpdkReceiveBatch};
