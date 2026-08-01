//! Dedicated market-bar provider packet corpus and its packet-to-Origin conformance.
//!
//! This module owns the canonical market-bar wire payload, its stateful decoder, the
//! fenced-partition publication and bounded direct/durable fanout for that corpus, and
//! the Origin host edge assertions. It reuses the shared queue policy and fanout
//! acceptance helpers rather than redefining partition semantics.

use crate::canonical_market_bar::accept_for_fanout;
use crate::harness_error::{
    ConformanceHarnessError, FixtureDecodeError, market_stream_error, realtime_error,
};
use crate::ingest_outcomes_semantically_equivalent;
use crate::packet_corpus::{
    ConformanceOutcome, ETHERNET_HEADER_BYTES, FixtureTransport, IPV4_HEADER_BYTES,
    TCP_HEADER_BYTES, UDP_HEADER_BYTES,
};
use crate::realtime_recovery::queue_policy;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarReplayPort, ReplayProvenance,
    ReplayStreamUpdate,
};
use axiusflow_chart_integration::run_origin_gpui_host_sample;
use axiusflow_market_protocol_adapter::{
    try_encode_canonical_market_bar_payload, try_project_canonical_market_bar,
    try_project_canonical_market_bar_snapshot,
};
use axiusflow_realtime::{
    BoundedEventBranch, CanonicalEventHeader, CanonicalMarketEvent, CanonicalSeriesIdentity,
    CanonicalSnapshot, CanonicalTimestamps, DirectDurableFanout, FencedPartition,
    NicTimestampSource, PartitionOwner, QueueOutcome, SemanticClass,
};
use axiusflow_transport::{
    FixtureFrame, IngestDriver, QueueBinding, ReceiveBatch, ReceiveMetadata, TimestampSource,
};
use std::num::NonZeroUsize;

const MARKET_BAR_PROVIDER_MAGIC: &[u8; 4] = b"AXB1";
const MARKET_BAR_PROVIDER_PAYLOAD_BYTES: usize = 4
    + core::mem::size_of::<u64>()
    + core::mem::size_of::<i64>()
    + axiusflow_market_protocol_adapter::CANONICAL_MARKET_BAR_PAYLOAD_BYTES;
const MARKET_BAR_PACKET_ORIGIN_REQUIRED_CHECKS: u8 = 0x3f;

/// Dedicated deterministic provider packets carrying complete canonical market-bar values.
///
/// This corpus is separate from [`deterministic_ingest_corpus`], whose compact provider payload
/// intentionally represents a generic price event. It retains the validated embedded fixture's
/// instrument and bar-definition identity while exercising packet decode and receive provenance.
pub struct DeterministicMarketBarPacketCorpus {
    source: axiusflow_application::ReplaySnapshot,
    frames: Vec<FixtureFrame>,
}

impl DeterministicMarketBarPacketCorpus {
    #[must_use]
    pub fn frames(&self) -> &[FixtureFrame] {
        &self.frames
    }
}

/// Result of driving one market-bar packet corpus through fenced partition publication,
/// independently bounded direct/durable fanout, canonical projection, and Origin.
#[derive(Clone, Debug)]
pub struct MarketBarPacketOriginConformance {
    outcomes: Vec<ConformanceOutcome>,
    partition_fanout_last_source_sequence: u64,
    origin_last_source_sequence: u64,
    origin_primitive_count: usize,
    gpui_plan_operations: u32,
    gpui_mesh_vertices: u32,
    passed_checks: u8,
}

impl MarketBarPacketOriginConformance {
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.passed_checks == MARKET_BAR_PACKET_ORIGIN_REQUIRED_CHECKS
    }

    #[must_use]
    pub fn outcome_count(&self) -> usize {
        self.outcomes.len()
    }

    #[must_use]
    pub fn accepted_canonical_event_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ConformanceOutcome::Accepted(_)))
            .count()
    }

    #[must_use]
    pub const fn partition_fanout_last_source_sequence(&self) -> u64 {
        self.partition_fanout_last_source_sequence
    }

    #[must_use]
    pub const fn origin_last_source_sequence(&self) -> u64 {
        self.origin_last_source_sequence
    }

    #[must_use]
    pub fn semantically_equivalent(&self, other: &Self) -> bool {
        self.is_complete()
            && other.is_complete()
            && ingest_outcomes_semantically_equivalent(&self.outcomes, &other.outcomes)
            && self.partition_fanout_last_source_sequence
                == other.partition_fanout_last_source_sequence
            && self.origin_last_source_sequence == other.origin_last_source_sequence
            && self.origin_primitive_count == other.origin_primitive_count
            && self.gpui_plan_operations == other.gpui_plan_operations
            && self.gpui_mesh_vertices == other.gpui_mesh_vertices
    }
}

/// Builds contiguous market-bar packets followed by duplicate, gap, latch, and malformed cases.
///
/// # Errors
///
/// Returns an error if the embedded identity fixture or a canonical market-bar payload is invalid.
pub fn deterministic_market_bar_packet_corpus()
-> Result<DeterministicMarketBarPacketCorpus, ConformanceHarnessError> {
    let source = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 5 })
        .map_err(market_stream_error)?;
    let scenarios = [
        (FixtureTransport::Udp, 0_usize),
        (FixtureTransport::Tcp, 1),
        (FixtureTransport::Udp, 2),
        (FixtureTransport::Tcp, 2),
        (FixtureTransport::Udp, 4),
        (FixtureTransport::Tcp, 3),
    ];
    let mut frames = scenarios
        .into_iter()
        .map(|(transport, index)| market_bar_packet_frame(transport, &source.bars()[index]))
        .collect::<Result<Vec<_>, _>>()?;
    let malformed_timestamp = source.bars().last().map_or(1, |item| {
        item.provenance()
            .axiusflow_receive_timestamp_unix_nanos
            .saturating_add(1_000)
    });
    frames.push(FixtureFrame {
        bytes: vec![0_u8; 12],
        metadata: ReceiveMetadata {
            receive_timestamp_unix_nanos: malformed_timestamp,
            timestamp_source: TimestampSource::SocketSoftware,
            queue_id: 0,
        },
    });
    Ok(DeterministicMarketBarPacketCorpus { source, frames })
}

/// Runs one bounded driver over the dedicated market-bar corpus and reaches the Origin host edge.
///
/// The callback seeds a started native driver. Every receive batch is released before owned
/// canonical events enter the fenced partition. The accepted delta is published to independently
/// bounded direct and durable-tap queues; only the direct event drives model/Origin projection.
/// No borrowed driver memory crosses the canonical boundary.
///
/// # Errors
///
/// Returns an error for driver lifecycle, packet decoding, canonical/replay projection, or Origin
/// host preparation failures.
pub fn run_market_bar_packet_to_origin_conformance_after_start<Driver>(
    driver: &mut Driver,
    corpus: &DeterministicMarketBarPacketCorpus,
    after_start: impl FnOnce(&mut Driver, &[FixtureFrame]) -> Result<(), ConformanceHarnessError>,
) -> Result<MarketBarPacketOriginConformance, ConformanceHarnessError>
where
    Driver: IngestDriver,
{
    driver
        .bind_queue(QueueBinding {
            queue_id: 0,
            maximum_batch_items: NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
            maximum_frame_bytes: NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
        })
        .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
    driver
        .start()
        .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
    after_start(driver, corpus.frames())?;

    let mut decoder = FixtureMarketBarProviderDecoder::new(&corpus.source);
    let mut outcomes = Vec::with_capacity(corpus.frames().len());
    loop {
        let batch = driver
            .receive_batch()
            .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        let count = batch.frame_count();
        for index in 0..count {
            let frame = batch
                .frame(index)
                .ok_or(ConformanceHarnessError::MissingFrame(index))?;
            outcomes.push(decoder.decode(frame.bytes, frame.metadata));
        }
        batch.release();
        if count == 0 {
            break;
        }
    }
    driver
        .shutdown()
        .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
    let health = driver.health();
    let lifecycle_complete = health.lifecycle == axiusflow_transport::DriverLifecycle::Stopped
        && health.released_batches > 0
        && health.abandoned_batches == 0;

    let accepted = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            ConformanceOutcome::Accepted(event) => Some(event.as_ref().clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if accepted.len() != 3 {
        return Err(ConformanceHarnessError::MarketStream(format!(
            "market-bar packet corpus accepted {} events instead of three",
            accepted.len()
        )));
    }
    let canonical_snapshot = CanonicalSnapshot::try_from_events(7, 1, 1, 1, accepted[..2].to_vec())
        .map_err(realtime_error)?;
    let baseline = try_project_canonical_market_bar_snapshot(
        &canonical_snapshot,
        corpus.source.instrument(),
        ReplayProvenance::EmbeddedFixture,
        corpus.source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let fanout = run_market_bar_packet_partition_fanout(&canonical_snapshot, &accepted[2])?;
    let delta_item = try_project_canonical_market_bar(
        &fanout.direct_event,
        corpus.source.instrument(),
        corpus.source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let delta = axiusflow_application::StreamDelta::try_new(2, 3, delta_item)
        .map_err(market_stream_error)?;
    let host = run_origin_gpui_host_sample(&baseline, ReplayStreamUpdate::Delta(delta))
        .map_err(|error| ConformanceHarnessError::MarketStream(error.to_string()))?;

    let checks = [
        market_bar_packet_outcome_shape(&outcomes),
        baseline.sequence_range() == (1, 2) && baseline.bars().len() == 2,
        host.origin_last_source_sequence == 3,
        host.origin_primitive_count > 0
            && host.gpui_plan_operations > 0
            && host.submission_boundary_ready,
        lifecycle_complete
            && !host.renderer_submission_performed
            && !host.physical_presentation_measured,
        fanout.complete && fanout.last_source_sequence == host.origin_last_source_sequence,
    ];
    let passed_checks = checks
        .into_iter()
        .enumerate()
        .fold(
            0_u8,
            |bits, (index, passed)| {
                if passed { bits | (1_u8 << index) } else { bits }
            },
        );
    Ok(MarketBarPacketOriginConformance {
        outcomes,
        partition_fanout_last_source_sequence: fanout.last_source_sequence,
        origin_last_source_sequence: host.origin_last_source_sequence,
        origin_primitive_count: host.origin_primitive_count,
        gpui_plan_operations: host.gpui_plan_operations,
        gpui_mesh_vertices: host.gpui_mesh_vertices,
        passed_checks,
    })
}

struct MarketBarPacketFanoutEvidence {
    direct_event: CanonicalMarketEvent,
    last_source_sequence: u64,
    complete: bool,
}

fn run_market_bar_packet_partition_fanout(
    snapshot: &CanonicalSnapshot,
    delta: &CanonicalMarketEvent,
) -> Result<MarketBarPacketFanoutEvidence, ConformanceHarnessError> {
    let owner =
        PartitionOwner::try_new(7, "market_bar_packet_native_owner", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    partition
        .install_snapshot(&fence, snapshot)
        .map_err(realtime_error)?;

    let direct = BoundedEventBranch::try_new(queue_policy("market_bar_packet_direct", 1))
        .map_err(realtime_error)?;
    let durable = BoundedEventBranch::try_new(queue_policy("market_bar_packet_durable_tap", 1))
        .map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence.clone(), direct, durable).map_err(realtime_error)?;

    let enqueue_unix_nanos = delta
        .header()
        .timestamps
        .normalized_unix_nanos
        .saturating_add(100);
    let mut header = delta.header().clone();
    header.timestamps.fanout_enqueue_unix_nanos = Some(enqueue_unix_nanos);
    let delta = CanonicalMarketEvent::try_new(header, delta.payload()).map_err(realtime_error)?;
    let accepted = accept_for_fanout(&mut partition, &fence, delta)?;
    let outcomes = fanout
        .publish(&mut partition, &fence, accepted, enqueue_unix_nanos)
        .map_err(realtime_error)?;
    let observed_at = enqueue_unix_nanos.saturating_add(10);
    let direct_queued = fanout.direct_metrics(observed_at);
    let durable_queued = fanout.durable_metrics(observed_at);
    let (direct, direct_residence) = fanout.pop_direct(observed_at).ok_or_else(|| {
        ConformanceHarnessError::Realtime(
            "native packet direct branch did not expose the accepted delta".to_string(),
        )
    })?;
    let (durable, durable_residence) = fanout.pop_durable(observed_at).ok_or_else(|| {
        ConformanceHarnessError::Realtime(
            "native packet durable-tap branch did not expose the accepted delta".to_string(),
        )
    })?;
    let direct_event = direct.event().clone();
    let identity_preserved = direct_event == durable
        && direct_event.header().source_sequence == 3
        && direct_event.header().partition_id == fence.partition_id()
        && direct_event.header().ownership_epoch == fence.ownership_epoch()
        && direct_event.header().timestamps.fanout_enqueue_unix_nanos == Some(enqueue_unix_nanos)
        && direct_event.header().nic_timestamp_source == Some(NicTimestampSource::SocketSoftware);
    let queues_visible = outcomes.direct == QueueOutcome::Enqueued
        && outcomes.durable == QueueOutcome::Enqueued
        && direct_queued.items == 1
        && direct_queued.bytes > 0
        && direct_queued.overflows == 0
        && !direct_queued.snapshot_required
        && durable_queued.items == 1
        && durable_queued.bytes > 0
        && durable_queued.overflows == 0
        && !durable_queued.snapshot_required;
    let queues_drained = fanout.direct_metrics(observed_at).items == 0
        && fanout.durable_metrics(observed_at).items == 0
        && direct_residence == 10
        && durable_residence == 10;
    let last_source_sequence = direct_event.header().source_sequence;
    Ok(MarketBarPacketFanoutEvidence {
        direct_event,
        last_source_sequence,
        complete: identity_preserved && queues_visible && queues_drained,
    })
}

fn market_bar_packet_frame(
    transport: FixtureTransport,
    item: &axiusflow_application::ProvenancedMarketBar,
) -> Result<FixtureFrame, ConformanceHarnessError> {
    let bar = item.value();
    let canonical_payload =
        try_encode_canonical_market_bar_payload(bar).map_err(market_stream_error)?;
    let transport_bytes = match transport {
        FixtureTransport::Udp => UDP_HEADER_BYTES,
        FixtureTransport::Tcp => TCP_HEADER_BYTES,
    };
    let mut bytes = vec![
        0_u8;
        ETHERNET_HEADER_BYTES
            + IPV4_HEADER_BYTES
            + transport_bytes
            + MARKET_BAR_PROVIDER_PAYLOAD_BYTES
    ];
    bytes[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
    let ip = ETHERNET_HEADER_BYTES;
    bytes[ip] = 0x45;
    bytes[ip + 9] = match transport {
        FixtureTransport::Udp => 17,
        FixtureTransport::Tcp => 6,
    };
    let payload = ip + IPV4_HEADER_BYTES + transport_bytes;
    bytes[payload..payload + 4].copy_from_slice(MARKET_BAR_PROVIDER_MAGIC);
    bytes[payload + 4..payload + 12].copy_from_slice(&bar.source_sequence.to_be_bytes());
    bytes[payload + 12..payload + 20].copy_from_slice(
        &item
            .provenance()
            .exchange_timestamp_unix_nanos
            .to_be_bytes(),
    );
    bytes[payload + 20..payload + MARKET_BAR_PROVIDER_PAYLOAD_BYTES]
        .copy_from_slice(&canonical_payload);
    Ok(FixtureFrame {
        bytes,
        metadata: ReceiveMetadata {
            receive_timestamp_unix_nanos: item.provenance().axiusflow_receive_timestamp_unix_nanos,
            timestamp_source: TimestampSource::SocketSoftware,
            queue_id: 0,
        },
    })
}

struct FixtureMarketBarProviderDecoder<'a> {
    source: &'a axiusflow_application::ReplaySnapshot,
    last_sequence: Option<u64>,
    snapshot_required: bool,
}

impl<'a> FixtureMarketBarProviderDecoder<'a> {
    const fn new(source: &'a axiusflow_application::ReplaySnapshot) -> Self {
        Self {
            source,
            last_sequence: None,
            snapshot_required: false,
        }
    }

    fn decode(&mut self, packet: &[u8], metadata: ReceiveMetadata) -> ConformanceOutcome {
        let decoded = match decode_market_bar_provider_packet(packet) {
            Ok(decoded) => decoded,
            Err(error) => return ConformanceOutcome::Malformed(error),
        };
        if self.snapshot_required {
            return ConformanceOutcome::SnapshotRequired {
                sequence: Some(decoded.sequence),
            };
        }
        if let Some(last) = self.last_sequence {
            if decoded.sequence <= last {
                return ConformanceOutcome::Duplicate {
                    sequence: decoded.sequence,
                };
            }
            let Some(expected) = last.checked_add(1) else {
                self.snapshot_required = true;
                return ConformanceOutcome::SnapshotRequired {
                    sequence: Some(decoded.sequence),
                };
            };
            if decoded.sequence != expected {
                self.snapshot_required = true;
                return ConformanceOutcome::Gap {
                    expected,
                    actual: decoded.sequence,
                };
            }
        }
        let entitlement_revision = self.source.bars().first().map_or_else(String::new, |item| {
            item.provenance().entitlement_revision.clone()
        });
        let timestamps = CanonicalTimestamps {
            exchange_unix_nanos: decoded.exchange_timestamp_unix_nanos,
            provider_receive_unix_nanos: metadata.receive_timestamp_unix_nanos.saturating_sub(200),
            nic_receive_unix_nanos: Some(metadata.receive_timestamp_unix_nanos),
            axiusflow_receive_unix_nanos: metadata.receive_timestamp_unix_nanos,
            normalized_unix_nanos: metadata.receive_timestamp_unix_nanos.saturating_add(100),
            fanout_enqueue_unix_nanos: None,
        };
        let header = CanonicalEventHeader {
            event_id: format!("fixture_market_bar_packet_{}", decoded.sequence),
            event_time_unix_nanos: decoded.exchange_timestamp_unix_nanos,
            publication_time_unix_nanos: metadata.receive_timestamp_unix_nanos.saturating_add(200),
            producer: "fixture_market_bar_packet_decoder".to_string(),
            correlation_id: "fixture_market_bar_packet_corpus".to_string(),
            causation_id: format!("embedded_market_bar_{}", decoded.sequence),
            entitlement_revision,
            instrument_id: self.source.instrument().instrument_id.as_str().to_string(),
            venue_id: self.source.instrument().venue_id.clone(),
            source_id: "fixture_market_bar_provider".to_string(),
            series_identity: Some(CanonicalSeriesIdentity {
                instrument_revision: self.source.instrument().revision,
                definition_id: self.source.bar_definition().definition_id.clone(),
                definition_version: self.source.bar_definition().version,
                interval_seconds: self.source.bar_definition().interval_seconds,
            }),
            source_sequence: decoded.sequence,
            partition_id: 7,
            ownership_epoch: 1,
            timestamps,
            nic_timestamp_source: Some(NicTimestampSource::SocketSoftware),
            correction_flags: 0,
            quality_flags: 0,
            schema_version: 1,
            semantic_class: SemanticClass::OrderedDelta,
        };
        let Ok(event) = CanonicalMarketEvent::try_new(header, &decoded.canonical_payload) else {
            return ConformanceOutcome::Malformed(FixtureDecodeError::CanonicalEvent);
        };
        self.last_sequence = Some(decoded.sequence);
        ConformanceOutcome::Accepted(Box::new(event))
    }
}

struct DecodedMarketBarProviderMessage {
    sequence: u64,
    exchange_timestamp_unix_nanos: i64,
    canonical_payload: [u8; axiusflow_market_protocol_adapter::CANONICAL_MARKET_BAR_PAYLOAD_BYTES],
}

fn decode_market_bar_provider_packet(
    packet: &[u8],
) -> Result<DecodedMarketBarProviderMessage, FixtureDecodeError> {
    if packet.len() < ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES {
        return Err(FixtureDecodeError::TruncatedEthernetOrIp);
    }
    if packet[12..14] != 0x0800_u16.to_be_bytes() {
        return Err(FixtureDecodeError::UnsupportedEtherType);
    }
    let ip = ETHERNET_HEADER_BYTES;
    if packet[ip] >> 4 != 4 || packet[ip] & 0x0f != 5 {
        return Err(FixtureDecodeError::UnsupportedIpv4Header);
    }
    let transport_bytes = match packet[ip + 9] {
        17 => UDP_HEADER_BYTES,
        6 => TCP_HEADER_BYTES,
        protocol => return Err(FixtureDecodeError::UnsupportedIpProtocol(protocol)),
    };
    let payload = ip + IPV4_HEADER_BYTES + transport_bytes;
    if packet.len() != payload + MARKET_BAR_PROVIDER_PAYLOAD_BYTES {
        return Err(FixtureDecodeError::InvalidProviderLength(packet.len()));
    }
    if &packet[payload..payload + 4] != MARKET_BAR_PROVIDER_MAGIC {
        return Err(FixtureDecodeError::InvalidProviderMagic);
    }
    let sequence = u64::from_be_bytes(
        packet[payload + 4..payload + 12]
            .try_into()
            .expect("validated market-bar provider sequence width"),
    );
    if sequence == 0 {
        return Err(FixtureDecodeError::ZeroSequence);
    }
    let exchange_timestamp_unix_nanos = i64::from_be_bytes(
        packet[payload + 12..payload + 20]
            .try_into()
            .expect("validated market-bar provider timestamp width"),
    );
    let canonical_payload = packet[payload + 20..payload + MARKET_BAR_PROVIDER_PAYLOAD_BYTES]
        .try_into()
        .expect("validated canonical market-bar payload width");
    Ok(DecodedMarketBarProviderMessage {
        sequence,
        exchange_timestamp_unix_nanos,
        canonical_payload,
    })
}

fn market_bar_packet_outcome_shape(outcomes: &[ConformanceOutcome]) -> bool {
    outcomes.len() == 7
        && matches!(
            &outcomes[0],
            ConformanceOutcome::Accepted(event) if event.header().source_sequence == 1
        )
        && matches!(
            &outcomes[1],
            ConformanceOutcome::Accepted(event) if event.header().source_sequence == 2
        )
        && matches!(
            &outcomes[2],
            ConformanceOutcome::Accepted(event) if event.header().source_sequence == 3
        )
        && outcomes[3] == (ConformanceOutcome::Duplicate { sequence: 3 })
        && outcomes[4]
            == (ConformanceOutcome::Gap {
                expected: 4,
                actual: 5,
            })
        && outcomes[5] == (ConformanceOutcome::SnapshotRequired { sequence: Some(4) })
        && outcomes[6] == ConformanceOutcome::Malformed(FixtureDecodeError::TruncatedEthernetOrIp)
}
