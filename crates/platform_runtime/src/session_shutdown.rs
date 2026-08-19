//! Native system and user-session shutdown notifications.

use crate::CapabilityAvailability;
use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, sync_channel},
    },
    thread::{self, JoinHandle},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
    iterator::{Handle as SignalHandle, Signals},
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::mpsc::SyncSender;

#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use zbus::{
    MatchRule, MessageType,
    blocking::{Connection, MessageIterator},
};

const EVENT_CAPACITY: usize = 1;

#[cfg(target_os = "linux")]
const LOGIND_DESTINATION: &str = "org.freedesktop.login1";
#[cfg(target_os = "linux")]
const LOGIND_PATH: &str = "/org/freedesktop/login1";
#[cfg(target_os = "linux")]
const LOGIND_MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
#[cfg(target_os = "linux")]
const PREPARE_FOR_SHUTDOWN_SIGNAL: &str = "PrepareForShutdown";

/// Blocking native notification source for system shutdown or user-session exit.
pub struct NativeSessionShutdownMonitor {
    cancellation: NativeSessionShutdownCancellation,
    events: Receiver<()>,
    workers: Vec<JoinHandle<()>>,
}

/// Handle that unblocks a matching [`NativeSessionShutdownMonitor`].
#[derive(Clone)]
pub struct NativeSessionShutdownCancellation {
    inner: Arc<CancellationInner>,
}

struct CancellationInner {
    cancelled: AtomicBool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    signal_handle: SignalHandle,
    #[cfg(target_os = "linux")]
    logind_connection: Mutex<Option<Connection>>,
    #[cfg(target_os = "windows")]
    windows_thread_id: std::sync::atomic::AtomicU32,
}

impl NativeSessionShutdownCancellation {
    /// Cancels all native waits and allows their owner to join them.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.inner.signal_handle.close();
        #[cfg(target_os = "linux")]
        if let Ok(connection) = self.inner.logind_connection.lock()
            && let Some(connection) = connection.as_ref()
        {
            let _ = connection.close();
        }
        #[cfg(target_os = "windows")]
        windows::cancel_wait(self.inner.windows_thread_id.load(Ordering::Acquire));
    }
}

impl NativeSessionShutdownMonitor {
    /// Reports whether this target has a native system/session shutdown source.
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        return CapabilityAvailability::Available;

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        CapabilityAvailability::Unavailable
    }

    /// Starts the bounded native notification helpers for this process.
    ///
    /// Linux combines systemd-logind's early `PrepareForShutdown` notification
    /// with process termination signals so logout and service-stop paths are
    /// also covered. macOS uses launchd/session termination signals. Windows
    /// owns an invisible top-level window so broadcast session-end messages are
    /// received even though the release engine has no console.
    ///
    /// # Errors
    /// Returns an error when the target is unsupported or its required native
    /// signal/window helper cannot be started.
    pub fn connect() -> Result<Self, SessionShutdownError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return unix_monitor();

        #[cfg(target_os = "windows")]
        return windows_monitor();

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(SessionShutdownError::UnsupportedPlatform)
    }

    /// Returns a handle that wakes every native helper from another thread.
    #[must_use]
    pub fn cancellation(&self) -> NativeSessionShutdownCancellation {
        self.cancellation.clone()
    }

    /// Blocks until the OS begins system shutdown/user-session exit or cancellation.
    ///
    /// # Errors
    /// Returns an error when cancelled or every native event source exits.
    pub fn wait_for_shutdown(&self) -> Result<(), SessionShutdownError> {
        match self.events.recv() {
            Ok(()) => Ok(()),
            Err(_) if self.cancellation.inner.cancelled.load(Ordering::Acquire) => {
                Err(SessionShutdownError::Cancelled)
            }
            Err(_) => Err(SessionShutdownError::StreamClosed),
        }
    }
}

impl Drop for NativeSessionShutdownMonitor {
    fn drop(&mut self) {
        self.cancellation.cancel();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unix_monitor() -> Result<NativeSessionShutdownMonitor, SessionShutdownError> {
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])
        .map_err(|_| SessionShutdownError::NativeRegistration)?;
    let signal_handle = signals.handle();
    let (events, receiver) = sync_channel(EVENT_CAPACITY);
    let cancelled = Arc::new(CancellationInner {
        cancelled: AtomicBool::new(false),
        signal_handle,
        #[cfg(target_os = "linux")]
        logind_connection: Mutex::new(None),
    });
    let mut workers = Vec::with_capacity(if cfg!(target_os = "linux") { 2 } else { 1 });
    workers.push(spawn_signal_worker(
        signals,
        events.clone(),
        Arc::clone(&cancelled),
    )?);
    #[cfg(target_os = "linux")]
    if let Some(worker) = spawn_logind_worker(events, &cancelled)? {
        workers.push(worker);
    }
    Ok(NativeSessionShutdownMonitor {
        cancellation: NativeSessionShutdownCancellation { inner: cancelled },
        events: receiver,
        workers,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_signal_worker(
    mut signals: Signals,
    events: SyncSender<()>,
    cancellation: Arc<CancellationInner>,
) -> Result<JoinHandle<()>, SessionShutdownError> {
    thread::Builder::new()
        .name("axiusflow-session-signal".to_string())
        .spawn(move || {
            if signals.forever().next().is_some() && !cancellation.cancelled.load(Ordering::Acquire)
            {
                let _ = events.try_send(());
            }
        })
        .map_err(|_| SessionShutdownError::ThreadStart)
}

#[cfg(target_os = "linux")]
fn spawn_logind_worker(
    events: SyncSender<()>,
    cancellation: &Arc<CancellationInner>,
) -> Result<Option<JoinHandle<()>>, SessionShutdownError> {
    let Ok(connection) = Connection::system() else {
        return Ok(None);
    };
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(LOGIND_DESTINATION)
        .map_err(|_| SessionShutdownError::NativeRegistration)?
        .path(LOGIND_PATH)
        .map_err(|_| SessionShutdownError::NativeRegistration)?
        .interface(LOGIND_MANAGER_INTERFACE)
        .map_err(|_| SessionShutdownError::NativeRegistration)?
        .member(PREPARE_FOR_SHUTDOWN_SIGNAL)
        .map_err(|_| SessionShutdownError::NativeRegistration)?
        .build();
    let messages = MessageIterator::for_match_rule(rule, &connection, Some(EVENT_CAPACITY))
        .map_err(|_| SessionShutdownError::NativeRegistration)?;
    *cancellation
        .logind_connection
        .lock()
        .map_err(|_| SessionShutdownError::NativeRegistration)? = Some(connection);
    let cancellation = Arc::clone(cancellation);
    thread::Builder::new()
        .name("axiusflow-session-logind".to_string())
        .spawn(move || run_logind(messages, events, &cancellation))
        .map(Some)
        .map_err(|_| SessionShutdownError::ThreadStart)
}

#[cfg(target_os = "linux")]
fn run_logind(
    mut messages: MessageIterator,
    events: SyncSender<()>,
    cancellation: &CancellationInner,
) {
    while !cancellation.cancelled.load(Ordering::Acquire) {
        let Some(Ok(message)) = messages.next() else {
            return;
        };
        if message
            .body()
            .deserialize::<bool>()
            .is_ok_and(|value| value)
        {
            let _ = events.try_send(());
            return;
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_monitor() -> Result<NativeSessionShutdownMonitor, SessionShutdownError> {
    let (events, receiver) = sync_channel(EVENT_CAPACITY);
    let cancelled = Arc::new(CancellationInner {
        cancelled: AtomicBool::new(false),
        windows_thread_id: std::sync::atomic::AtomicU32::new(0),
    });
    let worker_cancellation = Arc::clone(&cancelled);
    let worker = thread::Builder::new()
        .name("axiusflow-session-window".to_string())
        .spawn(move || windows::run(&events, &worker_cancellation))
        .map_err(|_| SessionShutdownError::ThreadStart)?;
    Ok(NativeSessionShutdownMonitor {
        cancellation: NativeSessionShutdownCancellation { inner: cancelled },
        events: receiver,
        workers: vec![worker],
    })
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use super::CancellationInner;
    use std::{
        ptr,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::SyncSender,
        },
    };
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, MSG, PM_NOREMOVE, PeekMessageW,
            PostQuitMessage, PostThreadMessageW, RegisterClassW, SetWindowLongPtrW,
            TranslateMessage, WM_ENDSESSION, WM_NCCREATE, WM_QUERYENDSESSION, WM_QUIT, WNDCLASSW,
        },
    };

    pub(super) fn run(events: &SyncSender<()>, cancellation: &Arc<CancellationInner>) {
        let thread_id = unsafe { GetCurrentThreadId() };
        cancellation
            .windows_thread_id
            .store(thread_id, Ordering::Release);
        let mut message = MSG::default();
        unsafe {
            PeekMessageW(&raw mut message, ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
        if cancellation.cancelled.load(Ordering::Acquire) {
            return;
        }
        let requested = AtomicBool::new(false);
        let Some(window) = create_window(&requested) else {
            return;
        };
        while unsafe { GetMessageW(&raw mut message, ptr::null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        unsafe {
            DestroyWindow(window);
        }
        if requested.load(Ordering::Acquire) && !cancellation.cancelled.load(Ordering::Acquire) {
            let _ = events.try_send(());
        }
    }

    fn create_window(requested: &AtomicBool) -> Option<HWND> {
        let class_name = "AxiusflowEngineSessionMonitor\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        let instance = unsafe { GetModuleHandleW(ptr::null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..WNDCLASSW::default()
        };
        if unsafe { RegisterClassW(&raw const class) } == 0 {
            return None;
        }
        let window = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::from_ref(requested).cast(),
            )
        };
        (!window.is_null()).then_some(window)
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let creation = unsafe { &*(lparam as *const CREATESTRUCTW) };
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, creation.lpCreateParams as isize);
            }
            return 1;
        }
        if message == WM_QUERYENDSESSION || message == WM_ENDSESSION && wparam != 0 {
            let requested =
                unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const AtomicBool;
            if !requested.is_null() {
                unsafe { &*requested }.store(true, Ordering::Release);
            }
            unsafe { PostQuitMessage(0) };
            return 1;
        }
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    pub(super) fn cancel_wait(thread_id: u32) {
        if thread_id != 0 {
            unsafe {
                PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
            }
        }
    }
}

/// Redacted native system/session shutdown notification failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionShutdownError {
    UnsupportedPlatform,
    NativeRegistration,
    ThreadStart,
    Cancelled,
    StreamClosed,
}

impl fmt::Display for SessionShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = match self {
            Self::UnsupportedPlatform => "session shutdown notifications are unavailable",
            Self::NativeRegistration => "session shutdown notification registration failed",
            Self::ThreadStart => "session shutdown notification worker could not start",
            Self::Cancelled => "session shutdown notification wait was cancelled",
            Self::StreamClosed => "session shutdown notification sources closed unexpectedly",
        };
        formatter.write_str(detail)
    }
}

impl Error for SessionShutdownError {}

#[cfg(test)]
mod tests {
    use super::{NativeSessionShutdownMonitor, SessionShutdownError};
    use std::time::{Duration, Instant};

    #[test]
    fn cancellation_unblocks_native_session_shutdown_waiters() {
        let monitor = NativeSessionShutdownMonitor::connect().expect("native shutdown monitor");
        let cancellation = monitor.cancellation();
        cancellation.cancel();
        let started = Instant::now();
        assert_eq!(
            monitor.wait_for_shutdown(),
            Err(SessionShutdownError::Cancelled)
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
