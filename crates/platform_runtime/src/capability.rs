//! Runtime capability vocabulary shared by operating-system adapters.

/// Whether a platform capability is usable in the current runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CapabilityAvailability {
    #[default]
    Unavailable,
    Available,
}
