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
mod power_notifications;
mod signed_update;
#[cfg(test)]
mod test_fixture;
mod thread_scheduling;
mod uri_callback;

pub use capability::{
    CapabilityAvailability, ClientOperatingMode, ClockCapability, RuntimeCapabilities,
};
pub use clock::{RuntimeClock, StandardRuntimeClock};
pub use composition::PlatformRuntime;
pub use credential_vault::{CredentialVault, NativeCredentialVault, NativeCredentialVaultError};
pub use loopback_callback::{AuthorizationCode, LoopbackCallbackError, LoopbackRedirectListener};
pub use paths::RuntimePaths;
pub use pkce::{CODE_CHALLENGE_METHOD, PkceError, PkceSecret};
pub use power_notifications::{NativePowerMonitor, PowerEvent, PowerNotificationError};
#[cfg(unix)]
pub use signed_update::{DurableUpdateActivator, UpdateActivationError};
pub use signed_update::{
    InstalledRelease, KeyRotationProof, SignedUpdateError, SignedUpdateVerifier, UpdateManifest,
    UpdateRollbackState,
};
pub use thread_scheduling::{
    AffinityTarget, NativeThreadScheduler, ThreadPriorityHint, ThreadSchedulingError,
};
pub use uri_callback::{
    MAXIMUM_APPLICATION_ID_BYTES, MAXIMUM_REDIRECT_BASE_BYTES, MAXIMUM_REDIRECT_RESPONSE_BYTES,
    MAXIMUM_REDIRECT_URI_BYTES, MAXIMUM_URI_SCHEME_BYTES, NativeUriSchemeRegistrar,
    RegisteredUriRedirect, UriCallbackError,
};
