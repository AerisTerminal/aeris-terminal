//! Capability-oriented operating-system boundary for native clients and ingest hosts.
//!
//! Consumers depend on capabilities rather than branching on operating-system names.

use std::path::PathBuf;
use std::time::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

/// User-visible runtime policy. Direct networking remains independently capability-gated.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ClientOperatingMode {
    #[default]
    Balanced,
    LowLatency,
    LinuxDirect,
}

/// Quality of a timestamp exposed by a platform or network adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ClockCapability {
    #[default]
    Software,
    KernelTimestamped,
    HardwareTimestamped,
}

/// Whether a platform capability is usable in the current runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CapabilityAvailability {
    #[default]
    Unavailable,
    Available,
}

/// Platform facilities discovered at runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCapabilities {
    pub credential_vault: CapabilityAvailability,
    pub pkce_loopback_callback: CapabilityAvailability,
    pub registered_uri_callback: CapabilityAvailability,
    pub signed_updates: CapabilityAvailability,
    pub rollback: CapabilityAvailability,
    pub power_notifications: CapabilityAvailability,
    pub display_timing: CapabilityAvailability,
    pub priority_hints: CapabilityAvailability,
    pub affinity_hints: CapabilityAvailability,
    pub network_timestamping: ClockCapability,
}

/// Secure credential storage boundary. Implementations must never use plaintext files.
pub trait CredentialVault {
    type Error;

    /// Stores an opaque secret under a non-secret key.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage is unavailable.
    fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error>;

    /// Loads an opaque secret, if present.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage cannot be queried.
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Deletes an opaque secret.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage cannot be updated.
    fn delete(&self, key: &str) -> Result<(), Self::Error>;
}

/// Monotonic and wall-clock source with explicit units.
pub trait RuntimeClock {
    type Error;

    fn monotonic_nanos(&self) -> u64;

    /// Returns Unix wall time in nanoseconds.
    ///
    /// # Errors
    ///
    /// Returns an error when the system clock is earlier than the Unix epoch.
    fn wall_unix_nanos(&self) -> Result<i128, Self::Error>;
}

/// Process-relative standard-library clock implementation.
#[derive(Clone, Debug)]
pub struct StandardRuntimeClock {
    monotonic_origin: Instant,
}

impl StandardRuntimeClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            monotonic_origin: Instant::now(),
        }
    }
}

impl Default for StandardRuntimeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeClock for StandardRuntimeClock {
    type Error = SystemTimeError;

    fn monotonic_nanos(&self) -> u64 {
        u64::try_from(self.monotonic_origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn wall_unix_nanos(&self) -> Result<i128, Self::Error> {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
        Ok(i128::try_from(elapsed.as_nanos()).unwrap_or(i128::MAX))
    }
}

/// Application-owned paths selected by a platform adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    pub cache: PathBuf,
    pub configuration: PathBuf,
    pub logs: PathBuf,
}

/// Capability-oriented native runtime composition port.
pub trait PlatformRuntime {
    type Clock: RuntimeClock;
    type Vault: CredentialVault;

    fn capabilities(&self) -> &RuntimeCapabilities;
    fn clock(&self) -> &Self::Clock;
    fn credential_vault(&self) -> &Self::Vault;
    fn paths(&self) -> &RuntimePaths;
}
