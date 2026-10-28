//! Signed release lifecycle: install, update, rollback and uninstall mechanics,
//! release delivery plans, rollout policy, the compiled release identity, the candidate
//! readiness report, the release origin's layout and the `ReleaseSource` it is read through,
//! the trading store schema a rollback must respect, the startup-complete marker that ends
//! a new release's probation, and the stable launcher that the
//! `aeris_launcher` binary runs.

mod launcher;
mod lifecycle;
mod origin_layout;
mod protocol;
mod readiness;
mod release_config;
mod release_delivery;
mod release_identity;
mod release_source;
mod rollout;
mod startup_marker;
mod store_schema;

pub use launcher::{
    ReleaseOrigin, run_launcher, run_native_launcher, valid_release_identity,
    validate_release_channel,
};
#[cfg(any(test, feature = "producer"))]
pub use lifecycle::sign_release_manifest;
#[cfg(target_os = "windows")]
pub use lifecycle::verify_windows_publisher_signature;
pub use lifecycle::{
    ActiveRelease, CURRENT_STATE_COMPATIBILITY_EPOCH, InstallationInventory, LifecycleError,
    LifecycleHooks, RELEASE_CHANNEL_SCHEMA_VERSION, RELEASE_MANIFEST_SCHEMA_VERSION,
    ROLLBACK_COMPATIBILITY_FILENAME, ROLLBACK_COMPATIBILITY_SCHEMA_VERSION, ReleaseChannelPointer,
    ReleaseFile, ReleaseFileRole, ReleaseInstaller, ReleaseInstallerMetadata, ReleaseManifest,
    ReleasePolicy, ReleaseUninstaller, RollbackCompatibilityMetadata, RolloutMetadata,
    SignedReleaseManifest, UninstallOutcome, UpdateOutcome, VaultEntry, native_install_root,
    native_installation_inventory, verify_release_file, verify_release_manifest,
};
pub use origin_layout::{
    RELEASE_CHANNEL, RELEASE_MANIFEST_FILENAME, channel_pointer_path, installer_filename,
    release_directory_path,
};
pub use protocol::{
    LAUNCHER_PROTOCOL_VERSION, UPDATE_RESTART_COMMIT, UPDATE_RESTART_READY,
    UPDATE_RESTART_VERIFICATION_TIMEOUT,
};
pub use readiness::{
    PRODUCTION_VAULT_NAMESPACE, READINESS_REPORT_SCHEMA_VERSION, ReadinessRejection,
    ReadinessReport, ServiceReadiness, ServiceStarted, VALIDATION_VAULT_NAMESPACE,
};
pub use release_config::{ReleaseConfig, release_config};
#[cfg(any(test, feature = "producer"))]
pub use release_delivery::sign_block_plan;
pub use release_delivery::{
    BLOCK_PLAN_BLOCK_BYTES, BLOCK_PLAN_FILENAME, BLOCK_PLAN_SCHEMA_VERSION, BlockDescriptor,
    BlockFilePlan, BlockPlan, MAXIMUM_BLOCK_PLAN_BLOCKS, MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS,
    MAXIMUM_BLOCK_PLAN_FILES, MAXIMUM_SIGNED_BLOCK_PLAN_BYTES, ReleaseDeliveryError,
    SignedBlockPlan, decode_and_verify_block_plan, validate_block_plan_binding, verify_block_plan,
};
pub use release_identity::{ReleaseIdentity, current_release_identity};
pub use release_source::{ArtifactBody, ReleaseSource, ReleaseSourceError};
pub use rollout::rollout_eligible;
pub use startup_marker::{
    STARTUP_COMPLETE_FILENAME, STARTUP_COMPLETE_SCHEMA_VERSION, StartupComplete,
    read_startup_complete, record_startup_complete, startup_complete_path,
};
pub use store_schema::{LiveStoreSchema, read_live_store_schema, trading_database_path};
