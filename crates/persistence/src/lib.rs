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

#[cfg(test)]
mod tests {
    use super::{OutboxRecord, TransactionalInbox, TransactionalOutbox};
    use std::{collections::BTreeSet, convert::Infallible};

    #[derive(Default)]
    struct MemoryTransaction {
        outbox: Vec<OutboxRecord>,
        inbox: BTreeSet<String>,
    }

    impl TransactionalOutbox for MemoryTransaction {
        type Error = Infallible;

        fn insert(&mut self, record: OutboxRecord) -> Result<(), Self::Error> {
            self.outbox.push(record);
            Ok(())
        }
    }

    impl TransactionalInbox for MemoryTransaction {
        type Error = Infallible;

        fn record_once(&mut self, event_id: &str) -> Result<bool, Self::Error> {
            Ok(self.inbox.insert(event_id.to_string()))
        }
    }

    fn record(event_id: &str) -> OutboxRecord {
        OutboxRecord {
            event_id: event_id.to_string(),
            topic: "canonical_market_events".to_string(),
            partition_key: b"instrument-1".to_vec(),
            payload: b"payload".to_vec(),
        }
    }

    #[test]
    fn outbox_record_preserves_owned_routing_and_payload_bytes() {
        let original = record("event-1");
        let cloned = original.clone();
        assert_eq!(cloned, original);
        assert_eq!(cloned.partition_key, b"instrument-1");
        assert_eq!(cloned.payload, b"payload");
    }

    #[test]
    fn transactional_outbox_stages_records_in_call_order() {
        let mut transaction = MemoryTransaction::default();
        transaction
            .insert(record("event-1"))
            .expect("insert succeeds");
        transaction
            .insert(record("event-2"))
            .expect("insert succeeds");
        assert_eq!(
            transaction
                .outbox
                .iter()
                .map(|record| record.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["event-1", "event-2"]
        );
    }

    #[test]
    fn transactional_inbox_reports_only_the_first_receipt_as_new() {
        let mut transaction = MemoryTransaction::default();
        assert!(
            transaction
                .record_once("event-1")
                .expect("first receipt succeeds")
        );
        assert!(
            !transaction
                .record_once("event-1")
                .expect("duplicate receipt succeeds")
        );
        assert!(
            transaction
                .record_once("event-2")
                .expect("next receipt succeeds")
        );
        assert_eq!(transaction.inbox.len(), 2);
    }

    #[test]
    fn inbox_and_outbox_can_share_one_transaction_boundary() {
        let mut transaction = MemoryTransaction::default();
        if transaction
            .record_once("input-event")
            .expect("receipt succeeds")
        {
            transaction
                .insert(record("derived-event"))
                .expect("derived event is staged");
        }
        if transaction
            .record_once("input-event")
            .expect("duplicate receipt succeeds")
        {
            transaction
                .insert(record("duplicate-derived-event"))
                .expect("insert succeeds");
        }
        assert_eq!(transaction.outbox, vec![record("derived-event")]);
    }
}
