use axiusflow_transport::IngestProfile;

pub const PROFILE: IngestProfile = IngestProfile::LinuxDpdk;
pub const NATIVE_DEPENDENCY_SELECTED: bool = false;
pub const POLL_MODE_DRIVER_VERIFIED: bool = false;
pub const SAFETY_REVIEW: &str = "dpdk-stdlib 0.2.0 wraps mbufs but its RX/TX queue methods are placeholders and its sys crate defaults to behaviorally successful stubs";
pub const LICENSE_REVIEW: &str = "dpdk-stdlib 0.2.0 and DPDK userspace licensing are acceptable, but license acceptance does not make placeholder I/O selectable";
pub const PROVENANCE_REVIEW: &str = "dpdk-stdlib 0.2.0 registry source is 2bfbb7f20f1410bc11fea71014f218282dcda9e6; real bindgen accepts any libdpdk >=21.0 instead of one exact reviewed DPDK release";
pub const MAINTENANCE_REVIEW: &str = "dpdk-stdlib 0.2.0 is current but incomplete: queue setup, receive, and transmit explicitly remain placeholders";
pub const BUILD_REVIEW: &str = "dpdk-stdlib-sys 0.2.0 silently compiles stubs unless both DPDK and bindgen are available; that mode can report a fake device and is forbidden for Axiusflow activation";
pub const CANDIDATE_SURVEY: &str = "surveyed 2026-08-02: the lemonrock dpdk/dpdk-sys/dpdk-core family is abandoned since 2017-2018 against DPDK 17/18-era headers; rpkt-dpdk 0.1.0 (Apache-2.0, 1580 downloads) was published once on 2023-11-22 and never re-released despite repository pushes through 2025-12-22, and it is a batch packet-processing framework owning its own port/queue runtime rather than a narrow receive adapter; dpdk-stdlib 0.2.0 (MIT, 281 downloads) keeps placeholder RX/TX and a stub-default sys crate per the recorded source review; teto-dpdk 0.2.0 (84 downloads, created 2026-07-11) binds the F-Stack userspace TCP stack, a large unreviewed C codebase at the wrong layer for raw frame ingest; dpdk-net 0.1.0 (27 downloads, one 2026-01-18 release) lacks the adoption and source history needed for a Section 3.1 review";
pub const DEPENDENCY_DECISION: &str = "no published crate meets the exact-pin, real-RX/TX, no-stub, and maintenance bar; when DPDK work resumes, generate first-party bindgen bindings against one exact-pinned vendored DPDK release following the third_party/libxdp-sys pattern, with every unsafe call isolated behind this adapter";
pub const MISSING_NATIVE_EVIDENCE: &str = "approved exact-pinned binding and DPDK release, audited mbuf ownership wrapper, fuzzing, huge-page/EAL virtual-device lifecycle, PMD queue isolation";
