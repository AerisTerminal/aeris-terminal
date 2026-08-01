//! Capability-oriented native runtime composition port.

use crate::capability::RuntimeCapabilities;
use crate::clock::RuntimeClock;
use crate::credential_vault::CredentialVault;
use crate::paths::RuntimePaths;

/// Capability-oriented native runtime composition port.
pub trait PlatformRuntime {
    type Clock: RuntimeClock;
    type Vault: CredentialVault;

    fn capabilities(&self) -> &RuntimeCapabilities;
    fn clock(&self) -> &Self::Clock;
    fn credential_vault(&self) -> &Self::Vault;
    fn paths(&self) -> &RuntimePaths;
}
