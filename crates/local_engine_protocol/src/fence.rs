//! Stale-update fencing across engine, workspace, selection, and provider generations.

/// Generation fence carried by snapshot and delta messages.
///
/// Callers keep their current fence and compare each incoming message's fence
/// against it; [`Fence::accepts`] returns `true` only when every generation
/// matches, so any stale dimension rejects the message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fence {
    /// Engine process epoch; changes on engine restart.
    pub engine_epoch: u64,
    /// Workspace revision the message belongs to.
    pub workspace_revision: u64,
    /// Selection generation the message was produced under.
    pub selection_generation: u64,
    /// Provider generation the message was produced under.
    pub provider_generation: u64,
}

impl Fence {
    /// Returns `true` only when every generation of `snapshot` equals this
    /// fence. Compare against the caller's current fence: any mismatch means
    /// the message is stale and must be dropped.
    #[must_use]
    pub fn accepts(&self, snapshot: Self) -> bool {
        self.eq(&snapshot)
    }
}
