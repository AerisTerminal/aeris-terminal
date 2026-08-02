//! Native DPDK EAL and virtual-device lifecycle probe.
//!
//! This is the first native milestone for the DPDK profile: it proves EAL startup,
//! virtual PMD enumeration, runtime version binding, and clean teardown against the
//! exact reviewed release without huge pages, PCI devices, or privileges. Queue and
//! mbuf lifecycle is deliberately not claimed here; activation as a poll-mode ingest
//! driver remains unavailable until that evidence exists.

use crate::DpdkError;
use crate::native_sys;

/// DPDK release this adapter is reviewed and built against.
pub const EXPECTED_DPDK_VERSION: &str = "25.11.0";
/// Program name passed as EAL argument zero.
const EAL_PROGRAM_NAME: &str = "axiusflow-dpdk-lifecycle";

/// Outcome of one native EAL and virtual-device lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdevLifecycleReport {
    pub runtime_version: String,
    pub ports_available: u16,
    pub cleanup_succeeded: bool,
}

/// Runs one EAL init, virtual-device enumeration, and cleanup lifecycle.
pub struct NativeEalLifecycle;

impl NativeEalLifecycle {
    /// Initializes EAL with the `net_ring` virtual device, counts the ports it
    /// registered, verifies the runtime version matches the reviewed release, and
    /// tears EAL down.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime version differs from the reviewed release,
    /// EAL init or cleanup fails, or the virtual device registers no ports.
    pub fn run_net_ring_lifecycle() -> Result<VdevLifecycleReport, DpdkError> {
        let runtime_version = native_sys::runtime_version()
            .map_err(|_| DpdkError::NativeLifecycleFailed)?
            .to_string();
        if !runtime_version.contains(EXPECTED_DPDK_VERSION) {
            return Err(DpdkError::RuntimeVersionMismatch);
        }
        let pmd_override = std::env::var_os("AXIUSFLOW_DPDK_PMD_LIBRARY")
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string_lossy().into_owned());
        let pmd_candidates = [
            pmd_override,
            Some(format!(
                "{}/librte_net_ring.so.26",
                env!("AXIUSFLOW_DPDK_LIBDIR")
            )),
            Some(format!(
                "{}/librte_net_ring.so",
                env!("AXIUSFLOW_DPDK_LIBDIR")
            )),
        ];
        let pmd_library = pmd_candidates
            .iter()
            .flatten()
            .find(|candidate| std::path::Path::new(candidate).is_file())
            .ok_or(DpdkError::NativeLifecycleFailed)?;
        let pmd_argument = format!("-d{pmd_library}");
        let arguments = [
            EAL_PROGRAM_NAME,
            "--no-huge",
            "--no-pci",
            "--log-level=error",
            pmd_argument.as_str(),
            "--vdev=net_ring0",
        ];
        let ports_available = native_sys::eal_init_counting_ports(&arguments)
            .map_err(|_| DpdkError::NativeLifecycleFailed)?;
        let cleanup_succeeded = native_sys::eal_cleanup().is_ok();
        if ports_available == 0 {
            return Err(DpdkError::NativeLifecycleFailed);
        }
        if !cleanup_succeeded {
            return Err(DpdkError::NativeLifecycleFailed);
        }
        Ok(VdevLifecycleReport {
            runtime_version,
            ports_available,
            cleanup_succeeded,
        })
    }
}
