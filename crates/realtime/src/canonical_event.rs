//! Provider-neutral canonical market events and their timestamp vocabulary.

use crate::errors::RealtimeError;

pub const MAX_CANONICAL_PAYLOAD_BYTES: usize = 4_096;

/// Delivery and recovery semantics for one event stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticClass {
    StateReplace,
    OrderedDelta,
    ReliableEvent,
    AuthoritativeEvent,
    Snapshot,
}

/// Clock source retained for an optional NIC timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NicTimestampSource {
    SocketSoftware,
    KernelSoftware,
    NicHardware,
}

/// Complete Stage 1 canonical market timestamp vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalTimestamps {
    pub exchange_unix_nanos: i64,
    pub provider_receive_unix_nanos: i64,
    pub nic_receive_unix_nanos: Option<i64>,
    pub axiusflow_receive_unix_nanos: i64,
    pub normalized_unix_nanos: i64,
    pub fanout_enqueue_unix_nanos: Option<i64>,
}

impl CanonicalTimestamps {
    /// Validates only boundaries known to share Axiusflow's measured clock.
    /// External provider/exchange clocks remain provenance, not subtraction input.
    ///
    /// # Errors
    ///
    /// Returns an error when normalize or fanout precedes local receipt.
    pub fn validate(self) -> Result<(), RealtimeError> {
        if self.normalized_unix_nanos < self.axiusflow_receive_unix_nanos {
            return Err(RealtimeError::TimestampRegression {
                earlier: self.axiusflow_receive_unix_nanos,
                later: self.normalized_unix_nanos,
            });
        }
        if self
            .fanout_enqueue_unix_nanos
            .is_some_and(|fanout| fanout < self.normalized_unix_nanos)
        {
            return Err(RealtimeError::TimestampRegression {
                earlier: self.normalized_unix_nanos,
                later: self.fanout_enqueue_unix_nanos.unwrap_or_default(),
            });
        }
        Ok(())
    }
}

/// Versioned identity required when canonical payload semantics depend on a series definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalSeriesIdentity {
    pub instrument_revision: u64,
    pub definition_id: String,
    pub definition_version: u32,
    pub interval_seconds: u32,
}

/// Provider-neutral canonical market event header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalEventHeader {
    pub event_id: String,
    pub event_time_unix_nanos: i64,
    pub publication_time_unix_nanos: i64,
    pub producer: String,
    pub correlation_id: String,
    pub causation_id: String,
    pub entitlement_revision: String,
    pub instrument_id: String,
    pub venue_id: String,
    pub source_id: String,
    pub series_identity: Option<CanonicalSeriesIdentity>,
    pub source_sequence: u64,
    pub partition_id: u32,
    pub ownership_epoch: u64,
    pub timestamps: CanonicalTimestamps,
    pub nic_timestamp_source: Option<NicTimestampSource>,
    pub correction_flags: u64,
    pub quality_flags: u64,
    pub schema_version: u32,
    pub semantic_class: SemanticClass,
}

/// Materialized canonical event. No driver-owned pointer crosses this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalMarketEvent {
    header: CanonicalEventHeader,
    payload: Vec<u8>,
}

impl CanonicalMarketEvent {
    /// Creates a bounded, fully owned canonical event.
    ///
    /// # Errors
    ///
    /// Returns an error for missing identity, ordering, schema, timestamp, or payload bounds.
    pub fn try_new(header: CanonicalEventHeader, payload: &[u8]) -> Result<Self, RealtimeError> {
        for (field, value) in [
            ("event_id", header.event_id.as_str()),
            ("producer", header.producer.as_str()),
            ("entitlement_revision", header.entitlement_revision.as_str()),
            ("instrument_id", header.instrument_id.as_str()),
            ("venue_id", header.venue_id.as_str()),
            ("source_id", header.source_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(RealtimeError::EmptyIdentity(field));
            }
        }
        if let Some(series) = &header.series_identity {
            if series.instrument_revision == 0 {
                return Err(RealtimeError::ZeroInstrumentRevision);
            }
            if series.definition_id.trim().is_empty() {
                return Err(RealtimeError::EmptyIdentity("series.definition_id"));
            }
            if series.definition_version == 0 {
                return Err(RealtimeError::ZeroSeriesDefinitionVersion);
            }
            if series.interval_seconds == 0 {
                return Err(RealtimeError::ZeroSeriesInterval);
            }
        }
        if header.source_sequence == 0 {
            return Err(RealtimeError::ZeroSourceSequence);
        }
        if header.ownership_epoch == 0 {
            return Err(RealtimeError::ZeroOwnershipEpoch);
        }
        if header.schema_version == 0 {
            return Err(RealtimeError::ZeroSchemaVersion);
        }
        header.timestamps.validate()?;
        if payload.len() > MAX_CANONICAL_PAYLOAD_BYTES {
            return Err(RealtimeError::PayloadLimitExceeded {
                requested: payload.len(),
                maximum: MAX_CANONICAL_PAYLOAD_BYTES,
            });
        }
        Ok(Self {
            header,
            payload: payload.to_vec(),
        })
    }

    #[must_use]
    pub const fn header(&self) -> &CanonicalEventHeader {
        &self.header
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[must_use]
    pub fn encoded_size_bytes(&self) -> usize {
        self.payload
            .len()
            .saturating_add(self.header.event_id.len())
            .saturating_add(self.header.producer.len())
            .saturating_add(self.header.correlation_id.len())
            .saturating_add(self.header.causation_id.len())
            .saturating_add(self.header.entitlement_revision.len())
            .saturating_add(self.header.instrument_id.len())
            .saturating_add(self.header.venue_id.len())
            .saturating_add(self.header.source_id.len())
            .saturating_add(
                self.header
                    .series_identity
                    .as_ref()
                    .map_or(0, |series| series.definition_id.len().saturating_add(24)),
            )
            .saturating_add(128)
    }
}

#[cfg(test)]
mod tests {
    use super::{CanonicalMarketEvent, MAX_CANONICAL_PAYLOAD_BYTES, SemanticClass};
    use crate::errors::RealtimeError;
    use crate::test_fixture::event;

    #[test]
    fn canonical_event_rejects_local_timestamp_regression_and_unbounded_payload() {
        let mut header = event(1, 1, SemanticClass::OrderedDelta).header().clone();
        header.timestamps.normalized_unix_nanos =
            header.timestamps.axiusflow_receive_unix_nanos - 1;
        assert!(matches!(
            CanonicalMarketEvent::try_new(header, b"payload"),
            Err(RealtimeError::TimestampRegression { .. })
        ));

        let header = event(1, 1, SemanticClass::OrderedDelta).header().clone();
        assert!(matches!(
            CanonicalMarketEvent::try_new(header, &vec![0_u8; MAX_CANONICAL_PAYLOAD_BYTES + 1],),
            Err(RealtimeError::PayloadLimitExceeded { .. })
        ));
    }
}
