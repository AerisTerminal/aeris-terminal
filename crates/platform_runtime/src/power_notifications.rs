//! Native suspend and resume notification boundary.

use crate::CapabilityAvailability;
use std::{error::Error, fmt};

#[cfg(target_os = "windows")]
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
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

#[cfg(target_os = "windows")]
struct PowerEventPublisher {
    pending: Arc<AtomicU8>,
    wake: SyncSender<()>,
}

#[cfg(target_os = "windows")]
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

#[cfg(target_os = "windows")]
struct PowerEventInbox {
    pending: Arc<AtomicU8>,
    wake: Receiver<()>,
}

#[cfg(target_os = "windows")]
impl PowerEventInbox {
    fn channel() -> (Arc<PowerEventPublisher>, Self) {
        let pending = Arc::new(AtomicU8::new(0));
        let (wake, receiver) = sync_channel(1);
        (
            Arc::new(PowerEventPublisher {
                pending: Arc::clone(&pending),
                wake,
            }),
            Self {
                pending,
                wake: receiver,
            },
        )
    }

    fn recv(&self) -> Result<PowerEvent, PowerNotificationError> {
        loop {
            if let Some(event) = self.take_next() {
                return Ok(event);
            }
            self.wake
                .recv()
                .map_err(|_| PowerNotificationError::StreamClosed)?;
        }
    }

    #[cfg(test)]
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

#[cfg(target_os = "windows")]
const SUSPEND_PENDING: u8 = 1;
#[cfg(target_os = "windows")]
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
        pub(super) fn connect() -> Result<(Self, PowerEventInbox), PowerNotificationError> {
            let (publisher, events) = PowerEventInbox::channel();
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
}

/// Blocking native power-event listener.
///
/// On Linux this subscribes to systemd-logind's `PrepareForSleep` signal on the
/// system bus. On Windows it registers a suspend/resume callback with the power
/// manager and coalesces callback bursts while preserving suspend before resume.
/// Callers must run [`Self::next_event`] outside async executors and UI threads
/// because it blocks until a transition arrives.
pub struct NativePowerMonitor {
    #[cfg(target_os = "linux")]
    messages: MessageIterator,
    #[cfg(target_os = "windows")]
    events: PowerEventInbox,
    #[cfg(target_os = "windows")]
    _registration: windows::Registration,
}

impl NativePowerMonitor {
    /// Reports whether this crate implements a native power-event source for the target.
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        return CapabilityAvailability::Available;

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        CapabilityAvailability::Unavailable
    }

    /// Connects to the native power-event source.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is unsupported or the native Linux or
    /// Windows notification registration cannot be installed.
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
            Ok(Self { messages })
        }

        #[cfg(target_os = "windows")]
        {
            let (registration, events) = windows::Registration::connect()?;
            Ok(Self {
                events,
                _registration: registration,
            })
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        Err(PowerNotificationError::UnsupportedPlatform)
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
            let message = self
                .messages
                .next()
                .ok_or(PowerNotificationError::StreamClosed)?
                .map_err(PowerNotificationError::Platform)?;
            let preparing = message
                .body()
                .deserialize::<bool>()
                .map_err(PowerNotificationError::Platform)?;
            Ok(PowerEvent::from_preparing_for_sleep(preparing))
        }

        #[cfg(target_os = "windows")]
        {
            self.events.recv()
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        Err(PowerNotificationError::UnsupportedPlatform)
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_mailbox_preserves_suspend_before_a_coalesced_resume() {
        let (publisher, events) = PowerEventInbox::channel();
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
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert_eq!(
            NativePowerMonitor::availability(),
            CapabilityAvailability::Available
        );
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
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
}
