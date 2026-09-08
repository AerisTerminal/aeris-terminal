//! Capability-oriented operating-system boundary for native clients and ingest hosts.
//!
//! Consumers depend on capabilities rather than branching on operating-system names.

pub mod browser;
mod capability;
mod credential_vault;
mod display_timing;
mod io_cancellation;
mod lifecycle;
mod network_notifications;
mod power_notifications;
mod release_identity;
mod session_shutdown;

pub use browser::{MAXIMUM_AUTHORIZATION_URL_BYTES, open_system_browser};
pub use capability::CapabilityAvailability;
pub use credential_vault::{CredentialVault, NativeCredentialVault, NativeCredentialVaultError};
pub use display_timing::{
    DisplayEnvironment, DisplayOutput, DisplayTimingError, NativeDisplayProbe, PresentationClock,
};
#[cfg(target_os = "windows")]
pub use display_timing::{WindowsCompositionProbe, WindowsCompositionTiming};
pub use io_cancellation::cancel_tcp_stream_io;
pub use lifecycle::{
    ActiveRelease, InstallationInventory, LifecycleError, LifecycleHooks,
    RELEASE_CHANNEL_SCHEMA_VERSION, RELEASE_MANIFEST_SCHEMA_VERSION, ReleaseChannelPointer,
    ReleaseFile, ReleaseFileRole, ReleaseInstaller, ReleaseInstallerMetadata, ReleaseManifest,
    ReleasePolicy, RolloutMetadata, SignedReleaseManifest, UninstallOutcome, UpdateOutcome,
    VaultEntry, native_data_root, native_install_root, native_installation_inventory,
    sign_release_manifest, verify_release_file, verify_release_manifest,
};
pub use network_notifications::{
    NativeNetworkMonitor, NativeNetworkMonitorCancellation, NetworkEvent, NetworkNotificationError,
};
pub use power_notifications::{
    NativePowerMonitor, NativePowerMonitorCancellation, PowerEvent, PowerNotificationError,
};
pub use release_identity::{ReleaseIdentity, current_release_identity};
pub use session_shutdown::{
    NativeSessionShutdownCancellation, NativeSessionShutdownMonitor, SessionShutdownError,
};
