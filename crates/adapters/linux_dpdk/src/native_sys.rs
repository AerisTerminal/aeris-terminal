//! Raw DPDK FFI isolated behind the reviewed safe lifecycle surface.
//!
//! Invariants at this boundary:
//!
//! - `rte_eal_init` runs at most once per process and every argument pointer stays
//!   valid for the whole call; this module owns the only call site.
//! - `rte_eal_cleanup` runs only after a successful init and never twice; EAL is
//!   process-global state, so the safe wrapper serializes both calls behind one lock.
//! - `rte_version` returns a static NUL-terminated string owned by DPDK; it is only
//!   read, never stored beyond the call.
//! - `rte_eth_dev_count_avail` is a pure query with no ownership effects.
#![allow(unsafe_code)]

use core::ffi::{c_char, c_int};
use std::ffi::{CStr, CString};
use std::sync::{Mutex, OnceLock};

#[link(name = "rte_eal")]
unsafe extern "C" {
    fn rte_eal_init(argc: c_int, argv: *mut *mut c_char) -> c_int;
    fn rte_eal_cleanup() -> c_int;
    fn rte_version() -> *const c_char;
}

#[link(name = "rte_ethdev")]
unsafe extern "C" {
    fn rte_eth_dev_count_avail() -> u16;
}

fn eal_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Initializes EAL with the supplied argument strings and reports the number of
/// available ethdev ports.
///
/// # Errors
///
/// Returns the failing argument index when EAL rejects the arguments, or a lock
/// poisoning message when the serialization guard is unavailable.
pub(crate) fn eal_init_counting_ports(arguments: &[&str]) -> Result<u16, String> {
    let _guard = eal_lock()
        .lock()
        .map_err(|_| "DPDK EAL serialization lock is poisoned".to_string())?;
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
    // outlives the call, and EAL init is serialized behind `eal_lock`.
    let consumed = unsafe { rte_eal_init(argc, pointers.as_mut_ptr()) };
    if consumed < 0 {
        return Err("rte_eal_init rejected the arguments".to_string());
    }
    // SAFETY: pure query after a successful init.
    Ok(unsafe { rte_eth_dev_count_avail() })
}

/// Tears down EAL after a successful init.
///
/// # Errors
///
/// Returns a message when cleanup itself fails or the guard is poisoned.
pub(crate) fn eal_cleanup() -> Result<(), String> {
    let _guard = eal_lock()
        .lock()
        .map_err(|_| "DPDK EAL serialization lock is poisoned".to_string())?;
    // SAFETY: only called after a successful init, serialized behind `eal_lock`.
    if unsafe { rte_eal_cleanup() } != 0 {
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
    let version = unsafe { CStr::from_ptr(rte_version()) };
    version
        .to_str()
        .map_err(|_| "DPDK runtime version is not valid UTF-8".to_string())
}
