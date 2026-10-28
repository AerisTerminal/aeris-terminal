#![allow(unsafe_code)]

pub(crate) mod process;
pub(crate) mod trust;

use std::{io, os::windows::ffi::OsStrExt as _, path::Path};
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW,
};

pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    let mut source = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source.push(0);
    let mut destination = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    destination.push(0);

    // SAFETY: Both paths are stable, NUL-terminated UTF-16 buffers for the
    // duration of this call, and MoveFileExW does not retain either pointer.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn message_box(title: &str, message: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    let mut title = std::ffi::OsStr::new(title)
        .encode_wide()
        .collect::<Vec<_>>();
    title.push(0);
    let mut message = std::ffi::OsStr::new(message)
        .encode_wide()
        .collect::<Vec<_>>();
    message.push(0);
    // SAFETY: The title and message are stable, NUL-terminated UTF-16 buffers
    // for this single call, and MessageBoxW does not retain either pointer.
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn confirm_message_box(title: &str, message: &str) -> io::Result<bool> {
    let mut title = std::ffi::OsStr::new(title)
        .encode_wide()
        .collect::<Vec<_>>();
    title.push(0);
    let mut message = std::ffi::OsStr::new(message)
        .encode_wide()
        .collect::<Vec<_>>();
    message.push(0);
    // SAFETY: The title and message are stable, NUL-terminated UTF-16 buffers
    // for this single call, and MessageBoxW does not retain either pointer.
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result == IDYES)
    }
}
