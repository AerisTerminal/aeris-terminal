//! Transport- and provider-independent application use-case contracts.

#[cfg(any(test, feature = "test-support"))]
mod embedded_source;
mod errors;
mod generation;
mod provenance;
mod replay_snapshot;
mod stream;

#[cfg(any(test, feature = "test-support"))]
pub use embedded_source::{EmbeddedReplaySource, LoadEmbeddedReplay, MAX_EMBEDDED_REPLAY_BARS};
pub use errors::ReplayValidationError;
pub use generation::{
    MarketBarClientModel, MarketBarModelOutcome, MarketGeneration, ReplayRecoveryCommand,
    ResnapshotReason,
};
pub use provenance::{
    ProvenancedMarketBar, ReplayProvenance, RequestContext, validate_provenanced_market_bar,
};
pub use replay_snapshot::{
    ReplaySession, ReplaySnapshot, ReplayStreamUpdate, ReplayTailOperation, ReplayTailUpdate,
};
pub use stream::{
    MarketEventProvenance, Provenanced, SequenceDecision, SnapshotEvidence, StreamDelta,
};
