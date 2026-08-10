//! Capability-oriented operating-system boundary for native clients and ingest hosts.
//!
//! Consumers depend on capabilities rather than branching on operating-system names.

mod capability;
mod credential_vault;
mod display_timing;
mod io_cancellation;
mod network_notifications;
mod power_notifications;

pub use capability::CapabilityAvailability;
pub use credential_vault::{CredentialVault, NativeCredentialVault, NativeCredentialVaultError};
pub use display_timing::{
    DisplayEnvironment, DisplayOutput, DisplayTimingError, NativeDisplayProbe, PresentationClock,
};
#[cfg(target_os = "windows")]
pub use display_timing::{WindowsCompositionProbe, WindowsCompositionTiming};
pub use io_cancellation::cancel_tcp_stream_io;
pub use network_notifications::{NativeNetworkMonitor, NetworkEvent, NetworkNotificationError};
pub use power_notifications::{NativePowerMonitor, PowerEvent, PowerNotificationError};
