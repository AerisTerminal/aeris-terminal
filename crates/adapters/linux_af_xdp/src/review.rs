//! Recorded dependency review text and profile constants.

use axiusflow_transport::IngestProfile;

pub const PROFILE: IngestProfile = IngestProfile::LinuxAfXdp;
pub const NATIVE_DEPENDENCY_SELECTED: bool =
    cfg!(all(target_os = "linux", feature = "native-copy"));
pub const ZERO_COPY_VERIFIED: bool = false;
pub const SAFETY_REVIEW: &str = "xsk-rs 0.8.0 unsafe socket, UMEM-data, RX, and fill-ring calls are isolated in one private Linux module; the safe driver enforces single-UMEM descriptor provenance and retains user ownership until each descriptor is submitted";
pub const LICENSE_REVIEW: &str = "xsk-rs 0.8.0 is MIT licensed; Cargo.lock pins libxdp-sys 0.2.4+1.6.0, which builds vendored libxdp/libbpf and links system libelf/zlib under their respective terms";
pub const PROVENANCE_REVIEW: &str = "xsk-rs 0.8.0 registry checksum d1fef46e3505c5055082f52ada0a7f8e5dcaebdbb9eccf8e978c32382c159270; upstream tag v0.8.0 commit c0b110cd3b6763fdcfc41996b3cea8c9f259614f; Cargo.lock pins libxdp-sys 0.2.4+1.6.0 checksum 6098c8281e42ed6f46240af889297dae1e37f70ee505dd26fe5c7199563e4d86";
pub const MAINTENANCE_REVIEW: &str = "xsk-rs 0.8.0 was published 2025-09-17 and documents testing on Linux 6.5; the native-copy feature is isolated from portable builds; deterministic boundary stress and coverage-guided pre-open fuzzing exist, while privileged lifecycle, independent unsafe-boundary audit, and native data-path fuzzing remain incomplete";
pub const BUILD_REVIEW: &str = "Linux-only xsk-rs 0.8.0 is selected only by the native-copy feature and requires the libxdp/libbpf build stack plus privileges for socket/program activation; portable and non-Linux builds neither compile nor link it";
pub const MISSING_NATIVE_EVIDENCE: &str = "successful repeated privileged veth copy-mode lifecycle on a qualified host, independent unsafe-boundary audit, privileged native data-path fuzzing, qualified NIC/driver, zero-copy, authorized packet feed";
pub const COPY_DRIVER_EVIDENCE_ID: &str = "xsk_rs_0_8_0_af_xdp_copy_driver_review";

#[cfg(test)]
mod tests {
    use super::{PROFILE, ZERO_COPY_VERIFIED};
    use axiusflow_transport::IngestProfile;

    #[test]
    fn profile_is_linux_af_xdp_and_zero_copy_is_never_claimed() {
        assert_eq!(PROFILE, IngestProfile::LinuxAfXdp);
        const {
            assert!(
                !ZERO_COPY_VERIFIED,
                "zero copy must never be claimed by this adapter"
            );
        }
    }
}
