//! Runtime capability vocabulary shared by every operating-system adapter.

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
///
/// Prefer [`RuntimeCapabilities::detect_native`] over hand-written values: a literal
/// can claim a capability this crate does not implement for the running target.
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

impl RuntimeCapabilities {
    /// Reports only the capabilities this crate actually implements for the
    /// running target. Ports without a per-OS adapter stay `Unavailable`.
    #[must_use]
    pub fn detect_native() -> Self {
        Self {
            // Store initialization stays lazy so transient startup failures remain retryable.
            credential_vault: native_credential_vault_availability(),
            // `LoopbackRedirectListener` binds an ephemeral loopback port on any std target.
            pkce_loopback_callback: CapabilityAvailability::Available,
            registered_uri_callback: crate::NativeUriSchemeRegistrar::availability(),
            // `SignedUpdateVerifier` verifies manifests and artifacts without OS support.
            signed_updates: CapabilityAvailability::Available,
            // `UpdateRollbackState` tracks the verified predecessor in memory only;
            // durable activation and persistence still need per-OS adapters.
            rollback: CapabilityAvailability::Unavailable,
            power_notifications: crate::NativePowerMonitor::availability(),
            display_timing: CapabilityAvailability::Unavailable,
            priority_hints: crate::NativeThreadScheduler::priority_availability(),
            affinity_hints: crate::NativeThreadScheduler::affinity_availability(),
            network_timestamping: ClockCapability::Software,
        }
    }
}

const fn native_credential_vault_availability() -> CapabilityAvailability {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    return CapabilityAvailability::Available;

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    CapabilityAvailability::Unavailable
}

#[cfg(test)]
mod tests {
    use super::{CapabilityAvailability, ClockCapability, RuntimeCapabilities};

    #[test]
    fn detected_capabilities_never_claim_an_unimplemented_port() {
        let detected = RuntimeCapabilities::detect_native();

        assert_eq!(
            detected.registered_uri_callback,
            crate::NativeUriSchemeRegistrar::availability()
        );
        assert_eq!(detected.rollback, CapabilityAvailability::Unavailable);
        assert_eq!(
            detected.power_notifications,
            crate::NativePowerMonitor::availability()
        );
        assert_eq!(detected.display_timing, CapabilityAvailability::Unavailable);
        assert_eq!(detected.network_timestamping, ClockCapability::Software);
        assert_eq!(
            detected.priority_hints,
            crate::NativeThreadScheduler::priority_availability()
        );
        assert_eq!(
            detected.affinity_hints,
            crate::NativeThreadScheduler::affinity_availability()
        );
    }

    #[test]
    fn supported_targets_report_their_implemented_capabilities() {
        let detected = RuntimeCapabilities::detect_native();

        assert_eq!(
            detected.pkce_loopback_callback,
            CapabilityAvailability::Available
        );
        assert_eq!(detected.signed_updates, CapabilityAvailability::Available);
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        assert_eq!(detected.credential_vault, CapabilityAvailability::Available);
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        assert_eq!(
            detected.credential_vault,
            CapabilityAvailability::Unavailable
        );
    }
}
