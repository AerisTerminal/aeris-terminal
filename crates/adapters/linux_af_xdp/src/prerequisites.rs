//! Host prerequisite probing without claiming activation.

use crate::config::AfXdpConfig;
use crate::review::{MISSING_NATIVE_EVIDENCE, NATIVE_DEPENDENCY_SELECTED};

/// Evidence state for one native prerequisite or exercised behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AfXdpEvidenceStatus {
    Present,
    Missing,
    NotSelected,
    NotExercised,
    Unverified,
}

const fn evidence_status(present: bool) -> AfXdpEvidenceStatus {
    if present {
        AfXdpEvidenceStatus::Present
    } else {
        AfXdpEvidenceStatus::Missing
    }
}

/// Read-only host evidence. It does not load XDP, allocate UMEM, or change an interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AfXdpPrerequisiteReport {
    pub linux_target: bool,
    pub interface: AfXdpEvidenceStatus,
    pub receive_queue: AfXdpEvidenceStatus,
    pub kernel_btf: AfXdpEvidenceStatus,
    pub bpf_filesystem: AfXdpEvidenceStatus,
    pub xdp_diagnostics: AfXdpEvidenceStatus,
    pub native_dependency: AfXdpEvidenceStatus,
    pub copy_mode: AfXdpEvidenceStatus,
    pub zero_copy: AfXdpEvidenceStatus,
    pub missing_evidence: &'static str,
}

/// Returns software-visible prerequisites without making privileged host changes.
#[must_use]
pub fn probe_prerequisites(config: &AfXdpConfig) -> AfXdpPrerequisiteReport {
    #[cfg(target_os = "linux")]
    let (
        interface_present,
        receive_queue_present,
        kernel_btf_present,
        bpf_filesystem_mounted,
        xdp_diagnostics_present,
    ) = {
        let interface_path = std::path::Path::new("/sys/class/net").join(&config.interface_name);
        let receive_queue_present = interface_path
            .join("queues")
            .join(format!("rx-{}", config.queue_id))
            .exists();
        let bpf_filesystem_mounted =
            std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|mounts| {
                mounts.lines().any(|line| {
                    let Some((mount, filesystem)) = line.split_once(" - ") else {
                        return false;
                    };
                    mount.split_whitespace().nth(4) == Some("/sys/fs/bpf")
                        && filesystem.split_whitespace().next() == Some("bpf")
                })
            });
        (
            interface_path.exists(),
            receive_queue_present,
            std::path::Path::new("/sys/kernel/btf/vmlinux").exists(),
            bpf_filesystem_mounted,
            std::path::Path::new("/proc/net/xdp").exists()
                || std::path::Path::new("/sys/module/xsk_diag").exists(),
        )
    };
    #[cfg(not(target_os = "linux"))]
    let _ = config;
    #[cfg(not(target_os = "linux"))]
    let (
        interface_present,
        receive_queue_present,
        kernel_btf_present,
        bpf_filesystem_mounted,
        xdp_diagnostics_present,
    ) = (false, false, false, false, false);

    AfXdpPrerequisiteReport {
        linux_target: cfg!(target_os = "linux"),
        interface: evidence_status(interface_present),
        receive_queue: evidence_status(receive_queue_present),
        kernel_btf: evidence_status(kernel_btf_present),
        bpf_filesystem: evidence_status(bpf_filesystem_mounted),
        xdp_diagnostics: evidence_status(xdp_diagnostics_present),
        native_dependency: if NATIVE_DEPENDENCY_SELECTED {
            AfXdpEvidenceStatus::Present
        } else {
            AfXdpEvidenceStatus::NotSelected
        },
        copy_mode: AfXdpEvidenceStatus::NotExercised,
        zero_copy: AfXdpEvidenceStatus::Unverified,
        missing_evidence: MISSING_NATIVE_EVIDENCE,
    }
}
