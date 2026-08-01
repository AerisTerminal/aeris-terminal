//! Deterministic canonical events and queue policies for this crate's unit tests.

use crate::bounded_queue::{OverflowAction, QueuePolicy};
use crate::canonical_event::{
    CanonicalEventHeader, CanonicalMarketEvent, CanonicalTimestamps, SemanticClass,
};
use std::num::{NonZeroU64, NonZeroUsize};

pub(crate) fn event(
    sequence: u64,
    epoch: u64,
    semantic_class: SemanticClass,
) -> CanonicalMarketEvent {
    let receive = i64::try_from(sequence).unwrap_or(i64::MAX) * 1_000;
    CanonicalMarketEvent::try_new(
        CanonicalEventHeader {
            event_id: format!("event-{epoch}-{sequence}"),
            event_time_unix_nanos: receive - 20,
            publication_time_unix_nanos: receive + 30,
            producer: "test-decoder".to_string(),
            correlation_id: "correlation-1".to_string(),
            causation_id: String::new(),
            entitlement_revision: "entitlement-1".to_string(),
            instrument_id: "instrument-1".to_string(),
            venue_id: "venue-1".to_string(),
            source_id: "source-1".to_string(),
            series_identity: None,
            source_sequence: sequence,
            partition_id: 7,
            ownership_epoch: epoch,
            timestamps: CanonicalTimestamps {
                exchange_unix_nanos: receive - 20,
                provider_receive_unix_nanos: receive - 10,
                nic_receive_unix_nanos: None,
                axiusflow_receive_unix_nanos: receive,
                normalized_unix_nanos: receive + 10,
                fanout_enqueue_unix_nanos: Some(receive + 20),
            },
            nic_timestamp_source: None,
            correction_flags: 0,
            quality_flags: 0,
            schema_version: 1,
            semantic_class,
        },
        b"payload",
    )
    .expect("test event is valid")
}

pub(crate) fn policy(
    semantic_class: SemanticClass,
    overflow_action: OverflowAction,
    item_capacity: usize,
) -> QueuePolicy {
    QueuePolicy {
        name: "test-queue".to_string(),
        producer: "test-producer".to_string(),
        consumer: "test-consumer".to_string(),
        item_capacity: NonZeroUsize::new(item_capacity).expect("capacity is non-zero"),
        byte_capacity: NonZeroUsize::new(8_192).expect("byte capacity is non-zero"),
        semantic_class,
        overflow_action,
        maximum_residence_nanos: NonZeroU64::new(100).expect("residence is non-zero"),
        recovery: "install-snapshot".to_string(),
        alert_threshold_items: NonZeroUsize::MIN,
    }
}
