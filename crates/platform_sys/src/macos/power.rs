//! `IOKit` root power-domain notifications on a dedicated run-loop thread.

use std::{
    ffi::c_void,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{RecvTimeoutError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::power::{
    PowerCallback, RegistrationError, macos_power_message_requires_ack,
    power_event_from_macos_message_type,
};

type IoServiceInterestCallback = unsafe extern "C" fn(*mut c_void, u32, u32, *mut c_void);

const IO_SUCCESS: i32 = 0;
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(2);
const RUN_LOOP_SLICE_SECONDS: f64 = 0.1;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    #[link_name = "IORegisterForSystemPower"]
    fn io_register_for_system_power(
        refcon: *mut c_void,
        notification_port: *mut *mut c_void,
        callback: IoServiceInterestCallback,
        notifier: *mut u32,
    ) -> u32;
    #[link_name = "IODeregisterForSystemPower"]
    fn io_deregister_for_system_power(notifier: *mut u32) -> i32;
    #[link_name = "IONotificationPortGetRunLoopSource"]
    fn io_notification_port_get_run_loop_source(port: *mut c_void) -> *mut c_void;
    #[link_name = "IONotificationPortDestroy"]
    fn io_notification_port_destroy(port: *mut c_void);
    #[link_name = "IOAllowPowerChange"]
    fn io_allow_power_change(root_port: u32, notification_id: isize) -> i32;
    #[link_name = "IOServiceClose"]
    fn io_service_close(connection: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    #[link_name = "kCFRunLoopDefaultMode"]
    safe static CF_RUN_LOOP_DEFAULT_MODE: *const c_void;
    #[link_name = "CFRunLoopGetCurrent"]
    fn cf_run_loop_get_current() -> *mut c_void;
    #[link_name = "CFRunLoopAddSource"]
    fn cf_run_loop_add_source(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
    #[link_name = "CFRunLoopRemoveSource"]
    fn cf_run_loop_remove_source(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
    #[link_name = "CFRunLoopRunInMode"]
    fn cf_run_loop_run_in_mode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
}

/// The worker thread that owns the `IOKit` port and runs its run loop until
/// the registration is dropped.
pub(crate) struct Registration {
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Registration {
    pub(crate) fn register(callback: Arc<PowerCallback>) -> Result<Self, RegistrationError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (ready, registered) = sync_channel(1);
        let worker = thread::Builder::new()
            .name("aeris-power-macos".to_string())
            .spawn(move || match PowerPort::open(callback) {
                Ok(port) => {
                    if ready.send(Ok(())).is_ok() {
                        run_until_cancelled(&worker_cancelled);
                    }
                    drop(port);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|_| RegistrationError::new("power notification worker", None))?;
        match registered.recv_timeout(REGISTRATION_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                cancelled,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                Err(RegistrationError::new("power notification worker", None))
            }
            Err(RecvTimeoutError::Timeout) => {
                // A late worker finds no receiver and closes its port.
                cancelled.store(true, Ordering::Release);
                drop(worker);
                Err(RegistrationError::new(
                    "power notification registration wait",
                    None,
                ))
            }
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct CallbackContext {
    root_port: u32,
    callback: Arc<PowerCallback>,
}

/// The `IOKit` registration attached to the worker thread's run loop.
struct PowerPort {
    root_port: u32,
    notifier: u32,
    notification_port: *mut c_void,
    run_loop: *mut c_void,
    source: *mut c_void,
    context: *mut CallbackContext,
}

impl PowerPort {
    fn open(callback: Arc<PowerCallback>) -> Result<Self, RegistrationError> {
        let context = Box::into_raw(Box::new(CallbackContext {
            root_port: 0,
            callback,
        }));
        let mut notification_port = ptr::null_mut();
        let mut notifier = 0_u32;
        // SAFETY: `context` is a live allocation that outlives the
        // registration, both out-parameters are writable, and the callback
        // matches `IOServiceInterestCallback`.
        let root_port = unsafe {
            io_register_for_system_power(
                context.cast(),
                &raw mut notification_port,
                power_callback,
                &raw mut notifier,
            )
        };
        if root_port == 0 || notification_port.is_null() {
            cleanup_failed_registration(root_port, notifier, notification_port, context);
            return Err(RegistrationError::new("IORegisterForSystemPower", None));
        }
        // SAFETY: `context` is the live allocation above. Callbacks arrive
        // only through this thread's run loop, which is not running yet, so
        // nothing else references it.
        unsafe { (*context).root_port = root_port };
        // SAFETY: CFRunLoopGetCurrent has no preconditions.
        let run_loop = unsafe { cf_run_loop_get_current() };
        // SAFETY: `notification_port` is the live port IOKit just created.
        let source = unsafe { io_notification_port_get_run_loop_source(notification_port) };
        if run_loop.is_null() || source.is_null() {
            cleanup_failed_registration(root_port, notifier, notification_port, context);
            return Err(RegistrationError::new(
                "IONotificationPortGetRunLoopSource",
                None,
            ));
        }
        // SAFETY: The run loop and source are live CoreFoundation objects of
        // this thread, and the mode is CoreFoundation's exported default.
        unsafe { cf_run_loop_add_source(run_loop, source, CF_RUN_LOOP_DEFAULT_MODE) };
        Ok(Self {
            root_port,
            notifier,
            notification_port,
            run_loop,
            source,
            context,
        })
    }
}

/// Runs the current thread's run loop, which holds the power source, until
/// the registration is dropped.
fn run_until_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        // SAFETY: This runs the current thread's run loop for one bounded
        // slice.
        unsafe { cf_run_loop_run_in_mode(CF_RUN_LOOP_DEFAULT_MODE, RUN_LOOP_SLICE_SECONDS, 1) };
    }
}

impl Drop for PowerPort {
    fn drop(&mut self) {
        // SAFETY: This removes the source added in `open` from the same run
        // loop, on the thread that owns it.
        unsafe { cf_run_loop_remove_source(self.run_loop, self.source, CF_RUN_LOOP_DEFAULT_MODE) };
        // SAFETY: `notifier` is the live notifier from registration and is
        // deregistered once.
        let deregistered =
            unsafe { io_deregister_for_system_power(&raw mut self.notifier) } == IO_SUCCESS;
        // SAFETY: `root_port` is the connection registration opened, closed
        // once.
        let _ = unsafe { io_service_close(self.root_port) };
        // SAFETY: `notification_port` is the port registration created,
        // destroyed once.
        unsafe { io_notification_port_destroy(self.notification_port) };
        if deregistered {
            // SAFETY: `context` came from `Box::into_raw` in `open`, and
            // successful deregistration means IOKit no longer references it.
            drop(unsafe { Box::from_raw(self.context) });
        }
        // When IOKit refuses deregistration the context is leaked so a late
        // native callback never observes freed memory.
    }
}

fn cleanup_failed_registration(
    root_port: u32,
    mut notifier: u32,
    notification_port: *mut c_void,
    context: *mut CallbackContext,
) {
    if notifier != 0 {
        // SAFETY: A non-zero notifier came from the failed registration and is
        // deregistered once.
        let _ = unsafe { io_deregister_for_system_power(&raw mut notifier) };
    }
    if root_port != 0 {
        // SAFETY: A non-zero root port came from the failed registration and
        // is closed once.
        let _ = unsafe { io_service_close(root_port) };
    }
    if !notification_port.is_null() {
        // SAFETY: A non-null port came from the failed registration and is
        // destroyed once.
        unsafe { io_notification_port_destroy(notification_port) };
    }
    // SAFETY: `context` came from `Box::into_raw` in `open`, and no run loop
    // delivered a callback that could still reference it.
    drop(unsafe { Box::from_raw(context) });
}

unsafe extern "C" fn power_callback(
    context: *mut c_void,
    _service: u32,
    message_type: u32,
    message_argument: *mut c_void,
) {
    // SAFETY: IOKit passes back the `CallbackContext` registered in `open`,
    // which lives until deregistration succeeds.
    let Some(context) = (unsafe { context.cast::<CallbackContext>().as_ref() }) else {
        return;
    };
    if let Some(event) = power_event_from_macos_message_type(message_type) {
        context.callback.call(event);
    }
    if macos_power_message_requires_ack(message_type) {
        // SAFETY: `root_port` is the live root power-domain connection and
        // `message_argument` is this message's notification identifier.
        let _ = unsafe { io_allow_power_change(context.root_port, message_argument as isize) };
    }
}
