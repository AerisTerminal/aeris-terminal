//! Isolated DPDK activation gate and software-visible prerequisite probe.
//!
//! No reviewed Rust binding currently meets the Stage 1 compatibility, maintenance,
//! and safety bar. Activation fails explicitly and never falls back to a kernel socket.

mod config;
mod errors;
mod fixture;
mod prerequisites;
mod review;
#[cfg(test)]
mod test_fixture;
mod unavailable_driver;

pub use config::DpdkConfig;
pub use errors::DpdkError;
pub use fixture::fixture_driver;
pub use prerequisites::{DpdkEvidenceStatus, DpdkPrerequisiteReport, probe_prerequisites};
pub use review::{
    BUILD_REVIEW, LICENSE_REVIEW, MAINTENANCE_REVIEW, MISSING_NATIVE_EVIDENCE,
    NATIVE_DEPENDENCY_SELECTED, POLL_MODE_DRIVER_VERIFIED, PROFILE, PROVENANCE_REVIEW,
    SAFETY_REVIEW,
};
pub use unavailable_driver::{DpdkDriver, DpdkReceiveBatch};
