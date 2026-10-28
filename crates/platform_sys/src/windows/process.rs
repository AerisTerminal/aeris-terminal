use std::{
    ffi::OsString,
    io,
    os::windows::{
        ffi::OsStringExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    },
    path::PathBuf,
    time::Duration,
};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};

/// Longest Win32 path `QueryFullProcessImageNameW` can return.
const MAXIMUM_IMAGE_PATH_UNITS: usize = 32_768;
/// `INFINITE` is `u32::MAX`; a finite timeout stays below it.
const MAXIMUM_FINITE_WAIT_MILLISECONDS: u32 = u32::MAX - 1;

pub(crate) struct ImagePathBuffer(Vec<u16>);

impl ImagePathBuffer {
    pub(crate) fn new() -> Self {
        Self(vec![0; MAXIMUM_IMAGE_PATH_UNITS])
    }
}

/// A process handle opened with `SYNCHRONIZE`.
pub(crate) struct ProcessHandle(OwnedHandle);

impl ProcessHandle {
    pub(crate) fn wait_for_exit(&self, timeout: Duration) -> io::Result<bool> {
        let milliseconds = u32::try_from(timeout.as_millis())
            .unwrap_or(MAXIMUM_FINITE_WAIT_MILLISECONDS)
            .min(MAXIMUM_FINITE_WAIT_MILLISECONDS);
        // SAFETY: The owned handle is a live process handle opened with
        // SYNCHRONIZE access, and the wait does not retain it.
        let result = unsafe { WaitForSingleObject(self.0.as_raw_handle(), milliseconds) };
        match result {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

pub(crate) struct OpenedProcess {
    pub(crate) image: PathBuf,
    /// Absent when the process grants query but not wait access.
    pub(crate) handle: Option<ProcessHandle>,
}

pub(crate) fn process_ids() -> io::Result<Vec<u32>> {
    // SAFETY: Both arguments are plain values; the call returns a new
    // snapshot handle or INVALID_HANDLE_VALUE.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The snapshot handle was just returned, is valid and has no
    // other owner, so OwnedHandle may close it exactly once.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?,
        ..PROCESSENTRY32W::default()
    };
    let mut ids = Vec::new();
    // SAFETY: The snapshot handle is live and `entry` is a writable,
    // correctly sized PROCESSENTRY32W for this single call.
    let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    while more {
        ids.push(entry.th32ProcessID);
        // SAFETY: As above; the snapshot stays open for the whole iteration.
        more = unsafe { Process32NextW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == i32::try_from(ERROR_NO_MORE_FILES).ok() {
        Ok(ids)
    } else {
        Err(error)
    }
}

/// Opens one process for waiting and reads its image path. Returns `None`
/// when the process has exited or cannot be queried at all.
pub(crate) fn open_process(
    id: u32,
    buffer: &mut ImagePathBuffer,
) -> io::Result<Option<OpenedProcess>> {
    let (handle, waitable) = match open(id, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)
    {
        Some(handle) => (handle, true),
        None => match open(id, PROCESS_QUERY_LIMITED_INFORMATION) {
            Some(handle) => (handle, false),
            None => return Ok(None),
        },
    };
    let mut length =
        u32::try_from(buffer.0.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: The handle is live with query access, the buffer is writable
    // for `length` UTF-16 units, and `length` is a valid in/out pointer for
    // this single call.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            handle.as_raw_handle(),
            PROCESS_NAME_WIN32,
            buffer.0.as_mut_ptr(),
            &raw mut length,
        )
    };
    if queried == 0 {
        return Ok(None);
    }
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length <= buffer.0.len())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok(Some(OpenedProcess {
        image: PathBuf::from(OsString::from_wide(&buffer.0[..length])),
        handle: waitable.then_some(ProcessHandle(handle)),
    }))
}

fn open(id: u32, access: u32) -> Option<OwnedHandle> {
    // SAFETY: All arguments are plain values; the call returns a new process
    // handle or null.
    let handle = unsafe { OpenProcess(access, 0, id) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: The handle was just returned, is valid and has no other owner,
    // so OwnedHandle may close it exactly once.
    Some(unsafe { OwnedHandle::from_raw_handle(handle) })
}
