//! Canonical provider fault classification shared by adapters and market runtimes.

/// Canonical provider fault classification produced by adapters so the market
/// runtime never matches provider-specific wire invalidation enums.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFaultKind {
    Transport,
    Authentication,
    AgreementRequired,
    UnsupportedSystem,
    SchemaMismatch,
    HeartbeatSilence,
    MessageSilence,
    SequenceGap,
    QueueOverflow,
    MalformedMessage,
    Transient,
}

/// Normalized provider fault carrying recovery policy and user-safe detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderFault {
    pub kind: ProviderFaultKind,
    pub auto_recovers: bool,
    pub detail: &'static str,
}
