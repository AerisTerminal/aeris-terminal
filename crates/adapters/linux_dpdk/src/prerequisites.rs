use crate::MISSING_NATIVE_EVIDENCE;

/// Evidence state for one native prerequisite or exercised behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpdkEvidenceStatus {
    Present,
    Missing,
    NotSelected,
    NotExercised,
    Unverified,
}

const fn evidence_status(present: bool) -> DpdkEvidenceStatus {
    if present {
        DpdkEvidenceStatus::Present
    } else {
        DpdkEvidenceStatus::Missing
    }
}

/// Read-only host evidence. It does not initialize EAL, reserve pages, or bind a NIC.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DpdkPrerequisiteReport {
    pub linux_target: bool,
    pub huge_pages_total: u64,
    pub huge_pages_free: u64,
    pub vfio_driver: DpdkEvidenceStatus,
    pub vfio_control: DpdkEvidenceStatus,
    pub pkg_config_metadata: DpdkEvidenceStatus,
    pub native_dependency: DpdkEvidenceStatus,
    pub software_device: DpdkEvidenceStatus,
    pub poll_mode_driver: DpdkEvidenceStatus,
    pub missing_evidence: &'static str,
}

/// Returns software-visible prerequisites without changing host configuration.
#[must_use]
pub fn probe_prerequisites() -> DpdkPrerequisiteReport {
    #[cfg(target_os = "linux")]
    let (
        huge_pages_total,
        huge_pages_free,
        vfio_driver_present,
        vfio_control_present,
        pkg_config_metadata_present,
    ) = {
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let meminfo_value = |name: &str| {
            meminfo.lines().find_map(|line| {
                line.strip_prefix(name)
                    .and_then(|value| value.trim().parse::<u64>().ok())
            })
        };
        let package_metadata_in_common_path = [
            "/usr/lib/pkgconfig/libdpdk.pc",
            "/usr/lib/x86_64-linux-gnu/pkgconfig/libdpdk.pc",
            "/usr/local/lib/pkgconfig/libdpdk.pc",
            "/usr/local/lib64/pkgconfig/libdpdk.pc",
        ]
        .iter()
        .any(|path| std::path::Path::new(path).exists());
        let package_metadata_in_environment =
            std::env::var_os("PKG_CONFIG_PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|path| path.join("libdpdk.pc").exists())
            });
        (
            meminfo_value("HugePages_Total:").unwrap_or(0),
            meminfo_value("HugePages_Free:").unwrap_or(0),
            std::path::Path::new("/sys/bus/pci/drivers/vfio-pci").exists(),
            std::path::Path::new("/dev/vfio/vfio").exists(),
            package_metadata_in_common_path || package_metadata_in_environment,
        )
    };
    #[cfg(not(target_os = "linux"))]
    let (
        huge_pages_total,
        huge_pages_free,
        vfio_driver_present,
        vfio_control_present,
        pkg_config_metadata_present,
    ) = (0, 0, false, false, false);

    DpdkPrerequisiteReport {
        linux_target: cfg!(target_os = "linux"),
        huge_pages_total,
        huge_pages_free,
        vfio_driver: evidence_status(vfio_driver_present),
        vfio_control: evidence_status(vfio_control_present),
        pkg_config_metadata: evidence_status(pkg_config_metadata_present),
        native_dependency: DpdkEvidenceStatus::NotSelected,
        software_device: DpdkEvidenceStatus::NotExercised,
        poll_mode_driver: DpdkEvidenceStatus::Unverified,
        missing_evidence: MISSING_NATIVE_EVIDENCE,
    }
}
