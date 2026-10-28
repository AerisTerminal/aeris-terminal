//! The hidden window behind the session-end guard.

use std::{
    ptr,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::SyncSender,
    },
};
use windows_sys::Win32::{
    Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::WindowsAndMessaging::{
        CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, MSG, PM_NOREMOVE, PeekMessageW,
        PostQuitMessage, PostThreadMessageW, RegisterClassW, SetWindowLongPtrW, TranslateMessage,
        WM_ENDSESSION, WM_NCCREATE, WM_QUERYENDSESSION, WM_QUIT, WNDCLASSW,
    },
};

use crate::session::GuardWindowState;

const CLASS_NAME: &str = "AerisSessionShutdownGuard\0";

/// Runs the guard window's message loop on the calling thread until
/// [`post_quit`] or a committed session end stops it. `ready` reports whether
/// the window exists; `state` must outlive this call, which destroys the
/// window before returning.
pub(crate) fn run_guard_window(
    state: &GuardWindowState,
    thread_id: &AtomicU32,
    ready: &SyncSender<bool>,
) {
    // SAFETY: GetCurrentThreadId has no preconditions.
    thread_id.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
    let mut message = MSG::default();
    // SAFETY: `message` is a writable MSG. PM_NOREMOVE only creates this
    // thread's queue, so a WM_QUIT posted from now on reaches the loop below.
    unsafe { PeekMessageW(&raw mut message, ptr::null_mut(), 0, 0, PM_NOREMOVE) };
    let Some(window) = create_window(state) else {
        let _ = ready.try_send(false);
        return;
    };
    let _ = ready.try_send(true);
    // SAFETY: `message` is a writable MSG for each call on this thread.
    while unsafe { GetMessageW(&raw mut message, ptr::null_mut(), 0, 0) } > 0 {
        // SAFETY: `message` was just filled by GetMessageW.
        unsafe { TranslateMessage(&raw const message) };
        // SAFETY: As above; dispatch runs `window_proc` on this thread.
        unsafe { DispatchMessageW(&raw const message) };
    }
    // SAFETY: `window` was created on this thread and is destroyed once,
    // while `state`, its user data, is still alive.
    unsafe { DestroyWindow(window) };
}

fn create_window(state: &GuardWindowState) -> Option<HWND> {
    let class_name = CLASS_NAME.encode_utf16().collect::<Vec<_>>();
    // SAFETY: A null module name returns this executable's module handle
    // without taking a reference.
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class_name.as_ptr(),
        ..WNDCLASSW::default()
    };
    // SAFETY: `class` is fully initialized and its NUL-terminated name
    // outlives the call, which copies it.
    if unsafe { RegisterClassW(&raw const class) } == 0
        // SAFETY: GetLastError has no preconditions. An earlier guard in this
        // process registered the same class, which stays valid for reuse.
        && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
    {
        return None;
    }
    // SAFETY: The class is registered, both names are NUL-terminated and
    // outlive the call, and `state` outlives the window because
    // `run_guard_window` destroys it before returning.
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
            ptr::from_ref(state).cast(),
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
        // SAFETY: For WM_NCCREATE, `lparam` points at the CREATESTRUCTW of
        // this window's creation.
        let creation = unsafe { &*(lparam as *const CREATESTRUCTW) };
        // SAFETY: `window` is the window being created; this stores the
        // state pointer passed to CreateWindowExW as its user data.
        unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, creation.lpCreateParams as isize) };
        return 1;
    }
    // SAFETY: `window` is a live window of this class.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const GuardWindowState;
    // SAFETY: A non-null user-data pointer is the GuardWindowState stored at
    // WM_NCCREATE, which outlives the window.
    let state = unsafe { state.as_ref() };
    match message {
        WM_QUERYENDSESSION => LRESULT::from(state.is_some_and(GuardWindowState::query_end_session)),
        WM_ENDSESSION => {
            if let Some(state) = state {
                state.end_session(wparam != 0);
            }
            if wparam != 0 {
                // SAFETY: PostQuitMessage posts to this thread's own queue.
                unsafe { PostQuitMessage(0) };
            }
            0
        }
        // SAFETY: Other messages go to the default procedure unchanged.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Stops the guard window's message loop on `thread_id`; zero means the
/// thread never started its queue.
pub(crate) fn post_quit(thread_id: u32) {
    if thread_id != 0 {
        // SAFETY: Posting WM_QUIT takes only plain values; a thread that has
        // already exited makes the call fail harmlessly.
        unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
    }
}
