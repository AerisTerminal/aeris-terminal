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
mod release_delivery;
mod release_identity;
mod rollout;
mod session_shutdown;
mod user_notifications;

pub use browser::{MAXIMUM_AUTHORIZATION_URL_BYTES, open_system_browser};
pub use capability::CapabilityAvailability;
pub use credential_vault::{CredentialVault, NativeCredentialVault, NativeCredentialVaultError};
pub use display_timing::{
    DisplayEnvironment, DisplayOutput, DisplayTimingError, NativeDisplayProbe, PresentationClock,
};
#[cfg(target_os = "windows")]
pub use display_timing::{WindowsCompositionProbe, WindowsCompositionTiming};
pub use io_cancellation::cancel_tcp_stream_io;
#[cfg(target_os = "windows")]
pub use lifecycle::verify_windows_publisher_signature;
pub use lifecycle::{
    ActiveRelease, CURRENT_STATE_COMPATIBILITY_EPOCH, InstallationInventory, LifecycleError,
    LifecycleHooks, RELEASE_CHANNEL_SCHEMA_VERSION, RELEASE_MANIFEST_SCHEMA_VERSION,
    ROLLBACK_COMPATIBILITY_FILENAME, ROLLBACK_COMPATIBILITY_SCHEMA_VERSION, ReleaseChannelPointer,
    ReleaseFile, ReleaseFileRole, ReleaseInstaller, ReleaseInstallerMetadata, ReleaseManifest,
    ReleasePolicy, RollbackCompatibilityMetadata, RolloutMetadata, SignedReleaseManifest,
    UninstallOutcome, UpdateOutcome, VaultEntry, native_data_root, native_install_root,
    native_installation_inventory, sign_release_manifest, verify_release_file,
    verify_release_manifest, verify_release_manifest_signature,
};
pub use network_notifications::{
    NativeNetworkMonitor, NativeNetworkMonitorCancellation, NetworkEvent, NetworkNotificationError,
};
pub use power_notifications::{
    NativePowerMonitor, NativePowerMonitorCancellation, PowerEvent, PowerNotificationError,
};
pub use release_delivery::{
    BLOCK_PLAN_BLOCK_BYTES, BLOCK_PLAN_FILENAME, BLOCK_PLAN_SCHEMA_VERSION, BlockDescriptor,
    BlockFilePlan, BlockPlan, MAXIMUM_BLOCK_PLAN_BLOCKS, MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS,
    MAXIMUM_BLOCK_PLAN_FILES, MAXIMUM_SIGNED_BLOCK_PLAN_BYTES, ReleaseDeliveryError,
    SignedBlockPlan, decode_and_verify_block_plan, sign_block_plan, validate_block_plan_binding,
    verify_block_plan,
};
pub use release_identity::{ReleaseIdentity, current_release_identity};
pub use rollout::rollout_eligible;
pub use session_shutdown::{
    NativeSessionShutdownCancellation, NativeSessionShutdownMonitor, SessionShutdownError,
};
pub use user_notifications::{
    NativeUserNotification, NativeUserNotificationError, try_send_user_notification,
};
