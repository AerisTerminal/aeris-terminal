use std::net::TcpStream;

/// Requests cancellation of pending operating-system I/O on `stream`.
#[must_use]
pub fn cancel_tcp_stream_io(stream: &TcpStream) -> bool {
    platform::cancel_stream(stream)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod platform {
    use std::{net::TcpStream, os::windows::io::AsRawSocket, ptr};
    use windows_sys::Win32::System::IO::CancelIoEx;

    pub(super) fn cancel_stream(stream: &TcpStream) -> bool {
        let handle = stream.as_raw_socket() as windows_sys::Win32::Foundation::HANDLE;
        // The borrowed TcpStream keeps the socket handle valid, and CancelIoEx
        // neither closes nor takes ownership of it.
        unsafe { CancelIoEx(handle, ptr::null()) != 0 }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use std::net::TcpStream;

    pub(super) const fn cancel_stream(_stream: &TcpStream) -> bool {
        false
    }
}
