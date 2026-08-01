//! Shared in-memory platform fixtures and capability composition assertions.

use crate::{
    CapabilityAvailability, ClientOperatingMode, ClockCapability, CredentialVault, PlatformRuntime,
    RuntimeCapabilities, RuntimeClock, RuntimePaths, StandardRuntimeClock,
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
