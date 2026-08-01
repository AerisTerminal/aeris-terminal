use axiusflow_transport::IngestProfile;

pub const PROFILE: IngestProfile = IngestProfile::LinuxDpdk;
pub const NATIVE_DEPENDENCY_SELECTED: bool = false;
pub const POLL_MODE_DRIVER_VERIFIED: bool = false;
pub const SAFETY_REVIEW: &str = "dpdk-stdlib 0.2.0 wraps mbufs but its RX/TX queue methods are placeholders and its sys crate defaults to behaviorally successful stubs";
pub const LICENSE_REVIEW: &str = "dpdk-stdlib 0.2.0 and DPDK userspace licensing are acceptable, but license acceptance does not make placeholder I/O selectable";
pub const PROVENANCE_REVIEW: &str = "dpdk-stdlib 0.2.0 registry source is 2bfbb7f20f1410bc11fea71014f218282dcda9e6; real bindgen accepts any libdpdk >=21.0 instead of one exact reviewed DPDK release";
pub const MAINTENANCE_REVIEW: &str = "dpdk-stdlib 0.2.0 is current but incomplete: queue setup, receive, and transmit explicitly remain placeholders";
pub const BUILD_REVIEW: &str = "dpdk-stdlib-sys 0.2.0 silently compiles stubs unless both DPDK and bindgen are available; that mode can report a fake device and is forbidden for Axiusflow activation";
pub const MISSING_NATIVE_EVIDENCE: &str = "approved exact-pinned binding and DPDK release, audited mbuf ownership wrapper, fuzzing, huge-page/EAL virtual-device lifecycle, PMD queue isolation";
