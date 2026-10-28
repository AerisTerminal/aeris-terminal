//! Operating-system suspend and resume callbacks.

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::{error::Error, fmt, sync::Arc};

#[cfg(target_os = "macos")]
use crate::macos::power as native;
#[cfg(target_os = "windows")]
use crate::windows::power as native;

/// A system power transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerEvent {
    Suspending,
    Resumed,
}

/// The caller's transition callback, shared with the native registration.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) struct PowerCallback(Box<dyn Fn(PowerEvent) + Send + Sync>);

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl PowerCallback {
    pub(crate) fn call(&self, event: PowerEvent) {
        (self.0)(event);
    }
}

/// A live operating-system suspend/resume subscription. Dropping it
/// unregisters the callback.
///
/// Windows registers a power-manager callback. macOS attaches `IOKit`'s root
/// power-domain notifications to a dedicated CoreFoundation run-loop thread
/// and acknowledges each sleep request after the callback returns.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct SuspendResumeRegistration {
    _native: native::Registration,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SuspendResumeRegistration {
    /// Registers `callback` for system suspend and resume transitions.
    ///
    /// The callback runs on an operating-system or registration-owned thread.
    /// It must return promptly and must not panic.
    ///
    /// # Errors
    /// Returns the native operation that failed when the registration cannot
    /// be installed.
    pub fn register(
        callback: impl Fn(PowerEvent) + Send + Sync + 'static,
    ) -> Result<Self, RegistrationError> {
        native::Registration::register(Arc::new(PowerCallback(Box::new(callback))))
            .map(|native| Self { _native: native })
    }
}

/// A native power-notification operation that failed.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistrationError {
    operation: &'static str,
    code: Option<u32>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl RegistrationError {
    pub(crate) const fn new(operation: &'static str, code: Option<u32>) -> Self {
        Self { operation, code }
    }

    /// The native call or registration step that failed.
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    /// The operating-system error code, when the operation returns one.
    #[must_use]
    pub const fn code(&self) -> Option<u32> {
        self.code
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl fmt::Display for RegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "native power notification operation {} failed",
            self.operation
        )?;
        if let Some(code) = self.code {
            write!(formatter, " with error {code}")?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Error for RegistrationError {}

#[cfg(any(target_os = "macos", test))]
const MACOS_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
#[cfg(any(target_os = "macos", test))]
const MACOS_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
#[cfg(any(target_os = "macos", test))]
const MACOS_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

/// Maps an `IOKit` root power-domain message to the transition it commits.
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn power_event_from_macos_message_type(message_type: u32) -> Option<PowerEvent> {
    match message_type {
        MACOS_SYSTEM_WILL_SLEEP => Some(PowerEvent::Suspending),
        MACOS_SYSTEM_HAS_POWERED_ON => Some(PowerEvent::Resumed),
        _ => None,
    }
}

/// Whether `IOKit` waits for `IOAllowPowerChange` before continuing.
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn macos_power_message_requires_ack(message_type: u32) -> bool {
    matches!(
        message_type,
        MACOS_CAN_SYSTEM_SLEEP | MACOS_SYSTEM_WILL_SLEEP
    )
}

#[cfg(test)]
mod tests {
    use super::{
        MACOS_CAN_SYSTEM_SLEEP, MACOS_SYSTEM_HAS_POWERED_ON, MACOS_SYSTEM_WILL_SLEEP, PowerEvent,
        macos_power_message_requires_ack, power_event_from_macos_message_type,
    };

    #[test]
    fn macos_power_messages_map_only_committed_sleep_and_completed_wake() {
        assert_eq!(
            power_event_from_macos_message_type(MACOS_SYSTEM_WILL_SLEEP),
            Some(PowerEvent::Suspending)
        );
        assert_eq!(
            power_event_from_macos_message_type(MACOS_SYSTEM_HAS_POWERED_ON),
            Some(PowerEvent::Resumed)
        );
        assert_eq!(
            power_event_from_macos_message_type(MACOS_CAN_SYSTEM_SLEEP),
            None
        );
        assert_eq!(power_event_from_macos_message_type(0xe000_0320), None);
        assert!(macos_power_message_requires_ack(MACOS_CAN_SYSTEM_SLEEP));
        assert!(macos_power_message_requires_ack(MACOS_SYSTEM_WILL_SLEEP));
        assert!(!macos_power_message_requires_ack(
            MACOS_SYSTEM_HAS_POWERED_ON
        ));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn suspend_resume_callbacks_register_and_unregister() {
        let registration =
            super::SuspendResumeRegistration::register(|_| {}).expect("power callback registers");
        drop(registration);
    }
}
