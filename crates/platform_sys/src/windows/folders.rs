//! Known-folder lookup through the shell.

use std::{ffi::OsString, os::windows::ffi::OsStringExt as _, path::PathBuf, ptr};
use windows_sys::Win32::{
    System::Com::CoTaskMemFree,
    UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};

pub(crate) fn downloads_directory() -> Option<PathBuf> {
    let mut path = ptr::null_mut();
    // SAFETY: The folder identifier is a static GUID, a null token selects the calling user,
    // and `path` is a writable pointer the shell sets to a string it allocates.
    let result = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_Downloads,
            KF_FLAG_DEFAULT.cast_unsigned(),
            ptr::null_mut(),
            &raw mut path,
        )
    };
    let folder = (result >= 0 && !path.is_null()).then(|| {
        let mut length = 0;
        // SAFETY: A successful call returns a NUL-terminated UTF-16 string, so every unit up
        // to and including the terminator is readable.
        while unsafe { path.wrapping_add(length).read() } != 0 {
            length += 1;
        }
        // SAFETY: The `length` units before the terminator were just read from this buffer,
        // which stays allocated until it is freed below.
        let units = unsafe { std::slice::from_raw_parts(path, length) };
        PathBuf::from(OsString::from_wide(units))
    });
    // SAFETY: The shell documents that the caller frees this buffer with `CoTaskMemFree`
    // whether or not the call succeeded; a null pointer is accepted and nothing uses it after.
    unsafe { CoTaskMemFree(path.cast_const().cast()) };
    folder.filter(|folder| folder.is_absolute())
}
