//! Windows session-end veto for caller-owned durability claims.
//!
//! A hidden top-level window answers `WM_QUERYENDSESSION` on its own thread
//! before GUI shutdown begins, so a caller can hold a logoff or shutdown until
//! its claim is safe to keep through process exit.

use std::{
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc::sync_channel,
    },
    thread::{self, JoinHandle},
};

use crate::windows::session as native;

type ShutdownGate = Arc<dyn Fn() -> Option<NativeSessionShutdownPermit> + Send + Sync + 'static>;

/// Opaque owner token held while Windows is deciding whether the current user
/// session may end.
///
/// Dropping the token cancels the caller-owned shutdown claim. When Windows
/// commits the session end the guard intentionally leaks the token so its
/// claim remains active until process termination.
pub struct NativeSessionShutdownPermit {
    guard: Option<Box<dyn Send + 'static>>,
}

impl NativeSessionShutdownPermit {
    /// Wraps a caller-owned claim whose `Drop` cancels the pending shutdown
    /// ownership.
    #[must_use]
    pub fn new<T>(guard: T) -> Self
    where
        T: Send + 'static,
    {
        Self {
            guard: Some(Box::new(guard)),
        }
    }

    fn commit(mut self) {
        if let Some(guard) = self.guard.take() {
            std::mem::forget(guard);
        }
    }
}

/// Windows session-end gate that can veto logout/shutdown until a caller-owned
/// durability claim is safe to retain through process exit.
pub struct NativeSessionShutdownGuard {
    thread_id: Arc<AtomicU32>,
    worker: Option<JoinHandle<()>>,
}

impl NativeSessionShutdownGuard {
    /// Installs a hidden native window that participates in
    /// `WM_QUERYENDSESSION` before GUI shutdown begins.
    ///
    /// The callback runs on the guard's native worker thread, never the GUI
    /// thread. Returning `None` vetoes the current session-end attempt. A
    /// returned permit stays alive until Windows either cancels the attempt or
    /// commits `WM_ENDSESSION`.
    ///
    /// # Errors
    /// Returns an error if the native guard window cannot be started and made
    /// ready before this function returns.
    pub fn connect(
        gate: impl Fn() -> Option<NativeSessionShutdownPermit> + Send + Sync + 'static,
    ) -> Result<Self, SessionShutdownError> {
        let gate: ShutdownGate = Arc::new(gate);
        let thread_id = Arc::new(AtomicU32::new(0));
        let worker_thread_id = Arc::clone(&thread_id);
        let (ready_tx, ready_rx) = sync_channel(1);
        let worker = thread::Builder::new()
            .name("aeris-session-shutdown-guard".to_string())
            .spawn(move || {
                let state = GuardWindowState {
                    gate,
                    permit: Mutex::new(None),
                };
                native::run_guard_window(&state, &worker_thread_id, &ready_tx);
            })
            .map_err(|_| SessionShutdownError::ThreadStart)?;
        if ready_rx.recv().ok() != Some(true) {
            native::post_quit(thread_id.load(Ordering::Acquire));
            let _ = worker.join();
            return Err(SessionShutdownError::NativeRegistration);
        }
        Ok(Self {
            thread_id,
            worker: Some(worker),
        })
    }
}

impl Drop for NativeSessionShutdownGuard {
    fn drop(&mut self) {
        native::post_quit(self.thread_id.load(Ordering::Acquire));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The guard window's user data: the caller's gate and the permit it granted
/// for the session end currently being decided.
pub(crate) struct GuardWindowState {
    gate: ShutdownGate,
    permit: Mutex<Option<NativeSessionShutdownPermit>>,
}

impl GuardWindowState {
    /// Answers `WM_QUERYENDSESSION`: `true` lets the session end.
    pub(crate) fn query_end_session(&self) -> bool {
        let Ok(mut permit) = self.permit.lock() else {
            return false;
        };
        if permit.is_some() {
            return true;
        }
        let next = catch_unwind(AssertUnwindSafe(|| (self.gate)()))
            .ok()
            .flatten();
        if let Some(next) = next {
            *permit = Some(next);
            true
        } else {
            false
        }
    }

    /// Handles `WM_ENDSESSION`: a committed end keeps the permit alive
    /// through process exit, a cancelled one releases it.
    pub(crate) fn end_session(&self, committed: bool) {
        let Ok(mut permit) = self.permit.lock() else {
            return;
        };
        if let Some(permit) = permit.take()
            && committed
        {
            permit.commit();
        }
    }
}

/// Redacted native session-shutdown guard failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionShutdownError {
    NativeRegistration,
    ThreadStart,
}

impl fmt::Display for SessionShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NativeRegistration => "session shutdown guard registration failed",
            Self::ThreadStart => "session shutdown guard worker could not start",
        })
    }
}

impl Error for SessionShutdownError {}

#[cfg(test)]
mod tests {
    use super::{GuardWindowState, NativeSessionShutdownGuard, NativeSessionShutdownPermit};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn cancelled_session_end_releases_permit_and_allows_a_fresh_query() {
        let calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let gate_calls = Arc::clone(&calls);
        let gate_dropped = Arc::clone(&dropped);
        let state = GuardWindowState {
            gate: Arc::new(move || {
                gate_calls.fetch_add(1, Ordering::AcqRel);
                Some(NativeSessionShutdownPermit::new(DropFlag(Arc::clone(
                    &gate_dropped,
                ))))
            }),
            permit: Mutex::new(None),
        };

        assert!(state.query_end_session());
        assert!(state.query_end_session());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert!(!dropped.load(Ordering::Acquire));

        state.end_session(false);
        assert!(dropped.load(Ordering::Acquire));
        assert!(state.query_end_session());
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn committed_session_end_retains_permit_through_process_exit() {
        let dropped = Arc::new(AtomicBool::new(false));
        let gate_dropped = Arc::clone(&dropped);
        let state = GuardWindowState {
            gate: Arc::new(move || {
                Some(NativeSessionShutdownPermit::new(DropFlag(Arc::clone(
                    &gate_dropped,
                ))))
            }),
            permit: Mutex::new(None),
        };

        assert!(state.query_end_session());
        state.end_session(true);
        assert!(
            !dropped.load(Ordering::Acquire),
            "committed session shutdown must keep the caller-owned quiesce claim alive"
        );
    }

    #[test]
    fn rejected_session_end_is_vetoed_without_retaining_a_permit() {
        let state = GuardWindowState {
            gate: Arc::new(|| None),
            permit: Mutex::new(None),
        };

        assert!(!state.query_end_session());
        assert!(state.permit.lock().expect("permit state locks").is_none());
    }

    #[test]
    fn a_panicking_gate_vetoes_the_session_end() {
        let state = GuardWindowState {
            gate: Arc::new(|| panic!("gate failure")),
            permit: Mutex::new(None),
        };

        assert!(!state.query_end_session());
    }

    #[test]
    fn guards_start_and_stop_repeatedly_in_one_process() {
        for _ in 0..2 {
            let guard = NativeSessionShutdownGuard::connect(|| None).expect("guard window starts");
            drop(guard);
        }
    }
}
