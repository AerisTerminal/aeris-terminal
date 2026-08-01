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
