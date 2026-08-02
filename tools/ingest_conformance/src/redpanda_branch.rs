//! Redpanda durable-branch conformance over a real single broker.
//!
//! Publishes Section 11.2 envelopes through an idempotent `acks=all` producer on
//! one topic and reads the partition back, asserting byte-exact payloads, intact
//! ordering per key, and exact delivery accounting. A single dev broker proves
//! client/branch semantics only; TLS, ACLs, replication, and multi-broker failure
//! drills are not exercised and not claimed.

use axiusflow_streaming::{
    DurableEventEnvelope, DurableTopic, EventMetadataInput, RedpandaProducer,
    RedpandaProducerConfig, conformance,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{env, error::Error, fs, path::Path, time::Duration};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_2_redpanda_durable_branch";
const ENVELOPE_COUNT: usize = 64;
const PAYLOAD_BYTES: usize = 256;
const PARTITION_KEYS: usize = 4;
const RDKAFKA_VERSION: &str = "0.39.0";
const LIBRDKAFKA_VERSION: &str = "2.12.1";

#[derive(Serialize)]
struct ClientEvidence {
    crate_name: &'static str,
    crate_version: &'static str,
    librdkafka_version: &'static str,
    idempotent_producer: bool,
    acks: &'static str,
}

#[derive(Serialize)]
struct BehaviorEvidence {
    publish: &'static str,
    delivery_accounting: &'static str,
    byte_exact_readback: &'static str,
    per_key_ordering: &'static str,
    tls: &'static str,
    acls: &'static str,
    replication: &'static str,
    multi_broker_failover: &'static str,
}

#[derive(Serialize)]
struct RedpandaBranchReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    topic: String,
    envelopes_published: usize,
    envelopes_delivered: u64,
    deliveries_failed: u64,
    envelopes_read_back: usize,
    client: ClientEvidence,
    behavior: BehaviorEvidence,
    limitations: [&'static str; 4],
}

/// Runs the durable-branch conformance and writes one evidence artifact.
pub fn run(brokers: &str, report_path: &Path) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for Redpanda durable-branch evidence")?;
    let topic = DurableTopic::try_new("ci", "market", "bar", "normalized", 1)?;
    let topic_name = topic.name().to_string();
    let config = RedpandaProducerConfig {
        bootstrap_servers: brokers.to_string(),
        client_id: "axiusflow-conformance".to_string(),
        topic,
        maximum_in_flight: 128,
        delivery_capacity: 256,
        request_timeout: Duration::from_secs(30),
    };
    let mut producer = RedpandaProducer::connect(&config)?;

    for sequence in 0..ENVELOPE_COUNT {
        let key_index = sequence % PARTITION_KEYS;
        let key = format!("instrument-{key_index}");
        let envelope = envelope_for(sequence as u64)?;
        producer.publish(key.as_bytes(), &envelope)?;
    }
    let delivered = producer.flush()?;
    let health = producer.health();
    if delivered != ENVELOPE_COUNT as u64 || health.failed != 0 {
        return Err(format!(
            "Redpanda delivery accounting incomplete: delivered={delivered} failed={}",
            health.failed
        )
        .into());
    }

    verify_readback(brokers, &topic_name)?;

    let read_back_count = ENVELOPE_COUNT;
    let report = RedpandaBranchReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        topic: topic_name,
        envelopes_published: ENVELOPE_COUNT,
        envelopes_delivered: delivered,
        deliveries_failed: health.failed,
        envelopes_read_back: read_back_count,
        client: ClientEvidence {
            crate_name: "rdkafka",
            crate_version: RDKAFKA_VERSION,
            librdkafka_version: LIBRDKAFKA_VERSION,
            idempotent_producer: true,
            acks: "all",
        },
        behavior: BehaviorEvidence {
            publish: "passed",
            delivery_accounting: "passed",
            byte_exact_readback: "passed",
            per_key_ordering: "passed",
            tls: "not_exercised",
            acls: "not_exercised",
            replication: "not_exercised",
            multi_broker_failover: "not_exercised",
        },
        limitations: [
            "single_dev_broker",
            "plaintext_transport",
            "no_schema_registry",
            "no_failure_drills",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "redpanda_durable_branch=passed topic={} envelopes={} delivered={delivered} read_back={} idempotent=true acks=all tls=not_exercised report={}",
        report.topic,
        ENVELOPE_COUNT,
        read_back_count,
        report_path.display()
    );
    Ok(())
}

fn envelope_for(sequence: u64) -> Result<DurableEventEnvelope, Box<dyn Error>> {
    let mut payload = Vec::with_capacity(PAYLOAD_BYTES);
    payload.extend_from_slice(&sequence.to_le_bytes());
    let digest = Sha256::digest(format!("axiusflow-durable-payload-{sequence}"));
    while payload.len() < PAYLOAD_BYTES {
        payload.extend_from_slice(&digest);
    }
    payload.truncate(PAYLOAD_BYTES);
    Ok(DurableEventEnvelope::try_new(
        EventMetadataInput {
            event_id: format!("durable-conformance-{sequence}"),
            event_time_unix_nanos: 1_754_000_000_000_000_000 + sequence.cast_signed(),
            publication_time_unix_nanos: 1_754_000_000_000_001_000 + sequence.cast_signed(),
            producer: "axiusflow_ingest_conformance".to_string(),
            correlation_id: "durable-branch-conformance".to_string(),
            causation_id: format!("cause-{}", sequence / 16),
            partition_id: 0,
            ownership_epoch: 1,
        },
        payload,
    )?)
}

/// Reads the partition back and asserts byte-exact payloads, the published key
/// mapping, and per-key ordering.
fn verify_readback(brokers: &str, topic_name: &str) -> Result<(), Box<dyn Error>> {
    let read_back = conformance::read_partition(
        brokers,
        "axiusflow-conformance-readback",
        topic_name,
        0,
        ENVELOPE_COUNT,
        Duration::from_secs(30),
    )?;
    if read_back.len() != ENVELOPE_COUNT {
        return Err(format!(
            "Redpanda read-back returned {} of {ENVELOPE_COUNT} envelopes",
            read_back.len()
        )
        .into());
    }
    let mut per_key_sequences: [u64; PARTITION_KEYS] = [0; PARTITION_KEYS];
    for (index, (key, payload)) in read_back.iter().enumerate() {
        let expected = envelope_for(index as u64)?;
        if payload != &expected.encode() {
            return Err(format!("Redpanda read-back payload {index} diverged").into());
        }
        let key_text = String::from_utf8_lossy(key);
        let key_index = key_text
            .strip_prefix("instrument-")
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or("Redpanda read-back key was rewritten")?;
        if key_index >= PARTITION_KEYS {
            return Err("Redpanda read-back key outside the published set".into());
        }
        let sequence = index as u64;
        if usize::try_from(sequence).unwrap_or(usize::MAX) % PARTITION_KEYS != key_index {
            return Err("Redpanda read-back key does not match the published mapping".into());
        }
        if sequence < per_key_sequences[key_index] {
            return Err("Redpanda per-key ordering regressed".into());
        }
        per_key_sequences[key_index] = sequence;
    }
    Ok(())
}
