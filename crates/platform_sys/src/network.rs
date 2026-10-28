//! Operating-system network-availability callbacks.

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::{error::Error, fmt, sync::Arc};

#[cfg(target_os = "macos")]
use crate::macos::network as native;
#[cfg(target_os = "windows")]
use crate::windows::network as native;

/// Provider-relevant network availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkEvent {
    Unavailable,
    Available,
}

/// One notification from a registered network-availability source.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceChange {
    /// The source's current availability. The first notification reports the
    /// state at registration.
    Availability(NetworkEvent),
    /// The source stopped; no further notifications follow.
    Stopped,
}

/// The caller's change callback, shared with the native registration.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) struct ChangeCallback(Box<dyn Fn(InterfaceChange) + Send + Sync>);

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl ChangeCallback {
    pub(crate) fn call(&self, change: InterfaceChange) {
        (self.0)(change);
    }
}

/// A live operating-system network-availability subscription. Dropping it
/// cancels the callback.
///
/// Windows subscribes to connectivity-hint changes and reports the initial
/// state asynchronously, shortly after registration. macOS schedules
/// `SCNetworkReachability` for the default route on a dedicated
/// CoreFoundation run-loop thread and reports the initial state before
/// [`Self::register`] returns.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct InterfaceChangeRegistration {
    _native: native::Registration,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl InterfaceChangeRegistration {
    /// Registers `callback` for network-availability changes.
    ///
    /// The callback runs on an operating-system or registration-owned thread.
    /// It must return promptly and must not panic.
    ///
    /// # Errors
    /// Returns the native operation that failed when the subscription cannot
    /// be installed.
    pub fn register(
        callback: impl Fn(InterfaceChange) + Send + Sync + 'static,
    ) -> Result<Self, RegistrationError> {
        native::Registration::register(Arc::new(ChangeCallback(Box::new(callback))))
            .map(|native| Self { _native: native })
    }
}

/// A native network-notification operation that failed.
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
            "native network notification operation {} failed",
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

/// `NetworkConnectivityLevelHintInternetAccess`.
#[cfg(any(target_os = "windows", test))]
const WINDOWS_INTERNET_ACCESS: i32 = 3;

/// Only Windows' internet-access connectivity level makes providers reachable.
#[cfg(any(target_os = "windows", test))]
pub(crate) const fn availability_from_windows_connectivity_level(level: i32) -> NetworkEvent {
    if level == WINDOWS_INTERNET_ACCESS {
        NetworkEvent::Available
    } else {
        NetworkEvent::Unavailable
    }
}

#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_REACHABLE: u32 = 1 << 1;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_CONNECTION_REQUIRED: u32 = 1 << 2;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC: u32 = 1 << 3;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_INTERVENTION_REQUIRED: u32 = 1 << 4;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_CONNECTION_ON_DEMAND: u32 = 1 << 5;

/// A reachable default route is available unless it needs a connection that
/// cannot start without user intervention.
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn availability_from_macos_reachability_flags(flags: u32) -> NetworkEvent {
    let reachable = flags & MACOS_REACHABILITY_REACHABLE != 0;
    let connection_required = flags & MACOS_REACHABILITY_CONNECTION_REQUIRED != 0;
    let can_connect_automatically = flags
        & (MACOS_REACHABILITY_CONNECTION_ON_DEMAND | MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC)
        != 0;
    let intervention_required = flags & MACOS_REACHABILITY_INTERVENTION_REQUIRED != 0;
    if reachable && (!connection_required || (can_connect_automatically && !intervention_required))
    {
        NetworkEvent::Available
    } else {
        NetworkEvent::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MACOS_REACHABILITY_CONNECTION_ON_DEMAND, MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC,
        MACOS_REACHABILITY_CONNECTION_REQUIRED, MACOS_REACHABILITY_INTERVENTION_REQUIRED,
        MACOS_REACHABILITY_REACHABLE, NetworkEvent, availability_from_macos_reachability_flags,
        availability_from_windows_connectivity_level,
    };

    #[test]
    fn only_windows_internet_access_is_provider_available() {
        for level in [0, 1, 2, 4, 5, i32::MAX] {
            assert_eq!(
                availability_from_windows_connectivity_level(level),
                NetworkEvent::Unavailable
            );
        }
        assert_eq!(
            availability_from_windows_connectivity_level(3),
            NetworkEvent::Available
        );
    }

    #[test]
    fn macos_reachability_requires_a_usable_route() {
        assert_eq!(
            availability_from_macos_reachability_flags(0),
            NetworkEvent::Unavailable
        );
        assert_eq!(
            availability_from_macos_reachability_flags(MACOS_REACHABILITY_REACHABLE),
            NetworkEvent::Available
        );
        assert_eq!(
            availability_from_macos_reachability_flags(
                MACOS_REACHABILITY_REACHABLE | MACOS_REACHABILITY_CONNECTION_REQUIRED
            ),
            NetworkEvent::Unavailable
        );
        for automatic in [
            MACOS_REACHABILITY_CONNECTION_ON_DEMAND,
            MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC,
        ] {
            assert_eq!(
                availability_from_macos_reachability_flags(
                    MACOS_REACHABILITY_REACHABLE
                        | MACOS_REACHABILITY_CONNECTION_REQUIRED
                        | automatic
                ),
                NetworkEvent::Available
            );
            assert_eq!(
                availability_from_macos_reachability_flags(
                    MACOS_REACHABILITY_REACHABLE
                        | MACOS_REACHABILITY_CONNECTION_REQUIRED
                        | automatic
                        | MACOS_REACHABILITY_INTERVENTION_REQUIRED
                ),
                NetworkEvent::Unavailable
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn network_callbacks_report_the_initial_state() {
        use super::{InterfaceChange, InterfaceChangeRegistration};
        use std::{sync::mpsc::sync_channel, time::Duration};

        let (sender, receiver) = sync_channel(16);
        let registration = InterfaceChangeRegistration::register(move |change| {
            let _ = sender.try_send(change);
        })
        .expect("network callback registers");
        let first = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the initial state arrives");
        assert!(
            matches!(first, InterfaceChange::Availability(_)),
            "{first:?}"
        );
        drop(registration);
    }
}
