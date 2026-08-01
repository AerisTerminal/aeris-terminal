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

#[cfg(test)]
mod tests {
    use super::{
        CapabilityAvailability, ClientOperatingMode, ClockCapability, CredentialVault,
        PlatformRuntime, RuntimeCapabilities, RuntimeClock, RuntimePaths, StandardRuntimeClock,
    };
    use std::{
        cell::RefCell, collections::BTreeMap, convert::Infallible, path::PathBuf, thread,
        time::Duration,
    };

    #[derive(Default)]
    struct MemoryVault {
        secrets: RefCell<BTreeMap<String, Vec<u8>>>,
    }

    impl CredentialVault for MemoryVault {
        type Error = Infallible;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            self.secrets
                .borrow_mut()
                .insert(key.to_string(), secret.to_vec());
            Ok(())
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            Ok(self.secrets.borrow().get(key).cloned())
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            self.secrets.borrow_mut().remove(key);
            Ok(())
        }
    }

    struct TestRuntime {
        capabilities: RuntimeCapabilities,
        clock: StandardRuntimeClock,
        vault: MemoryVault,
        paths: RuntimePaths,
    }

    impl PlatformRuntime for TestRuntime {
        type Clock = StandardRuntimeClock;
        type Vault = MemoryVault;

        fn capabilities(&self) -> &RuntimeCapabilities {
            &self.capabilities
        }

        fn clock(&self) -> &Self::Clock {
            &self.clock
        }

        fn credential_vault(&self) -> &Self::Vault {
            &self.vault
        }

        fn paths(&self) -> &RuntimePaths {
            &self.paths
        }
    }

    fn capabilities() -> RuntimeCapabilities {
        RuntimeCapabilities {
            credential_vault: CapabilityAvailability::Available,
            pkce_loopback_callback: CapabilityAvailability::Available,
            registered_uri_callback: CapabilityAvailability::Unavailable,
            signed_updates: CapabilityAvailability::Available,
            rollback: CapabilityAvailability::Available,
            power_notifications: CapabilityAvailability::Unavailable,
            display_timing: CapabilityAvailability::Unavailable,
            priority_hints: CapabilityAvailability::Unavailable,
            affinity_hints: CapabilityAvailability::Unavailable,
            network_timestamping: ClockCapability::Software,
        }
    }

    #[test]
    fn defaults_never_claim_optional_platform_capabilities() {
        assert_eq!(
            ClientOperatingMode::default(),
            ClientOperatingMode::Balanced
        );
        assert_eq!(ClockCapability::default(), ClockCapability::Software);
        assert_eq!(
            CapabilityAvailability::default(),
            CapabilityAvailability::Unavailable
        );
    }

    #[test]
    fn standard_clock_is_monotonic_and_reports_post_epoch_wall_time() {
        let clock = StandardRuntimeClock::new();
        let first = clock.monotonic_nanos();
        thread::sleep(Duration::from_millis(1));
        let second = clock.monotonic_nanos();
        assert!(second >= first);
        assert!(clock.wall_unix_nanos().expect("wall time is post epoch") > 0);
    }

    #[test]
    fn platform_runtime_composes_capabilities_clock_vault_and_paths() {
        let runtime = TestRuntime {
            capabilities: capabilities(),
            clock: StandardRuntimeClock::new(),
            vault: MemoryVault::default(),
            paths: RuntimePaths {
                cache: PathBuf::from("cache"),
                configuration: PathBuf::from("configuration"),
                logs: PathBuf::from("logs"),
            },
        };

        assert_eq!(
            runtime.capabilities().credential_vault,
            CapabilityAvailability::Available
        );
        runtime
            .credential_vault()
            .store("session", b"opaque-secret")
            .expect("memory vault stores the secret");
        assert_eq!(
            runtime
                .credential_vault()
                .load("session")
                .expect("memory vault loads the secret"),
            Some(b"opaque-secret".to_vec())
        );
        runtime
            .credential_vault()
            .delete("session")
            .expect("memory vault deletes the secret");
        assert_eq!(
            runtime
                .credential_vault()
                .load("session")
                .expect("memory vault confirms deletion"),
            None
        );
        assert_eq!(runtime.paths().logs, PathBuf::from("logs"));
        assert!(runtime.clock().wall_unix_nanos().is_ok());
    }
}
