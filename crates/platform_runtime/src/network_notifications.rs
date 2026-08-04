//! Native network-availability notification boundary.

use crate::CapabilityAvailability;
use std::{error::Error, fmt};

#[cfg(target_os = "linux")]
use zbus::{
    MatchRule,
    blocking::{Connection, MessageIterator, Proxy, connection},
    message::Type as MessageType,
};

#[cfg(target_os = "linux")]
const NETWORK_MANAGER_DESTINATION: &str = "org.freedesktop.NetworkManager";
#[cfg(target_os = "linux")]
const NETWORK_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
#[cfg(target_os = "linux")]
const NETWORK_MANAGER_INTERFACE: &str = "org.freedesktop.NetworkManager";
#[cfg(target_os = "linux")]
const STATE_CHANGED_SIGNAL: &str = "StateChanged";
#[cfg(target_os = "linux")]
const STATE_PROPERTY: &str = "State";
#[cfg(target_os = "linux")]
const MAX_QUEUED_NETWORK_EVENTS: usize = 16;
#[cfg(target_os = "linux")]
const MAX_OWNER_RECONCILIATION_ATTEMPTS: usize = 3;
#[cfg(target_os = "linux")]
const DBUS_DESTINATION: &str = "org.freedesktop.DBus";
#[cfg(target_os = "linux")]
const DBUS_PATH: &str = "/org/freedesktop/DBus";
#[cfg(target_os = "linux")]
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";
#[cfg(target_os = "linux")]
const NAME_OWNER_CHANGED_SIGNAL: &str = "NameOwnerChanged";

#[cfg(any(target_os = "linux", test))]
const NETWORK_MANAGER_STATE_CONNECTED_GLOBAL: u32 = 70;

#[cfg(target_os = "linux")]
fn network_manager_state_rule() -> Result<MatchRule<'static>, zbus::Error> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(NETWORK_MANAGER_DESTINATION)?
        .path(NETWORK_MANAGER_PATH)?
        .interface(NETWORK_MANAGER_INTERFACE)?
        .member(STATE_CHANGED_SIGNAL)?
        .build();
    Ok(rule)
}

#[cfg(target_os = "linux")]
fn network_manager_owner_rule() -> Result<MatchRule<'static>, zbus::Error> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(DBUS_DESTINATION)?
        .path(DBUS_PATH)?
        .interface(DBUS_INTERFACE)?
        .member(NAME_OWNER_CHANGED_SIGNAL)?
        .add_arg(NETWORK_MANAGER_DESTINATION)?
        .build();
    Ok(rule)
}

/// Provider-relevant native network availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkEvent {
    Unavailable,
    Available,
}

struct NetworkTransitionFilter {
    current: NetworkEvent,
}

impl NetworkTransitionFilter {
    #[cfg(any(target_os = "linux", test))]
    const fn new(current: NetworkEvent) -> Self {
        Self { current }
    }

    #[cfg(any(target_os = "linux", test))]
    fn accept(&mut self, event: NetworkEvent) -> Option<NetworkEvent> {
        if event == self.current {
            return None;
        }
        self.current = event;
        Some(event)
    }
}

impl NetworkEvent {
    #[cfg(any(target_os = "linux", test))]
    const fn from_network_manager_state(state: u32) -> Self {
        if state == NETWORK_MANAGER_STATE_CONNECTED_GLOBAL {
            Self::Available
        } else {
            Self::Unavailable
        }
    }
}

/// Blocking native network-event listener.
///
/// On Linux this subscribes to `NetworkManager`'s `StateChanged` signal on the
/// system bus. Callers must run [`Self::next_event`] outside async executors and
/// UI threads because it blocks until availability changes.
pub struct NativeNetworkMonitor {
    transitions: NetworkTransitionFilter,
    #[cfg(target_os = "linux")]
    messages: MessageIterator,
    #[cfg(target_os = "linux")]
    query_connection: Connection,
    #[cfg(target_os = "linux")]
    state_rule: MatchRule<'static>,
    #[cfg(target_os = "linux")]
    owner_rule: MatchRule<'static>,
}

#[cfg(target_os = "linux")]
fn dbus_proxy(connection: &Connection) -> Result<Proxy<'_>, zbus::Error> {
    Proxy::new(connection, DBUS_DESTINATION, DBUS_PATH, DBUS_INTERFACE)
}

#[cfg(target_os = "linux")]
fn install_match(connection: &Connection, rule: &MatchRule<'_>) -> Result<(), zbus::Error> {
    dbus_proxy(connection)?.call("AddMatch", &(rule.to_string(),))
}

#[cfg(target_os = "linux")]
fn network_manager_has_owner(connection: &Connection) -> Result<bool, zbus::Error> {
    dbus_proxy(connection)?.call("NameHasOwner", &(NETWORK_MANAGER_DESTINATION,))
}

#[cfg(target_os = "linux")]
fn network_manager_owner(
    connection: &Connection,
) -> Result<Option<String>, NetworkNotificationError> {
    for _ in 0..MAX_OWNER_RECONCILIATION_ATTEMPTS {
        if !network_manager_has_owner(connection).map_err(NetworkNotificationError::Platform)? {
            return Ok(None);
        }
        match dbus_proxy(connection)
            .map_err(NetworkNotificationError::Platform)?
            .call("GetNameOwner", &(NETWORK_MANAGER_DESTINATION,))
        {
            Ok(owner) => return Ok(Some(owner)),
            Err(_) => {
                if !network_manager_has_owner(connection)
                    .map_err(NetworkNotificationError::Platform)?
                {
                    return Ok(None);
                }
            }
        }
    }
    Err(NetworkNotificationError::OwnerChangedRepeatedly)
}

#[cfg(target_os = "linux")]
fn read_network_event(connection: &Connection) -> Result<NetworkEvent, NetworkNotificationError> {
    for _ in 0..MAX_OWNER_RECONCILIATION_ATTEMPTS {
        let Some(owner_before) = network_manager_owner(connection)? else {
            return Ok(NetworkEvent::Unavailable);
        };
        let proxy = Proxy::new(
            connection,
            NETWORK_MANAGER_DESTINATION,
            NETWORK_MANAGER_PATH,
            NETWORK_MANAGER_INTERFACE,
        )
        .map_err(NetworkNotificationError::Platform)?;
        let state = proxy.get_property::<u32>(STATE_PROPERTY);
        let owner_after = network_manager_owner(connection)?;
        if owner_after.as_ref() != Some(&owner_before) {
            continue;
        }
        return state
            .map(NetworkEvent::from_network_manager_state)
            .map_err(NetworkNotificationError::Platform);
    }
    Err(NetworkNotificationError::OwnerChangedRepeatedly)
}

impl NativeNetworkMonitor {
    /// Reports whether this crate implements a native network-event source.
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        #[cfg(target_os = "linux")]
        return CapabilityAvailability::Available;

        #[cfg(not(target_os = "linux"))]
        CapabilityAvailability::Unavailable
    }

    /// Connects to the native source and reads its current availability.
    ///
    /// Both signal matches are installed before the initial property read so a
    /// transition cannot be silently lost during construction.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is unsupported or a Linux system-bus,
    /// subscription, stable-owner, or property operation fails. An absent
    /// `NetworkManager` owner is represented as [`NetworkEvent::Unavailable`].
    pub fn connect() -> Result<Self, NetworkNotificationError> {
        #[cfg(target_os = "linux")]
        {
            let connection = connection::Builder::system()
                .map_err(NetworkNotificationError::Platform)?
                .max_queued(MAX_QUEUED_NETWORK_EVENTS)
                .build()
                .map_err(NetworkNotificationError::Platform)?;
            let messages = MessageIterator::from(&connection);
            let state_rule =
                network_manager_state_rule().map_err(NetworkNotificationError::Platform)?;
            let owner_rule =
                network_manager_owner_rule().map_err(NetworkNotificationError::Platform)?;
            install_match(&connection, &state_rule).map_err(NetworkNotificationError::Platform)?;
            install_match(&connection, &owner_rule).map_err(NetworkNotificationError::Platform)?;
            let query_connection =
                Connection::system().map_err(NetworkNotificationError::Platform)?;
            let current = read_network_event(&query_connection)?;
            Ok(Self {
                transitions: NetworkTransitionFilter::new(current),
                messages,
                query_connection,
                state_rule,
                owner_rule,
            })
        }

        #[cfg(not(target_os = "linux"))]
        Err(NetworkNotificationError::UnsupportedPlatform)
    }

    /// Returns the availability observed during construction or the last event.
    #[must_use]
    pub const fn current(&self) -> NetworkEvent {
        self.transitions.current
    }

    /// Blocks until provider-relevant availability changes.
    ///
    /// Each matching signal triggers a fresh owner/property read, so queued stale
    /// signals, daemon restarts, intermediate states, and duplicates are reconciled
    /// to the current `Available`/`Unavailable` condition.
    ///
    /// # Errors
    ///
    /// Returns an error when the native stream closes or a bus/property operation
    /// fails while `NetworkManager` still owns its service name.
    pub fn next_event(&mut self) -> Result<NetworkEvent, NetworkNotificationError> {
        #[cfg(target_os = "linux")]
        loop {
            let message = self
                .messages
                .next()
                .ok_or(NetworkNotificationError::StreamClosed)?
                .map_err(NetworkNotificationError::Platform)?;
            if !self
                .state_rule
                .matches(&message)
                .map_err(NetworkNotificationError::Platform)?
                && !self
                    .owner_rule
                    .matches(&message)
                    .map_err(NetworkNotificationError::Platform)?
            {
                continue;
            }
            let current = read_network_event(&self.query_connection)?;
            if let Some(event) = self.transitions.accept(current) {
                return Ok(event);
            }
        }

        #[cfg(not(target_os = "linux"))]
        Err(NetworkNotificationError::UnsupportedPlatform)
    }
}

impl fmt::Debug for NativeNetworkMonitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeNetworkMonitor")
            .field("availability", &Self::availability())
            .field("current", &self.transitions.current)
            .finish_non_exhaustive()
    }
}

/// Errors returned by native network notification monitoring.
#[derive(Debug)]
pub enum NetworkNotificationError {
    #[cfg(target_os = "linux")]
    Platform(zbus::Error),
    OwnerChangedRepeatedly,
    StreamClosed,
    UnsupportedPlatform,
}

impl fmt::Display for NetworkNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            Self::Platform(error) => {
                write!(formatter, "native network notification failed: {error}")
            }
            Self::OwnerChangedRepeatedly => formatter
                .write_str("native network notification owner changed repeatedly during sampling"),
            Self::StreamClosed => formatter.write_str("native network notification stream closed"),
            Self::UnsupportedPlatform => {
                formatter.write_str("native network notifications are unavailable on this platform")
            }
        }
    }
}

impl Error for NetworkNotificationError {
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
    use super::{NativeNetworkMonitor, NetworkEvent, NetworkTransitionFilter};
    use crate::CapabilityAvailability;

    #[test]
    fn only_global_network_manager_state_is_provider_available() {
        for state in [0, 10, 20, 30, 40, 50, 60, 80, u32::MAX] {
            assert_eq!(
                NetworkEvent::from_network_manager_state(state),
                NetworkEvent::Unavailable
            );
        }
        assert_eq!(
            NetworkEvent::from_network_manager_state(70),
            NetworkEvent::Available
        );
    }

    #[test]
    fn repeated_and_intermediate_states_emit_only_availability_changes() {
        let mut transitions = NetworkTransitionFilter::new(NetworkEvent::Unavailable);
        for state in [0, 40, 50, 60] {
            assert_eq!(
                transitions.accept(NetworkEvent::from_network_manager_state(state)),
                None
            );
        }
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(70)),
            Some(NetworkEvent::Available)
        );
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(70)),
            None
        );
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(60)),
            Some(NetworkEvent::Unavailable)
        );
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(20)),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_match_rule_is_specific_to_network_manager_state_changes() {
        let state_rule = super::network_manager_state_rule().expect("static match rule is valid");
        assert_eq!(
            state_rule.to_string(),
            "type='signal',sender='org.freedesktop.NetworkManager',interface='org.freedesktop.NetworkManager',member='StateChanged',path='/org/freedesktop/NetworkManager'"
        );
        let owner_rule = super::network_manager_owner_rule().expect("static match rule is valid");
        assert_eq!(
            owner_rule.to_string(),
            "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',path='/org/freedesktop/DBus',arg0='org.freedesktop.NetworkManager'"
        );
    }

    #[test]
    fn availability_matches_the_implemented_native_backend() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            NativeNetworkMonitor::availability(),
            CapabilityAvailability::Available
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            NativeNetworkMonitor::availability(),
            CapabilityAvailability::Unavailable
        );
    }
}
