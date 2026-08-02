//! Native DPDK EAL and virtual-device lifecycle probe.
//!
//! This is the first native milestone for the DPDK profile: it proves EAL startup,
//! virtual PMD enumeration, runtime version binding, clean teardown, and the
//! queue/mbuf lifecycle against the exact reviewed release without huge pages, PCI
//! devices, or privileges. A virtual loopback proves packet flow only; activation
//! as a poll-mode ingest driver remains unavailable until a real PMD queue, mbuf
//! pool sizing, and hardware evidence exist.

use crate::DpdkError;
use crate::native_sys;
use std::time::{Duration, Instant};

/// DPDK release this adapter is reviewed and built against.
pub const EXPECTED_DPDK_VERSION: &str = "25.11.0";
/// Program name passed as EAL argument zero.
const EAL_PROGRAM_NAME: &str = "axiusflow-dpdk-lifecycle";
/// Pool entries backing the loopback queues; one ring descriptor per entry.
const POOL_ENTRIES: u32 = 1_024;
/// Descriptors on each loopback queue.
const QUEUE_DESCRIPTORS: u16 = 1_024;
/// Frames transmitted into the loopback ring.
const LOOPBACK_FRAMES: usize = 8;
/// Payload carried by every loopback frame.
const LOOPBACK_PAYLOAD: [u8; 64] = [0xa5; 64];
/// Bounded wait for the looped frames to appear on the receive queue.
const RECEIVE_DEADLINE: Duration = Duration::from_secs(2);

/// Outcome of one native EAL and virtual-device lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdevLifecycleReport {
    pub runtime_version: String,
    pub ports_available: u16,
    pub cleanup_succeeded: bool,
}

/// Outcome of one queue/mbuf loopback over the `net_ring` virtual device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RingLoopbackReport {
    pub pool_entries_before: u32,
    pub frames_transmitted: usize,
    pub frames_received: usize,
    pub payloads_matched: bool,
    pub pool_entries_after: u32,
    pub pool_leaked: bool,
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
        let pmd_argument = driver_argument("librte_net_ring")?;
        let mempool_ops_argument = driver_argument("librte_mempool_ring")?;
        let arguments = [
            EAL_PROGRAM_NAME,
            "--no-huge",
            "--no-pci",
            "--log-level=error",
            pmd_argument.as_str(),
            mempool_ops_argument.as_str(),
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

    /// Runs one EAL session that additionally proves the queue/mbuf lifecycle:
    /// pool creation, port configuration, receive/transmit queue setup, start,
    /// a bounded transmit/receive loopback with payload verification, exact mbuf
    /// return accounting, stop, close, and pool teardown.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime version differs from the reviewed release,
    /// any lifecycle step fails, frames do not loop back with their payloads intact,
    /// or the pool does not account for every entry at the end.
    pub fn run_net_ring_loopback() -> Result<(VdevLifecycleReport, RingLoopbackReport), DpdkError> {
        let runtime_version = native_sys::runtime_version()
            .map_err(|_| DpdkError::NativeLifecycleFailed)?
            .to_string();
        if !runtime_version.contains(EXPECTED_DPDK_VERSION) {
            return Err(DpdkError::RuntimeVersionMismatch);
        }
        let pmd_argument = driver_argument("librte_net_ring")?;
        let mempool_ops_argument = driver_argument("librte_mempool_ring")?;
        let arguments = [
            EAL_PROGRAM_NAME,
            "--no-huge",
            "--no-pci",
            "--log-level=error",
            pmd_argument.as_str(),
            mempool_ops_argument.as_str(),
            "--vdev=net_ring0",
        ];
        let ports_available = native_sys::eal_init_counting_ports(&arguments)
            .map_err(|_| DpdkError::NativeLifecycleFailed)?;
        if ports_available == 0 {
            return Err(DpdkError::NativeLifecycleFailed);
        }
        let loopback = run_loopback_on_port(0);
        let cleanup_succeeded = native_sys::eal_cleanup().is_ok();
        let loopback = loopback?;
        if !cleanup_succeeded {
            return Err(DpdkError::NativeLifecycleFailed);
        }
        Ok((
            VdevLifecycleReport {
                runtime_version,
                ports_available,
                cleanup_succeeded,
            },
            loopback,
        ))
    }
}

/// Resolves one DPDK shared driver for an EAL `-d` argument: an explicit operator
/// override first, then the exact reviewed soname and the development symlink in
/// the recorded libdir.
fn driver_argument(library_stem: &str) -> Result<String, DpdkError> {
    let override_path = std::env::var_os("AXIUSFLOW_DPDK_PMD_LIBRARY")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned());
    let override_dir = std::env::var_os("AXIUSFLOW_DPDK_DRIVER_DIRECTORY")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned());
    let candidates = [
        override_path.filter(|path| path.contains(library_stem)),
        override_dir.map(|dir| format!("{dir}/{library_stem}.so.26")),
        Some(format!(
            "{}/{library_stem}.so.26",
            env!("AXIUSFLOW_DPDK_LIBDIR")
        )),
        Some(format!(
            "{}/{library_stem}.so",
            env!("AXIUSFLOW_DPDK_LIBDIR")
        )),
    ];
    let library = candidates
        .iter()
        .flatten()
        .find(|candidate| std::path::Path::new(candidate).is_file())
        .ok_or(DpdkError::NativeLifecycleFailed)?;
    Ok(format!("-d{library}"))
}

fn run_loopback_on_port(port: u16) -> Result<RingLoopbackReport, DpdkError> {
    let pool = native_sys::mempool_create("axiusflow_loopback", POOL_ENTRIES)
        .map_err(|_| DpdkError::NativeLifecycleFailed)?;
    let result = run_loopback_with_pool(port, pool);
    if result.is_ok() {
        native_sys::mempool_free(pool);
    }
    result
}

fn run_loopback_with_pool(
    port: u16,
    pool: core::ptr::NonNull<crate::native_sys::Mempool>,
) -> Result<RingLoopbackReport, DpdkError> {
    let pool_entries_before = native_sys::mempool_avail_count(pool);
    native_sys::dev_configure(port, 1, 1).map_err(|_| DpdkError::NativeLifecycleFailed)?;
    native_sys::rx_queue_setup(port, 0, QUEUE_DESCRIPTORS, pool)
        .map_err(|_| DpdkError::NativeLifecycleFailed)?;
    native_sys::tx_queue_setup(port, 0, QUEUE_DESCRIPTORS)
        .map_err(|_| DpdkError::NativeLifecycleFailed)?;
    native_sys::dev_start(port).map_err(|_| DpdkError::NativeLifecycleFailed)?;

    let mut frames_transmitted = 0_usize;
    for _ in 0..LOOPBACK_FRAMES {
        let mbuf = native_sys::mbuf_alloc_write(pool, &LOOPBACK_PAYLOAD)
            .map_err(|_| DpdkError::NativeLifecycleFailed)?;
        if native_sys::tx_burst(port, 0, mbuf) == 1 {
            frames_transmitted += 1;
        } else {
            native_sys::mbuf_free(mbuf);
        }
    }

    let deadline = Instant::now() + RECEIVE_DEADLINE;
    let mut received: Vec<Vec<u8>> = Vec::new();
    while received.len() < frames_transmitted && Instant::now() < deadline {
        received.extend(native_sys::rx_burst_collect(port, 0, 64));
        if received.len() < frames_transmitted {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    native_sys::dev_stop(port).map_err(|_| DpdkError::NativeLifecycleFailed)?;
    native_sys::dev_close(port).map_err(|_| DpdkError::NativeLifecycleFailed)?;

    let payloads_matched = received
        .iter()
        .all(|payload| payload.as_slice() == LOOPBACK_PAYLOAD);
    let pool_entries_after = native_sys::mempool_avail_count(pool);
    if frames_transmitted != LOOPBACK_FRAMES
        || received.len() != frames_transmitted
        || !payloads_matched
    {
        return Err(DpdkError::NativeLifecycleFailed);
    }
    Ok(RingLoopbackReport {
        pool_entries_before,
        frames_transmitted,
        frames_received: received.len(),
        payloads_matched,
        pool_entries_after,
        pool_leaked: pool_entries_after != pool_entries_before,
    })
}
