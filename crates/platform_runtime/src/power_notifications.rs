//! Native suspend and resume notification boundary.

use crate::CapabilityAvailability;
use std::{error::Error, fmt};

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
}

/// Blocking native power-event listener.
///
/// On Linux this subscribes to systemd-logind's `PrepareForSleep` signal on the
/// system bus. Callers must run [`Self::next_event`] outside async executors and UI
/// threads because it blocks until a transition arrives.
pub struct NativePowerMonitor {
    #[cfg(target_os = "linux")]
    messages: MessageIterator,
}

impl NativePowerMonitor {
    /// Reports whether this crate implements a native power-event source for the target.
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        #[cfg(target_os = "linux")]
        return CapabilityAvailability::Available;

        #[cfg(not(target_os = "linux"))]
        CapabilityAvailability::Unavailable
    }

    /// Connects to the native power-event source.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is unsupported, the Linux system bus is
    /// unavailable, or the logind signal subscription cannot be installed.
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

        #[cfg(not(target_os = "linux"))]
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

        #[cfg(not(target_os = "linux"))]
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
    StreamClosed,
    UnsupportedPlatform,
}

impl fmt::Display for PowerNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            Self::Platform(error) => write!(formatter, "native power notification failed: {error}"),
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
    fn availability_matches_the_implemented_native_backend() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            NativePowerMonitor::availability(),
            CapabilityAvailability::Available
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            NativePowerMonitor::availability(),
            CapabilityAvailability::Unavailable
        );
    }
}
