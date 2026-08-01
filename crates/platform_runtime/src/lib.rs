//! Capability-oriented operating-system boundary for native clients and ingest hosts.
//!
//! Consumers depend on capabilities rather than branching on operating-system names.

mod capability;
mod clock;
mod composition;
mod credential_vault;
mod loopback_callback;
mod paths;
mod pkce;
mod signed_update;
#[cfg(test)]
mod test_fixture;
mod thread_scheduling;

pub use capability::{
    CapabilityAvailability, ClientOperatingMode, ClockCapability, RuntimeCapabilities,
};
pub use clock::{RuntimeClock, StandardRuntimeClock};
pub use composition::PlatformRuntime;
pub use credential_vault::{CredentialVault, NativeCredentialVault, NativeCredentialVaultError};
pub use loopback_callback::{AuthorizationCode, LoopbackCallbackError, LoopbackRedirectListener};
pub use paths::RuntimePaths;
pub use pkce::{CODE_CHALLENGE_METHOD, PkceError, PkceSecret};
pub use signed_update::{
    InstalledRelease, SignedUpdateError, SignedUpdateVerifier, UpdateManifest, UpdateRollbackState,
};
pub use thread_scheduling::{
    AffinityTarget, NativeThreadScheduler, ThreadPriorityHint, ThreadSchedulingError,
};
