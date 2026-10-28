//! Signed release lifecycle: install, update, rollback and uninstall mechanics,
//! release delivery plans, rollout policy, the compiled release identity and the
//! stable `aeris_launcher` binary.

mod lifecycle;
mod release_delivery;
mod release_identity;
mod rollout;

#[cfg(any(test, feature = "producer"))]
pub use lifecycle::sign_release_manifest;
#[cfg(target_os = "windows")]
pub use lifecycle::verify_windows_publisher_signature;
pub use lifecycle::{
    ActiveRelease, CURRENT_STATE_COMPATIBILITY_EPOCH, InstallationInventory, LifecycleError,
    LifecycleHooks, RELEASE_CHANNEL_SCHEMA_VERSION, RELEASE_MANIFEST_SCHEMA_VERSION,
    ROLLBACK_COMPATIBILITY_FILENAME, ROLLBACK_COMPATIBILITY_SCHEMA_VERSION, ReleaseChannelPointer,
    ReleaseFile, ReleaseFileRole, ReleaseInstaller, ReleaseInstallerMetadata, ReleaseManifest,
    ReleasePolicy, RollbackCompatibilityMetadata, RolloutMetadata, SignedReleaseManifest,
    UninstallOutcome, UpdateOutcome, VaultEntry, native_install_root,
    native_installation_inventory, verify_release_file, verify_release_manifest,
    verify_release_manifest_signature,
};
#[cfg(any(test, feature = "producer"))]
pub use release_delivery::sign_block_plan;
pub use release_delivery::{
    BLOCK_PLAN_BLOCK_BYTES, BLOCK_PLAN_FILENAME, BLOCK_PLAN_SCHEMA_VERSION, BlockDescriptor,
    BlockFilePlan, BlockPlan, MAXIMUM_BLOCK_PLAN_BLOCKS, MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS,
    MAXIMUM_BLOCK_PLAN_FILES, MAXIMUM_SIGNED_BLOCK_PLAN_BYTES, ReleaseDeliveryError,
    SignedBlockPlan, decode_and_verify_block_plan, validate_block_plan_binding, verify_block_plan,
};
pub use release_identity::{ReleaseIdentity, current_release_identity};
pub use rollout::rollout_eligible;
