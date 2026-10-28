use std::{
    ffi::{OsString, c_int, c_void},
    io,
    os::unix::ffi::OsStringExt as _,
    path::PathBuf,
};

/// `PROC_PIDPATHINFO_MAXSIZE` from `<libproc.h>` (four times `MAXPATHLEN`).
const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;
/// Headroom for processes that start between sizing and listing.
const PROCESS_LIST_HEADROOM: usize = 64;

// libproc is part of libSystem, which every macOS binary links.
unsafe extern "C" {
    fn proc_listallpids(buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
}

pub(crate) fn process_ids() -> io::Result<Vec<u32>> {
    // SAFETY: A null buffer with zero size asks only for the process count.
    let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    let count = usize::try_from(count).map_err(|_| io::Error::last_os_error())?;
    let mut pids = vec![0 as c_int; count + PROCESS_LIST_HEADROOM];
    let bytes = c_int::try_from(pids.len() * std::mem::size_of::<c_int>())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `pids` is writable for exactly `bytes` bytes for this call.
    let listed = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let listed = usize::try_from(listed).map_err(|_| io::Error::last_os_error())?;
    Ok(pids
        .into_iter()
        .take(listed)
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .collect())
}

pub(crate) fn image_path(id: u32) -> Option<PathBuf> {
    let pid = c_int::try_from(id).ok()?;
    let mut buffer = vec![0_u8; PROC_PIDPATHINFO_MAXSIZE];
    let size = u32::try_from(buffer.len()).ok()?;
    // SAFETY: `buffer` is writable for `size` bytes for this single call.
    let length = unsafe { proc_pidpath(pid, buffer.as_mut_ptr().cast(), size) };
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length > 0 && *length <= buffer.len())?;
    buffer.truncate(length);
    Some(PathBuf::from(OsString::from_vec(buffer)))
}
