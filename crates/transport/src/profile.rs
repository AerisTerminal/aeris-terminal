//! Ingest profiles, readiness levels, and active mode vocabulary.

use serde::{Deserialize, Serialize};

/// Foundational network profiles. Accelerated profiles never silently fall back.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestProfile {
    PortableSocket,
    TunedLinuxSocket,
    LinuxAfXdp,
    LinuxDpdk,
}

/// Ordered evidence state for one ingest profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    ContractOnly,
    Implemented,
    FixtureValidated,
    HardwareValidated,
    ProviderCertified,
    ProductionEnabled,
}

/// Runtime mode reported separately from requested profile and readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveIngestMode {
    Unavailable,
    SoftwareFixture,
    PortableSocket,
    TunedLinuxSocket,
    AfXdpCopy,
    AfXdpZeroCopy,
    DpdkPollMode,
}

pub(crate) fn mode_matches_profile(profile: IngestProfile, mode: ActiveIngestMode) -> bool {
    matches!(
        (profile, mode),
        (
            IngestProfile::PortableSocket,
            ActiveIngestMode::PortableSocket
        ) | (
            IngestProfile::TunedLinuxSocket,
            ActiveIngestMode::TunedLinuxSocket
        ) | (
            IngestProfile::LinuxAfXdp,
            ActiveIngestMode::AfXdpCopy | ActiveIngestMode::AfXdpZeroCopy
        ) | (IngestProfile::LinuxDpdk, ActiveIngestMode::DpdkPollMode)
    )
}
