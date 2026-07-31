//! Persistence contracts for transactional outbox and inbox implementations.

/// A durable event awaiting publication after its owning transaction commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboxRecord {
    pub event_id: String,
    pub topic: String,
    pub partition_key: Vec<u8>,
    pub payload: Vec<u8>,
}

/// Records outbox events inside the same transaction as authoritative state.
pub trait TransactionalOutbox {
    type Error;

    /// Stages an event in the transaction's durable outbox.
    ///
    /// # Errors
    ///
    /// Returns the implementation's error when the row cannot be staged.
    fn insert(&mut self, record: OutboxRecord) -> Result<(), Self::Error>;
}

/// Detects duplicate event delivery inside the transaction applying an effect.
pub trait TransactionalInbox {
    type Error;

    /// Records an event identifier once and reports whether it was newly stored.
    ///
    /// # Errors
    ///
    /// Returns the implementation's error when the receipt cannot be checked or stored.
    fn record_once(&mut self, event_id: &str) -> Result<bool, Self::Error>;
}
