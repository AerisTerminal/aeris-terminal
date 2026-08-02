//! Durable event backbone boundary for Redpanda and raw capture.
//!
//! Consumers publish canonical events through the Section 11.2 envelope contract
//! and never touch Kafka or object-storage types directly.

mod capture;
mod envelope;
mod errors;
mod review;

#[cfg(all(target_os = "linux", feature = "redpanda"))]
mod producer;

pub use capture::{CaptureEndpoint, CapturedObject, MAXIMUM_CAPTURE_BYTES, RawCaptureClient};
pub use envelope::{
    DURABLE_SCHEMA_VERSION, DurableEventEnvelope, DurableTopic, EventMetadataInput,
    MAXIMUM_ENVELOPE_BYTES, MAXIMUM_PAYLOAD_BYTES,
};
pub use errors::StreamingError;
#[cfg(all(target_os = "linux", feature = "redpanda"))]
pub use producer::{ProducerHealth, RedpandaProducer, RedpandaProducerConfig, conformance};
pub use review::{
    BUILD_REVIEW, CLIENT_DEPENDENCY_SELECTED, LICENSE_REVIEW, MAINTENANCE_REVIEW,
    MISSING_DURABLE_EVIDENCE, PROVENANCE_REVIEW, SAFETY_REVIEW,
};
