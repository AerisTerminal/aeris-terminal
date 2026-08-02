//! Recorded dependency review text for the durable streaming clients.

pub const CLIENT_DEPENDENCY_SELECTED: bool = cfg!(all(target_os = "linux", feature = "redpanda"));
pub const SAFETY_REVIEW: &str = "rdkafka 0.39.0 unsafe FFI to librdkafka is isolated in one private producer module behind bounded publish and flush calls; delivery evidence returns through one context channel instead of shared mutable state";
pub const LICENSE_REVIEW: &str = "rdkafka 0.39.0 is MIT licensed; its bundled librdkafka builds from the rdkafka-sys crate source under librdkafka's BSD-2-Clause terms";
pub const PROVENANCE_REVIEW: &str = "rdkafka 0.39.0 registry checksum recorded in Cargo.lock; rdkafka-sys 4.10.0+2.12.1 builds vendored librdkafka 2.12.1 with cmake";
pub const MAINTENANCE_REVIEW: &str = "rust-rdkafka is the mature maintained Rust Kafka client with 33M downloads; the pure-Rust rskafka 0.6.0 was rejected because it lacks idempotent-producer semantics, which Section 11.1 requires for durable classes";
pub const BUILD_REVIEW: &str = "the vendored librdkafka build compiles only under the redpanda feature on Linux; portable builds neither compile nor link it";
pub const MISSING_DURABLE_EVIDENCE: &str = "TLS, ACLs, replication and tiered-storage deployment, schema-registry governance, multi-broker failure drills";
