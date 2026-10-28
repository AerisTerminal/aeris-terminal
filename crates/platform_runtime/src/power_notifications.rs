//! Native suspend and resume notification boundary.

use crate::CapabilityAvailability;
pub use aeris_platform_sys::power::PowerEvent;
use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use aeris_platform_sys::power::{RegistrationError, SuspendResumeRegistration};
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
    fn channel() -> (PowerEventPublisher, Self, NativePowerMonitorCancellation) {
        let pending = Arc::new(AtomicU8::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (wake, receiver) = sync_channel(1);
        (
            PowerEventPublisher {
                pending: Arc::clone(&pending),
                wake: wake.clone(),
            },
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

#[cfg(any(target_os = "macos", target_os = "windows"))]
const SUSPEND_PENDING: u8 = 1;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const RESUME_PENDING: u8 = 2;

#[cfg(any(target_os = "linux", test))]
const fn power_event_from_preparing_for_sleep(preparing: bool) -> PowerEvent {
    if preparing {
        PowerEvent::Suspending
    } else {
        PowerEvent::Resumed
    }
}

/// Blocking native power-event listener.
///
/// On Linux this subscribes to systemd-logind's `PrepareForSleep` signal on the
/// system bus. On macOS and Windows it consumes the `platform_sys`
/// suspend/resume callback. Callback-based backends coalesce bursts while
/// preserving suspend before resume. Callers must run [`Self::next_event`]
/// outside async executors and UI threads because it blocks until a transition
/// arrives.
pub struct NativePowerMonitor {
    cancellation: NativePowerMonitorCancellation,
    #[cfg(target_os = "linux")]
    messages: MessageIterator,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    events: PowerEventInbox,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    _registration: SuspendResumeRegistration,
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

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let (publisher, events, cancellation) = PowerEventInbox::channel();
            let registration =
                SuspendResumeRegistration::register(move |event| publisher.publish(event))
                    .map_err(PowerNotificationError::Registration)?;
            Ok(Self {
                cancellation,
                events,
                _registration: registration,
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
            Ok(power_event_from_preparing_for_sleep(preparing))
        }

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.events.recv()
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Registration(RegistrationError),
    Cancelled,
    StreamClosed,
    UnsupportedPlatform,
}

impl fmt::Display for PowerNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            Self::Platform(error) => write!(formatter, "native power notification failed: {error}"),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::Registration(error) => error.fmt(formatter),
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::Registration(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    use super::PowerEventInbox;
    use super::{NativePowerMonitor, PowerEvent, power_event_from_preparing_for_sleep};
    use crate::CapabilityAvailability;

    #[test]
    fn prepare_for_sleep_boolean_maps_to_ordered_power_transitions() {
        assert_eq!(
            power_event_from_preparing_for_sleep(true),
            PowerEvent::Suspending
        );
        assert_eq!(
            power_event_from_preparing_for_sleep(false),
            PowerEvent::Resumed
        );
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn mailbox_preserves_suspend_before_a_coalesced_resume() {
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

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn power_monitor_registers_and_unregisters() {
        let monitor = NativePowerMonitor::connect().expect("native power callback registers");
        drop(monitor);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn power_monitor_cancellation_unblocks_a_waiter() {
        let mut monitor = NativePowerMonitor::connect().expect("native power callback registers");
        let cancellation = monitor.cancellation();
        let waiter = std::thread::spawn(move || monitor.next_event());
        cancellation.cancel();
        assert!(matches!(
            waiter.join().expect("join power monitor waiter"),
            Err(super::PowerNotificationError::Cancelled)
        ));
    }
}
