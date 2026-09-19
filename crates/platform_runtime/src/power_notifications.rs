//! Native suspend and resume notification boundary.

use crate::CapabilityAvailability;
use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::{
    atomic::AtomicU8,
    mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
};

#[cfg(target_os = "linux")]
use zbus::{
    MatchRule,
    blocking::{Connection, MessageIterator},
    message::Type as MessageType,
};

#[cfg(target_os = "linux")]
const LOGIND_DESTINATION: &str = "org.freedesktop.login1";
#[cfg(target_os = "linux")]
const LOGIND_PATH: &str = "/org/freedesktop/login1";
#[cfg(target_os = "linux")]
const LOGIND_MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
#[cfg(target_os = "linux")]
const PREPARE_FOR_SLEEP_SIGNAL: &str = "PrepareForSleep";
#[cfg(target_os = "linux")]
const MAX_QUEUED_POWER_EVENTS: usize = 16;

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct PowerEventPublisher {
    pending: Arc<AtomicU8>,
    wake: SyncSender<()>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl PowerEventPublisher {
    fn publish(&self, event: PowerEvent) {
        match event {
            PowerEvent::Suspending => self.pending.store(SUSPEND_PENDING, Ordering::Release),
            PowerEvent::Resumed => {
                self.pending.fetch_or(RESUME_PENDING, Ordering::AcqRel);
            }
        }
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct PowerEventInbox {
    pending: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
    wake: Receiver<()>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl PowerEventInbox {
    fn channel() -> (
        Arc<PowerEventPublisher>,
        Self,
        NativePowerMonitorCancellation,
    ) {
        let pending = Arc::new(AtomicU8::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (wake, receiver) = sync_channel(1);
        (
            Arc::new(PowerEventPublisher {
                pending: Arc::clone(&pending),
                wake: wake.clone(),
            }),
            Self {
                pending,
                cancelled: Arc::clone(&cancelled),
                wake: receiver,
            },
            NativePowerMonitorCancellation { cancelled, wake },
        )
    }

    fn recv(&self) -> Result<PowerEvent, PowerNotificationError> {
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(PowerNotificationError::Cancelled);
            }
            if let Some(event) = self.take_next() {
                return Ok(event);
            }
            self.wake
                .recv()
                .map_err(|_| PowerNotificationError::StreamClosed)?;
        }
    }

    #[cfg(all(test, target_os = "windows"))]
    fn try_recv(&self) -> Option<PowerEvent> {
        self.take_next()
    }

    fn take_next(&self) -> Option<PowerEvent> {
        loop {
            let pending = self.pending.load(Ordering::Acquire);
            let (event, retained) = if pending & SUSPEND_PENDING != 0 {
                (PowerEvent::Suspending, pending & !SUSPEND_PENDING)
            } else if pending & RESUME_PENDING != 0 {
                (PowerEvent::Resumed, pending & !RESUME_PENDING)
            } else {
                return None;
            };
            if self
                .pending
                .compare_exchange(pending, retained, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(event);
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
const SUSPEND_PENDING: u8 = 1;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const RESUME_PENDING: u8 = 2;

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    // The callback context is an Arc kept alive until Win32 confirms
    // unregistration. Failed unregistration leaks that Arc deliberately so a
    // late operating-system callback can never dereference freed memory.
    use super::{PowerEvent, PowerEventInbox, PowerEventPublisher, PowerNotificationError};
    use std::{ffi::c_void, ptr, sync::Arc};
    use windows_sys::Win32::{
        Foundation::{ERROR_SUCCESS, HANDLE},
        System::Power::{
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, PowerRegisterSuspendResumeNotification,
            PowerUnregisterSuspendResumeNotification,
        },
        UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND},
    };

    pub(super) struct Registration {
        handle: isize,
        parameters: usize,
        context: Option<Arc<PowerEventPublisher>>,
    }

    impl Registration {
        pub(super) fn connect() -> Result<
            (Self, PowerEventInbox, super::NativePowerMonitorCancellation),
            PowerNotificationError,
        > {
            let (publisher, events, cancellation) = PowerEventInbox::channel();
            let context = publisher;
            let mut parameters = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(power_callback),
                Context: Arc::as_ptr(&context).cast_mut().cast(),
            });
            let mut handle = ptr::null_mut();
            // SAFETY: parameters and its Arc context remain valid for the
            // registration lifetime, and the callback matches Win32's ABI.
            let result = unsafe {
                PowerRegisterSuspendResumeNotification(
                    DEVICE_NOTIFY_CALLBACK,
                    (&raw mut *parameters).cast::<c_void>() as HANDLE,
                    &raw mut handle,
                )
            };
            if result != ERROR_SUCCESS {
                return Err(PowerNotificationError::WindowsPlatform {
                    operation: "PowerRegisterSuspendResumeNotification",
                    code: result,
                });
            }
            Ok((
                Self {
                    handle: handle.addr().cast_signed(),
                    parameters: Box::into_raw(parameters).addr(),
                    context: Some(context),
                },
                events,
                cancellation,
            ))
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: handle was returned by the matching registration call.
            let result = unsafe { PowerUnregisterSuspendResumeNotification(self.handle) };
            if result == ERROR_SUCCESS {
                // SAFETY: this address came from Box::into_raw above, and
                // successful unregistration means Win32 no longer uses it.
                drop(unsafe {
                    Box::from_raw(self.parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS)
                });
            } else if let Some(context) = self.context.take() {
                let _ = Arc::into_raw(context);
            }
        }
    }

    unsafe extern "system" fn power_callback(
        context: *const c_void,
        event_type: u32,
        _setting: *const c_void,
    ) -> u32 {
        let Some(event) = PowerEvent::from_windows_event_type(event_type) else {
            return ERROR_SUCCESS;
        };
        // SAFETY: registration pins this sender until unregistration succeeds;
        // on failure it is leaked. The callback only takes a shared reference.
        let events = unsafe { &*context.cast::<PowerEventPublisher>() };
        events.publish(event);
        ERROR_SUCCESS
    }

    pub(super) const fn is_resume_event(event_type: u32) -> bool {
        event_type == PBT_APMRESUMEAUTOMATIC
    }

    pub(super) const fn is_suspend_event(event_type: u32) -> bool {
        event_type == PBT_APMSUSPEND
    }
}

#[cfg(any(target_os = "macos", test))]
const MACOS_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
#[cfg(any(target_os = "macos", test))]
const MACOS_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
#[cfg(any(target_os = "macos", test))]
const MACOS_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

#[cfg(any(target_os = "macos", test))]
const fn macos_power_message_requires_ack(message_type: u32) -> bool {
    matches!(
        message_type,
        MACOS_CAN_SYSTEM_SLEEP | MACOS_SYSTEM_WILL_SLEEP
    )
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use super::{
        NativePowerMonitorCancellation, PowerEvent, PowerEventPublisher, PowerNotificationError,
    };
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
        static CF_RUN_LOOP_DEFAULT_MODE: *const c_void;
        #[link_name = "CFRunLoopGetCurrent"]
        fn cf_run_loop_get_current() -> *mut c_void;
        #[link_name = "CFRunLoopAddSource"]
        fn cf_run_loop_add_source(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
        #[link_name = "CFRunLoopRemoveSource"]
        fn cf_run_loop_remove_source(
            run_loop: *mut c_void,
            source: *mut c_void,
            mode: *const c_void,
        );
        #[link_name = "CFRunLoopRunInMode"]
        fn cf_run_loop_run_in_mode(
            mode: *const c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
    }

    struct CallbackContext {
        root_port: u32,
        publisher: Arc<PowerEventPublisher>,
    }

    struct Registration {
        root_port: u32,
        notifier: u32,
        notification_port: *mut c_void,
        run_loop: *mut c_void,
        source: *mut c_void,
        context: *mut CallbackContext,
    }

    impl Registration {
        fn connect(publisher: Arc<PowerEventPublisher>) -> Result<Self, PowerNotificationError> {
            let context = Box::into_raw(Box::new(CallbackContext {
                root_port: 0,
                publisher,
            }));
            let mut notification_port = ptr::null_mut();
            let mut notifier = 0_u32;
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
                return Err(PowerNotificationError::MacOsPlatform(
                    "IORegisterForSystemPower",
                ));
            }
            unsafe {
                (*context).root_port = root_port;
            }
            let run_loop = unsafe { cf_run_loop_get_current() };
            let source = unsafe { io_notification_port_get_run_loop_source(notification_port) };
            if run_loop.is_null() || source.is_null() {
                cleanup_failed_registration(root_port, notifier, notification_port, context);
                return Err(PowerNotificationError::MacOsPlatform(
                    "IONotificationPortGetRunLoopSource",
                ));
            }
            unsafe {
                cf_run_loop_add_source(run_loop, source, CF_RUN_LOOP_DEFAULT_MODE);
            }
            Ok(Self {
                root_port,
                notifier,
                notification_port,
                run_loop,
                source,
                context,
            })
        }

        fn run(self, cancelled: &AtomicBool) {
            let _registration = self;
            while !cancelled.load(Ordering::Acquire) {
                unsafe {
                    cf_run_loop_run_in_mode(CF_RUN_LOOP_DEFAULT_MODE, RUN_LOOP_SLICE_SECONDS, 1);
                }
            }
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            unsafe {
                cf_run_loop_remove_source(self.run_loop, self.source, CF_RUN_LOOP_DEFAULT_MODE);
            }
            let deregistered =
                unsafe { io_deregister_for_system_power(&raw mut self.notifier) } == IO_SUCCESS;
            unsafe {
                let _ = io_service_close(self.root_port);
                io_notification_port_destroy(self.notification_port);
            }
            if deregistered {
                unsafe {
                    drop(Box::from_raw(self.context));
                }
            }
            // If IOKit refuses deregistration, retain the callback context.
            // This mirrors the Windows backend's fail-safe lifetime rule: a
            // late native callback must never observe freed memory.
        }
    }

    pub(super) fn connect(
        publisher: Arc<PowerEventPublisher>,
        cancellation: &NativePowerMonitorCancellation,
    ) -> Result<JoinHandle<()>, PowerNotificationError> {
        let cancelled = Arc::clone(&cancellation.cancelled);
        let (ready, registered) = sync_channel(1);
        let worker = thread::Builder::new()
            .name("tradingplot-power-macos".to_string())
            .spawn(move || match Registration::connect(publisher) {
                Ok(registration) => {
                    if ready.send(Ok(())).is_ok() {
                        registration.run(&cancelled);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|_| PowerNotificationError::MacOsPlatform("power notification worker"))?;
        match registered.recv_timeout(REGISTRATION_TIMEOUT) {
            Ok(Ok(())) => Ok(worker),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                Err(PowerNotificationError::StreamClosed)
            }
            Err(RecvTimeoutError::Timeout) => {
                cancellation.clone().cancel();
                drop(worker);
                Err(PowerNotificationError::MacOsPlatform(
                    "power notification registration timed out",
                ))
            }
        }
    }

    fn cleanup_failed_registration(
        root_port: u32,
        mut notifier: u32,
        notification_port: *mut c_void,
        context: *mut CallbackContext,
    ) {
        if notifier != 0 {
            let _ = unsafe { io_deregister_for_system_power(&raw mut notifier) };
        }
        if root_port != 0 {
            let _ = unsafe { io_service_close(root_port) };
        }
        if !notification_port.is_null() {
            unsafe {
                io_notification_port_destroy(notification_port);
            }
        }
        unsafe {
            drop(Box::from_raw(context));
        }
    }

    unsafe extern "C" fn power_callback(
        context: *mut c_void,
        _service: u32,
        message_type: u32,
        message_argument: *mut c_void,
    ) {
        let Some(context) = (unsafe { context.cast::<CallbackContext>().as_ref() }) else {
            return;
        };
        if let Some(event) = PowerEvent::from_macos_message_type(message_type) {
            context.publisher.publish(event);
        }
        if super::macos_power_message_requires_ack(message_type) {
            let _ = unsafe { io_allow_power_change(context.root_port, message_argument as isize) };
        }
    }
}

/// A native system power transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerEvent {
    Suspending,
    Resumed,
}

impl PowerEvent {
    #[cfg(any(target_os = "linux", test))]
    const fn from_preparing_for_sleep(preparing: bool) -> Self {
        if preparing {
            Self::Suspending
        } else {
            Self::Resumed
        }
    }

    #[cfg(any(target_os = "windows", test))]
    const fn from_windows_event_type(event_type: u32) -> Option<Self> {
        #[cfg(target_os = "windows")]
        {
            if windows::is_suspend_event(event_type) {
                return Some(Self::Suspending);
            }
            if windows::is_resume_event(event_type) {
                return Some(Self::Resumed);
            }
        }

        #[cfg(all(test, not(target_os = "windows")))]
        {
            if event_type == 4 {
                return Some(Self::Suspending);
            }
            if event_type == 18 {
                return Some(Self::Resumed);
            }
        }
        None
    }

    #[cfg(any(target_os = "macos", test))]
    const fn from_macos_message_type(message_type: u32) -> Option<Self> {
        if message_type == MACOS_SYSTEM_WILL_SLEEP {
            return Some(Self::Suspending);
        }
        if message_type == MACOS_SYSTEM_HAS_POWERED_ON {
            return Some(Self::Resumed);
        }
        None
    }
}

/// Blocking native power-event listener.
///
/// On Linux this subscribes to systemd-logind's `PrepareForSleep` signal on the
/// system bus. On macOS it consumes `IOKit`'s root power-domain notifications on a
/// dedicated CoreFoundation run loop. On Windows it registers a suspend/resume
/// callback with the power manager. Callback-based backends coalesce bursts while
/// preserving suspend before resume. Callers must run [`Self::next_event`] outside
/// async executors and UI threads because it blocks until a transition arrives.
pub struct NativePowerMonitor {
    cancellation: NativePowerMonitorCancellation,
    #[cfg(target_os = "linux")]
    messages: MessageIterator,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    events: PowerEventInbox,
    #[cfg(target_os = "windows")]
    _registration: windows::Registration,
    #[cfg(target_os = "macos")]
    worker: Option<std::thread::JoinHandle<()>>,
}

/// Handle that unblocks a [`NativePowerMonitor`] waiting for its next event.
#[derive(Clone)]
pub struct NativePowerMonitorCancellation {
    cancelled: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    connection: Connection,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    wake: SyncSender<()>,
}

impl NativePowerMonitorCancellation {
    /// Cancels the matching monitor's blocking wait.
    pub fn cancel(self) {
        self.cancelled.store(true, Ordering::Release);
        #[cfg(target_os = "linux")]
        {
            let _ = self.connection.close();
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
        }
    }
}

impl NativePowerMonitor {
    /// Reports whether this crate implements a native power-event source for the target.
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        return CapabilityAvailability::Available;

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        CapabilityAvailability::Unavailable
    }

    /// Connects to the native power-event source.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is unsupported or its native power
    /// notification registration cannot be installed.
    pub fn connect() -> Result<Self, PowerNotificationError> {
        #[cfg(target_os = "linux")]
        {
            let connection = Connection::system().map_err(PowerNotificationError::Platform)?;
            let rule = MatchRule::builder()
                .msg_type(MessageType::Signal)
                .sender(LOGIND_DESTINATION)
                .map_err(PowerNotificationError::Platform)?
                .path(LOGIND_PATH)
                .map_err(PowerNotificationError::Platform)?
                .interface(LOGIND_MANAGER_INTERFACE)
                .map_err(PowerNotificationError::Platform)?
                .member(PREPARE_FOR_SLEEP_SIGNAL)
                .map_err(PowerNotificationError::Platform)?
                .build();
            let messages =
                MessageIterator::for_match_rule(rule, &connection, Some(MAX_QUEUED_POWER_EVENTS))
                    .map_err(PowerNotificationError::Platform)?;
            let cancelled = Arc::new(AtomicBool::new(false));
            Ok(Self {
                cancellation: NativePowerMonitorCancellation {
                    cancelled,
                    connection: connection.clone(),
                },
                messages,
            })
        }

        #[cfg(target_os = "windows")]
        {
            let (registration, events, cancellation) = windows::Registration::connect()?;
            Ok(Self {
                cancellation,
                events,
                _registration: registration,
            })
        }

        #[cfg(target_os = "macos")]
        {
            let (publisher, events, cancellation) = PowerEventInbox::channel();
            let worker = macos::connect(publisher, &cancellation)?;
            Ok(Self {
                cancellation,
                events,
                worker: Some(worker),
            })
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(PowerNotificationError::UnsupportedPlatform)
    }

    /// Returns a handle that can unblock [`Self::next_event`] from another thread.
    #[must_use]
    pub fn cancellation(&self) -> NativePowerMonitorCancellation {
        self.cancellation.clone()
    }

    /// Blocks until the next suspend or resume transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the native stream closes, transport fails, or logind
    /// sends a signal body that does not match its documented boolean contract.
    pub fn next_event(&mut self) -> Result<PowerEvent, PowerNotificationError> {
        #[cfg(target_os = "linux")]
        {
            if self.cancellation.cancelled.load(Ordering::Acquire) {
                return Err(PowerNotificationError::Cancelled);
            }
            let message = self.messages.next();
            if self.cancellation.cancelled.load(Ordering::Acquire) {
                return Err(PowerNotificationError::Cancelled);
            }
            let message = message
                .ok_or(PowerNotificationError::StreamClosed)?
                .map_err(PowerNotificationError::Platform)?;
            let preparing = message
                .body()
                .deserialize::<bool>()
                .map_err(PowerNotificationError::Platform)?;
            Ok(PowerEvent::from_preparing_for_sleep(preparing))
        }

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.events.recv()
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(PowerNotificationError::UnsupportedPlatform)
    }
}

impl Drop for NativePowerMonitor {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.cancellation.clone().cancel();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl fmt::Debug for NativePowerMonitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativePowerMonitor")
            .field("availability", &Self::availability())
            .finish_non_exhaustive()
    }
}

/// Errors returned by native power notification monitoring.
#[derive(Debug)]
pub enum PowerNotificationError {
    #[cfg(target_os = "linux")]
    Platform(zbus::Error),
    #[cfg(target_os = "windows")]
    WindowsPlatform {
        operation: &'static str,
        code: u32,
    },
    #[cfg(target_os = "macos")]
    MacOsPlatform(&'static str),
    Cancelled,
    StreamClosed,
    UnsupportedPlatform,
}

impl fmt::Display for PowerNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            Self::Platform(error) => write!(formatter, "native power notification failed: {error}"),
            #[cfg(target_os = "windows")]
            Self::WindowsPlatform { operation, code } => write!(
                formatter,
                "native power notification operation {operation} failed with Windows error {code}"
            ),
            #[cfg(target_os = "macos")]
            Self::MacOsPlatform(operation) => write!(
                formatter,
                "native power notification operation {operation} failed on macOS"
            ),
            Self::Cancelled => formatter.write_str("native power notification wait cancelled"),
            Self::StreamClosed => formatter.write_str("native power notification stream closed"),
            Self::UnsupportedPlatform => {
                formatter.write_str("native power notifications are unavailable on this platform")
            }
        }
    }
}

impl Error for PowerNotificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Platform(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use super::PowerEventInbox;
    use super::{NativePowerMonitor, PowerEvent};
    use crate::CapabilityAvailability;

    #[test]
    fn prepare_for_sleep_boolean_maps_to_ordered_power_transitions() {
        assert_eq!(
            PowerEvent::from_preparing_for_sleep(true),
            PowerEvent::Suspending
        );
        assert_eq!(
            PowerEvent::from_preparing_for_sleep(false),
            PowerEvent::Resumed
        );
    }

    #[test]
    fn windows_power_event_types_map_without_duplicate_resume_events() {
        assert_eq!(
            PowerEvent::from_windows_event_type(4),
            Some(PowerEvent::Suspending)
        );
        assert_eq!(
            PowerEvent::from_windows_event_type(18),
            Some(PowerEvent::Resumed)
        );
        for event_type in [0, 6, 7, 8, u32::MAX] {
            assert_eq!(PowerEvent::from_windows_event_type(event_type), None);
        }
    }

    #[test]
    fn macos_power_messages_map_only_committed_sleep_and_completed_wake() {
        assert_eq!(
            PowerEvent::from_macos_message_type(super::MACOS_SYSTEM_WILL_SLEEP),
            Some(PowerEvent::Suspending)
        );
        assert_eq!(
            PowerEvent::from_macos_message_type(super::MACOS_SYSTEM_HAS_POWERED_ON),
            Some(PowerEvent::Resumed)
        );
        assert_eq!(
            PowerEvent::from_macos_message_type(super::MACOS_CAN_SYSTEM_SLEEP),
            None
        );
        assert_eq!(PowerEvent::from_macos_message_type(0xe000_0320), None);
        assert!(super::macos_power_message_requires_ack(
            super::MACOS_CAN_SYSTEM_SLEEP
        ));
        assert!(super::macos_power_message_requires_ack(
            super::MACOS_SYSTEM_WILL_SLEEP
        ));
        assert!(!super::macos_power_message_requires_ack(
            super::MACOS_SYSTEM_HAS_POWERED_ON
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_mailbox_preserves_suspend_before_a_coalesced_resume() {
        let (publisher, events, _cancellation) = PowerEventInbox::channel();
        for _ in 0..64 {
            publisher.publish(PowerEvent::Suspending);
            publisher.publish(PowerEvent::Resumed);
        }
        assert_eq!(
            events.recv().expect("suspend is retained"),
            PowerEvent::Suspending
        );
        assert_eq!(
            events.recv().expect("resume is retained"),
            PowerEvent::Resumed
        );
        assert_eq!(events.try_recv(), None);

        publisher.publish(PowerEvent::Resumed);
        publisher.publish(PowerEvent::Suspending);
        assert_eq!(
            events.recv().expect("final suspend wins"),
            PowerEvent::Suspending
        );
        assert_eq!(events.try_recv(), None);
    }

    #[test]
    fn availability_matches_the_implemented_native_backend() {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        assert_eq!(
            NativePowerMonitor::availability(),
            CapabilityAvailability::Available
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        assert_eq!(
            NativePowerMonitor::availability(),
            CapabilityAvailability::Unavailable
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_power_monitor_registers_and_unregisters() {
        let monitor = NativePowerMonitor::connect().expect("Windows power callback registers");
        drop(monitor);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_power_monitor_cancellation_unblocks_a_waiter() {
        let mut monitor = NativePowerMonitor::connect().expect("Windows power callback registers");
        let cancellation = monitor.cancellation();
        let waiter = std::thread::spawn(move || monitor.next_event());
        cancellation.cancel();
        assert!(matches!(
            waiter.join().expect("join power monitor waiter"),
            Err(super::PowerNotificationError::Cancelled)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_power_monitor_registers_and_unregisters() {
        let monitor = NativePowerMonitor::connect().expect("macOS IOKit power callback registers");
        drop(monitor);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_power_monitor_cancellation_unblocks_a_waiter() {
        let mut monitor =
            NativePowerMonitor::connect().expect("macOS IOKit power callback registers");
        let cancellation = monitor.cancellation();
        let waiter = std::thread::spawn(move || monitor.next_event());
        cancellation.cancel();
        assert!(matches!(
            waiter.join().expect("join power monitor waiter"),
            Err(super::PowerNotificationError::Cancelled)
        ));
    }
}
