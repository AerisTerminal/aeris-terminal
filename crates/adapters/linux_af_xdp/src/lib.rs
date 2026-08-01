#![deny(unsafe_code)]
//! Linux `AF_XDP` copy-mode driver and explicit unavailable activation gate.
//!
//! The native dependency is Linux-only. All calls into its unsafe UMEM and ring API are
//! quarantined in the private `native` module; the public driver owns every descriptor
//! until it is submitted, receives ownership back from RX, and recycles it on batch
//! release or drop.

mod config;
mod copy_driver;
mod errors;
mod fixture;
#[cfg(all(target_os = "linux", feature = "native-copy"))]
mod native;
mod prerequisites;
mod review;
#[cfg(test)]
mod test_fixture;
mod unavailable_driver;

pub use config::AfXdpConfig;
pub use copy_driver::{AfXdpCopyDriver, AfXdpCopyReceiveBatch};
pub use errors::AfXdpError;
pub use fixture::fixture_driver;
pub use prerequisites::{AfXdpEvidenceStatus, AfXdpPrerequisiteReport, probe_prerequisites};
pub use review::{
    BUILD_REVIEW, COPY_DRIVER_EVIDENCE_ID, LICENSE_REVIEW, MAINTENANCE_REVIEW,
    MISSING_NATIVE_EVIDENCE, NATIVE_DEPENDENCY_SELECTED, PROFILE, PROVENANCE_REVIEW, SAFETY_REVIEW,
    ZERO_COPY_VERIFIED,
};
pub use unavailable_driver::{AfXdpDriver, AfXdpReceiveBatch};
