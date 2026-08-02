//! Raw DPDK FFI isolated behind the reviewed safe lifecycle surface.
//!
//! Exported functions and types come from bindgen against the exact reviewed
//! 25.11.0 headers through an explicit allowlist. DPDK's header-inline fast path
//! (`rte_eth_rx_burst`, `rte_eth_tx_burst`, `rte_pktmbuf_alloc/free/mtod`) has no
//! shared-library symbols, so `native/shim.c` re-exports those inlines unchanged
//! under stable names and the C compiler resolves their exact semantics.
//!
//! Invariants at this boundary:
//!
//! - `rte_eal_init` runs at most once per process and every argument pointer stays
//!   valid for the whole call; EAL is process-global state, so every call in this
//!   module is serialized behind one lock.
//! - `rte_eal_cleanup` runs only after a successful init and never twice.
//! - `rte_version` returns a static NUL-terminated string owned by DPDK; it is only
//!   read, never stored beyond the call.
//! - Every mbuf comes from this adapter's own pktmbuf pool and is returned exactly
//!   once through `rte_pktmbuf_free` after a transmit burst or payload copy; a
//!   transmit burst that accepts an mbuf transfers that ownership to the PMD.
//! - Payload copies never exceed `rte_pktmbuf_tailroom`, so a malformed length
//!   cannot overflow the mbuf data room.
#![allow(unsafe_code)]

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::ptr::{self, NonNull};
use std::ffi::{CStr, CString};
use std::sync::{Mutex, OnceLock};

#[allow(
    dead_code,
    non_camel_case_types,
    non_upper_case_globals,
    clippy::all,
    clippy::pedantic
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/dpdk_bindings.rs"));
}

/// Opaque DPDK mempool handle owned by this adapter.
pub(crate) type Mempool = generated::rte_mempool;

unsafe extern "C" {
    fn axiusflow_rte_eth_rx_burst(
        port_id: u16,
        queue_id: u16,
        rx_pkts: *mut *mut generated::rte_mbuf,
        nb_pkts: u16,
    ) -> u16;
    fn axiusflow_rte_eth_tx_burst(
        port_id: u16,
        queue_id: u16,
        tx_pkts: *mut *mut generated::rte_mbuf,
        nb_pkts: u16,
    ) -> u16;
    fn axiusflow_rte_pktmbuf_alloc(mp: *mut generated::rte_mempool) -> *mut generated::rte_mbuf;
    fn axiusflow_rte_pktmbuf_free(m: *mut generated::rte_mbuf);
    fn axiusflow_rte_pktmbuf_mtod(m: *mut generated::rte_mbuf) -> *mut c_void;
    fn axiusflow_rte_pktmbuf_tailroom(m: *mut generated::rte_mbuf) -> u16;
    fn axiusflow_rte_pktmbuf_set_len(m: *mut generated::rte_mbuf, len: u16);
    fn axiusflow_rte_pktmbuf_data_len(m: *mut generated::rte_mbuf) -> u16;
    fn axiusflow_rte_errno() -> c_int;
}

fn eal_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_eal() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    eal_lock()
        .lock()
        .map_err(|_| "DPDK EAL serialization lock is poisoned".to_string())
}

/// Initializes EAL with the supplied argument strings and reports the number of
/// available ethdev ports.
///
/// # Errors
///
/// Returns a message when EAL rejects the arguments or the guard is poisoned.
pub(crate) fn eal_init_counting_ports(arguments: &[&str]) -> Result<u16, String> {
    let _guard = lock_eal()?;
    let owned: Vec<CString> = arguments
        .iter()
        .map(|argument| {
            CString::new(*argument).map_err(|_| "EAL argument contains NUL".to_string())
        })
        .collect::<Result<_, _>>()?;
    let mut pointers: Vec<*mut c_char> = owned
        .iter()
        .map(|argument| argument.as_ptr().cast_mut())
        .collect();
    let argc = c_int::try_from(pointers.len()).map_err(|_| "too many EAL arguments".to_string())?;
    // SAFETY: `pointers` points at live NUL-terminated strings in `owned`, which
    // outlives the call, and EAL init is serialized behind the lock.
    let consumed = unsafe { generated::rte_eal_init(argc, pointers.as_mut_ptr()) };
    if consumed < 0 {
        return Err("rte_eal_init rejected the arguments".to_string());
    }
    // SAFETY: pure query after a successful init.
    Ok(unsafe { generated::rte_eth_dev_count_avail() })
}

/// Tears down EAL after a successful init.
///
/// # Errors
///
/// Returns a message when cleanup itself fails or the guard is poisoned.
pub(crate) fn eal_cleanup() -> Result<(), String> {
    let _guard = lock_eal()?;
    // SAFETY: only called after a successful init, serialized behind the lock.
    if unsafe { generated::rte_eal_cleanup() } != 0 {
        return Err("rte_eal_cleanup failed".to_string());
    }
    Ok(())
}

/// Reads the runtime DPDK version string.
///
/// # Errors
///
/// Returns a message when the runtime string is not valid UTF-8.
pub(crate) fn runtime_version() -> Result<&'static str, String> {
    // SAFETY: `rte_version` returns a static NUL-terminated string owned by DPDK.
    let version = unsafe { CStr::from_ptr(generated::rte_version()) };
    version
        .to_str()
        .map_err(|_| "DPDK runtime version is not valid UTF-8".to_string())
}

/// Creates a packet mbuf pool with default data rooms.
///
/// # Errors
///
/// Returns a message when pool creation fails or the arguments overflow.
pub(crate) fn mempool_create(
    name: &str,
    entries: u32,
) -> Result<NonNull<generated::rte_mempool>, String> {
    let _guard = lock_eal()?;
    let name = CString::new(name).map_err(|_| "mempool name contains NUL".to_string())?;
    let data_room = u16::try_from(generated::RTE_MBUF_DEFAULT_DATAROOM)
        .map_err(|_| "DPDK default data room does not fit u16".to_string())?;
    // SAFETY: name outlives the call; the returned pool is owned by the caller and
    // must be released through `mempool_free`.
    let pool = unsafe {
        generated::rte_pktmbuf_pool_create(name.as_ptr(), entries as c_uint, 32, 0, data_room, 0)
    };
    NonNull::new(pool).ok_or_else(|| {
        // SAFETY: rte_errno is a per-thread value valid after the failed call.
        let errno = unsafe { axiusflow_rte_errno() };
        format!("rte_pktmbuf_pool_create failed with rte_errno {errno}")
    })
}

/// Frees a pool created by [`mempool_create`]. The caller guarantees every mbuf
/// from the pool has been returned first.
pub(crate) fn mempool_free(pool: NonNull<generated::rte_mempool>) {
    let _guard = lock_eal();
    // SAFETY: the pool is owned by this adapter and all of its mbufs were returned.
    unsafe { generated::rte_mempool_free(pool.as_ptr()) }
}

/// Reports the number of entries currently available in the pool.
pub(crate) fn mempool_avail_count(pool: NonNull<generated::rte_mempool>) -> u32 {
    let _guard = lock_eal();
    // SAFETY: pure query on a live pool.
    unsafe { generated::rte_mempool_avail_count(pool.as_ptr()) }
}

/// Configures one port with default parameters.
///
/// # Errors
///
/// Returns a message when the device rejects the configuration.
pub(crate) fn dev_configure(port: u16, rx_queues: u16, tx_queues: u16) -> Result<(), String> {
    let _guard = lock_eal()?;
    // SAFETY: rte_eth_conf is a plain C configuration struct for which all-zero
    // selects every default mode.
    let configuration: generated::rte_eth_conf = unsafe { core::mem::zeroed() };
    // SAFETY: the buffer outlives the call.
    let result = unsafe {
        generated::rte_eth_dev_configure(port, rx_queues, tx_queues, &raw const configuration)
    };
    if result != 0 {
        return Err(format!("rte_eth_dev_configure failed with {result}"));
    }
    Ok(())
}

/// Sets up one receive queue on the pool with PMD default parameters.
///
/// # Errors
///
/// Returns a message when queue setup fails.
pub(crate) fn rx_queue_setup(
    port: u16,
    queue: u16,
    descriptors: u16,
    pool: NonNull<generated::rte_mempool>,
) -> Result<(), String> {
    let _guard = lock_eal()?;
    // SAFETY: a NULL rx_conf selects PMD defaults and the pool outlives the queue.
    let result = unsafe {
        generated::rte_eth_rx_queue_setup(port, queue, descriptors, 0, ptr::null(), pool.as_ptr())
    };
    if result != 0 {
        return Err(format!("rte_eth_rx_queue_setup failed with {result}"));
    }
    Ok(())
}

/// Sets up one transmit queue with PMD default parameters.
///
/// # Errors
///
/// Returns a message when queue setup fails.
pub(crate) fn tx_queue_setup(port: u16, queue: u16, descriptors: u16) -> Result<(), String> {
    let _guard = lock_eal()?;
    // SAFETY: a NULL tx_conf selects PMD defaults.
    let result =
        unsafe { generated::rte_eth_tx_queue_setup(port, queue, descriptors, 0, ptr::null()) };
    if result != 0 {
        return Err(format!("rte_eth_tx_queue_setup failed with {result}"));
    }
    Ok(())
}

/// Starts a configured port.
///
/// # Errors
///
/// Returns a message when the port fails to start.
pub(crate) fn dev_start(port: u16) -> Result<(), String> {
    let _guard = lock_eal()?;
    // SAFETY: the port is configured and its queues are set up.
    let result = unsafe { generated::rte_eth_dev_start(port) };
    if result != 0 {
        return Err(format!("rte_eth_dev_start failed with {result}"));
    }
    Ok(())
}

/// Stops a started port.
///
/// # Errors
///
/// Returns a message when the port fails to stop.
pub(crate) fn dev_stop(port: u16) -> Result<(), String> {
    let _guard = lock_eal()?;
    // SAFETY: the port was started by this adapter.
    let result = unsafe { generated::rte_eth_dev_stop(port) };
    if result != 0 {
        return Err(format!("rte_eth_dev_stop failed with {result}"));
    }
    Ok(())
}

/// Closes a stopped port.
///
/// # Errors
///
/// Returns a message when the port fails to close.
pub(crate) fn dev_close(port: u16) -> Result<(), String> {
    let _guard = lock_eal()?;
    // SAFETY: the port was stopped by this adapter.
    let result = unsafe { generated::rte_eth_dev_close(port) };
    if result != 0 {
        return Err(format!("rte_eth_dev_close failed with {result}"));
    }
    Ok(())
}

/// Allocates one direct mbuf from the pool and writes the payload into its data
/// room. The returned mbuf is owned by the caller until it is transmitted or freed
/// through [`mbuf_free`].
///
/// # Errors
///
/// Returns a message when the pool is empty, the payload exceeds the tailroom, or
/// the payload does not fit the DPDK length field.
pub(crate) fn mbuf_alloc_write(
    pool: NonNull<generated::rte_mempool>,
    payload: &[u8],
) -> Result<NonNull<generated::rte_mbuf>, String> {
    let _guard = lock_eal()?;
    let length = u16::try_from(payload.len())
        .map_err(|_| "payload does not fit the DPDK length field".to_string())?;
    // SAFETY: the pool is live and owned by this adapter.
    let mbuf = unsafe { axiusflow_rte_pktmbuf_alloc(pool.as_ptr()) };
    let mbuf = NonNull::new(mbuf).ok_or_else(|| "mempool has no free mbuf".to_string())?;
    // SAFETY: the mbuf is direct and freshly allocated from this adapter's pool.
    unsafe {
        let tailroom = axiusflow_rte_pktmbuf_tailroom(mbuf.as_ptr());
        if payload.len() > usize::from(tailroom) {
            axiusflow_rte_pktmbuf_free(mbuf.as_ptr());
            return Err("payload exceeds the mbuf tailroom".to_string());
        }
        axiusflow_rte_pktmbuf_set_len(mbuf.as_ptr(), length);
        ptr::copy_nonoverlapping(
            payload.as_ptr(),
            axiusflow_rte_pktmbuf_mtod(mbuf.as_ptr()).cast::<u8>(),
            payload.len(),
        );
    }
    Ok(mbuf)
}

/// Returns one direct mbuf to its pool.
pub(crate) fn mbuf_free(mbuf: NonNull<generated::rte_mbuf>) {
    let _guard = lock_eal();
    // SAFETY: the mbuf is direct with refcnt = 1 and belongs to this adapter's pool.
    unsafe { axiusflow_rte_pktmbuf_free(mbuf.as_ptr()) }
}

/// Transmits one mbuf through the `rte_eth_tx_burst` inline; an accepted mbuf
/// transfers ownership to the PMD. Returns the number of accepted mbufs.
///
/// # Errors
///
/// This wrapper is infallible beyond the burst result itself; the inline's
/// semantics are resolved by the shim.
pub(crate) fn tx_burst(port: u16, queue: u16, mbuf: NonNull<generated::rte_mbuf>) -> u16 {
    let _guard = lock_eal();
    let mut mbufs = [mbuf.as_ptr()];
    // SAFETY: the port is started by this adapter, the queue was set up on it, and
    // `mbufs` outlives the call.
    unsafe { axiusflow_rte_eth_tx_burst(port, queue, mbufs.as_mut_ptr(), 1) }
}

/// Receives up to `maximum` mbufs through the `rte_eth_rx_burst` inline, copies
/// each payload out, and frees every received mbuf.
///
/// # Errors
///
/// This wrapper is infallible beyond the burst result itself; the inline's
/// semantics are resolved by the shim.
pub(crate) fn rx_burst_collect(port: u16, queue: u16, maximum: u16) -> Vec<Vec<u8>> {
    let _guard = lock_eal();
    if maximum == 0 {
        return Vec::new();
    }
    let mut mbufs: Vec<*mut generated::rte_mbuf> = vec![ptr::null_mut(); usize::from(maximum)];
    // SAFETY: the port is started by this adapter, the queue was set up on it, and
    // `mbufs` has room for exactly `maximum` pointers.
    let received = unsafe { axiusflow_rte_eth_rx_burst(port, queue, mbufs.as_mut_ptr(), maximum) };
    let mut payloads = Vec::with_capacity(usize::from(received));
    for mbuf in mbufs.into_iter().take(usize::from(received)) {
        if mbuf.is_null() {
            continue;
        }
        // SAFETY: each received mbuf is a live direct mbuf; its data_len bounds the
        // valid payload at the mtod pointer, and it is freed exactly once here.
        unsafe {
            let length = usize::from(axiusflow_rte_pktmbuf_data_len(mbuf));
            let source = axiusflow_rte_pktmbuf_mtod(mbuf).cast::<u8>();
            payloads.push(std::slice::from_raw_parts(source, length).to_vec());
            axiusflow_rte_pktmbuf_free(mbuf);
        }
    }
    payloads
}
