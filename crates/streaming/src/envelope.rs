//! Section 11.2 durable event envelope and Section 11.3 topic contract.
//!
//! Every durable event carries the protobuf `EventMetadata` fields: event ID,
//! schema version, producer, timestamps with clock meaning, correlation and
//! causation identifiers, partition, and ownership epoch. The wire form is a
//! length-prefixed metadata message followed by the payload, so consumers can
//! bound and reject before decoding.

use crate::errors::StreamingError;
use axiusflow_protocols::protobuf::axiusflow::common::v1::EventMetadata;
use prost::Message;

/// Schema version stamped on every envelope this crate emits.
pub const DURABLE_SCHEMA_VERSION: u32 = 1;
/// Largest payload one envelope may carry.
pub const MAXIMUM_PAYLOAD_BYTES: usize = 1_048_576;
/// Largest encoded envelope accepted on decode.
pub const MAXIMUM_ENVELOPE_BYTES: usize = MAXIMUM_PAYLOAD_BYTES + 4_096;
/// Largest accepted event/correlation/causation identifier.
const MAXIMUM_IDENTIFIER_BYTES: usize = 128;
/// Largest accepted producer name.
const MAXIMUM_PRODUCER_BYTES: usize = 64;
/// Largest accepted topic segment.
const MAXIMUM_TOPIC_SEGMENT_BYTES: usize = 64;

/// Metadata fields supplied by the publishing domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventMetadataInput {
    pub event_id: String,
    pub event_time_unix_nanos: i64,
    pub publication_time_unix_nanos: i64,
    pub producer: String,
    pub correlation_id: String,
    pub causation_id: String,
    pub partition_id: u32,
    pub ownership_epoch: u64,
}

/// One durable event with its Section 11.2 metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableEventEnvelope {
    metadata: EventMetadataInput,
    payload: Vec<u8>,
}

impl DurableEventEnvelope {
    /// Builds an envelope after validating every bounded field.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers, timestamps, or payload bounds.
    pub fn try_new(input: EventMetadataInput, payload: Vec<u8>) -> Result<Self, StreamingError> {
        validate_identifier(&input.event_id)?;
        validate_producer(&input.producer)?;
        if input.correlation_id.len() > MAXIMUM_IDENTIFIER_BYTES
            || input.causation_id.len() > MAXIMUM_IDENTIFIER_BYTES
        {
            return Err(StreamingError::InvalidEventId);
        }
        if input.event_time_unix_nanos < 0 || input.publication_time_unix_nanos < 0 {
            return Err(StreamingError::InvalidTimestamp);
        }
        if input.payload_size_over_limit(&payload) {
            return Err(StreamingError::PayloadTooLarge(payload.len()));
        }
        Ok(Self {
            metadata: input,
            payload,
        })
    }

    /// The validated metadata.
    #[must_use]
    pub const fn metadata(&self) -> &EventMetadataInput {
        &self.metadata
    }

    /// The event payload bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Encodes as `u32le metadata length || protobuf metadata || u32le payload
    /// length || payload`, so a truncated payload is rejected instead of
    /// silently shortened.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let metadata = EventMetadata {
            event_id: self.metadata.event_id.clone(),
            event_time_unix_nanos: self.metadata.event_time_unix_nanos,
            publication_time_unix_nanos: self.metadata.publication_time_unix_nanos,
            producer: self.metadata.producer.clone(),
            schema_version: DURABLE_SCHEMA_VERSION,
            correlation_id: self.metadata.correlation_id.clone(),
            causation_id: self.metadata.causation_id.clone(),
            partition_id: self.metadata.partition_id,
            ownership_epoch: self.metadata.ownership_epoch,
        };
        let metadata_bytes = metadata.encode_to_vec();
        let mut encoded = Vec::with_capacity(4 + metadata_bytes.len() + self.payload.len());
        encoded.extend_from_slice(
            &u32::try_from(metadata_bytes.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        encoded.extend_from_slice(&metadata_bytes);
        encoded.extend_from_slice(
            &u32::try_from(self.payload.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        encoded.extend_from_slice(&self.payload);
        encoded
    }

    /// Decodes and revalidates one envelope.
    ///
    /// # Errors
    ///
    /// Returns an error for truncation, oversize, or invalid metadata.
    pub fn decode(encoded: &[u8]) -> Result<Self, StreamingError> {
        if encoded.len() > MAXIMUM_ENVELOPE_BYTES {
            return Err(StreamingError::EnvelopeTooLarge(encoded.len()));
        }
        let length_bytes = encoded.get(..4).ok_or(StreamingError::EnvelopeTruncated)?;
        let metadata_length =
            u32::from_le_bytes(length_bytes.try_into().unwrap_or([0xff; 4])) as usize;
        let metadata_end = 4_usize
            .checked_add(metadata_length)
            .ok_or(StreamingError::EnvelopeTruncated)?;
        let metadata_bytes = encoded
            .get(4..metadata_end)
            .ok_or(StreamingError::EnvelopeTruncated)?;
        let metadata =
            EventMetadata::decode(metadata_bytes).map_err(|_| StreamingError::EnvelopeTruncated)?;
        if metadata.schema_version != DURABLE_SCHEMA_VERSION {
            return Err(StreamingError::InvalidTimestamp);
        }
        let length_bytes = encoded
            .get(metadata_end..metadata_end + 4)
            .ok_or(StreamingError::EnvelopeTruncated)?;
        let payload_length =
            u32::from_le_bytes(length_bytes.try_into().unwrap_or([0xff; 4])) as usize;
        let payload_bytes = encoded
            .get(metadata_end + 4..)
            .ok_or(StreamingError::EnvelopeTruncated)?;
        if payload_bytes.len() != payload_length {
            return Err(StreamingError::EnvelopeTruncated);
        }
        let payload = payload_bytes.to_vec();
        Self::try_new(
            EventMetadataInput {
                event_id: metadata.event_id,
                event_time_unix_nanos: metadata.event_time_unix_nanos,
                publication_time_unix_nanos: metadata.publication_time_unix_nanos,
                producer: metadata.producer,
                correlation_id: metadata.correlation_id,
                causation_id: metadata.causation_id,
                partition_id: metadata.partition_id,
                ownership_epoch: metadata.ownership_epoch,
            },
            payload,
        )
    }
}

impl EventMetadataInput {
    fn payload_size_over_limit(&self, payload: &[u8]) -> bool {
        let _ = self;
        payload.len() > MAXIMUM_PAYLOAD_BYTES
    }
}

fn validate_identifier(value: &str) -> Result<(), StreamingError> {
    if value.is_empty() || value.len() > MAXIMUM_IDENTIFIER_BYTES {
        return Err(StreamingError::InvalidEventId);
    }
    Ok(())
}

fn validate_producer(value: &str) -> Result<(), StreamingError> {
    if value.is_empty() || value.len() > MAXIMUM_PRODUCER_BYTES {
        return Err(StreamingError::InvalidProducer);
    }
    Ok(())
}

/// Section 11.2 topic name: `<environment>.<domain>.<entity>.<event_family>.v<major>`.
///
/// Topics are never created per symbol or user; segments must be lowercase
/// `snake_case` so broker configuration stays enumerable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableTopic(String);

impl DurableTopic {
    /// Builds one validated topic name.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, or non-`snake_case` segments.
    pub fn try_new(
        environment: &str,
        domain: &str,
        entity: &str,
        event_family: &str,
        major_version: u32,
    ) -> Result<Self, StreamingError> {
        for segment in [environment, domain, entity, event_family] {
            validate_topic_segment(segment)?;
        }
        Ok(Self(format!(
            "{environment}.{domain}.{entity}.{event_family}.v{major_version}"
        )))
    }

    /// The full topic name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

fn validate_topic_segment(segment: &str) -> Result<(), StreamingError> {
    if segment.is_empty()
        || segment.len() > MAXIMUM_TOPIC_SEGMENT_BYTES
        || !segment.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
        || segment.starts_with(|character: char| character.is_ascii_digit())
    {
        return Err(StreamingError::InvalidTopicSegment);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DurableEventEnvelope, DurableTopic, EventMetadataInput, MAXIMUM_PAYLOAD_BYTES};

    fn input() -> EventMetadataInput {
        EventMetadataInput {
            event_id: "event-1".to_string(),
            event_time_unix_nanos: 10,
            publication_time_unix_nanos: 11,
            producer: "market_data_plane".to_string(),
            correlation_id: "corr-1".to_string(),
            causation_id: "cause-1".to_string(),
            partition_id: 7,
            ownership_epoch: 3,
        }
    }

    #[test]
    fn envelope_round_trips_byte_exact() {
        let envelope =
            DurableEventEnvelope::try_new(input(), b"payload".to_vec()).expect("valid envelope");
        let decoded = DurableEventEnvelope::decode(&envelope.encode()).expect("decodes");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn envelope_rejects_truncation_and_oversize() {
        let envelope =
            DurableEventEnvelope::try_new(input(), b"payload".to_vec()).expect("valid envelope");
        let encoded = envelope.encode();
        assert!(DurableEventEnvelope::decode(&encoded[..encoded.len() - 1]).is_err());
        assert!(DurableEventEnvelope::decode(&encoded[..2]).is_err());
        assert!(
            DurableEventEnvelope::try_new(input(), vec![0; MAXIMUM_PAYLOAD_BYTES + 1]).is_err()
        );
    }

    #[test]
    fn envelope_rejects_invalid_metadata() {
        let mut empty_id = input();
        empty_id.event_id = String::new();
        assert!(DurableEventEnvelope::try_new(empty_id, Vec::new()).is_err());
        let mut negative_time = input();
        negative_time.event_time_unix_nanos = -1;
        assert!(DurableEventEnvelope::try_new(negative_time, Vec::new()).is_err());
    }

    #[test]
    fn topic_names_follow_the_section_11_2_shape() {
        let topic =
            DurableTopic::try_new("prod", "market", "bar", "normalized", 1).expect("valid topic");
        assert_eq!(topic.name(), "prod.market.bar.normalized.v1");
        assert!(DurableTopic::try_new("Prod", "market", "bar", "normalized", 1).is_err());
        assert!(DurableTopic::try_new("prod", "market", "bar", "9lives", 1).is_err());
        assert!(DurableTopic::try_new("prod", "", "bar", "normalized", 1).is_err());
    }
}
