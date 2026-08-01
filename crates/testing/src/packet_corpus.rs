//! Deterministic Ethernet/IPv4/UDP/TCP provider packet corpus and its stateful decoder.
//!
//! The corpus owns wire framing and the fixture provider payload layout. The decoder
//! turns those packets into canonical events without retaining borrowed packet memory,
//! so sequence, duplicate, gap, and malformed semantics stay in one discoverable home.

use crate::harness_error::FixtureDecodeError;
use axiusflow_realtime::{
    CanonicalEventHeader, CanonicalMarketEvent, CanonicalTimestamps, NicTimestampSource,
    SemanticClass,
};
use axiusflow_transport::{FixtureFrame, ReceiveMetadata, TimestampSource};

pub(crate) const ETHERNET_HEADER_BYTES: usize = 14;
pub(crate) const IPV4_HEADER_BYTES: usize = 20;
pub(crate) const UDP_HEADER_BYTES: usize = 8;
pub(crate) const TCP_HEADER_BYTES: usize = 20;
const PROVIDER_PAYLOAD_BYTES: usize = 28;
const PROVIDER_MAGIC: &[u8; 4] = b"AXF1";

/// Packet transport represented in the deterministic corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureTransport {
    Udp,
    Tcp,
}

/// Semantic result produced by the stateful fixture decoder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConformanceOutcome {
    Accepted(Box<CanonicalMarketEvent>),
    Duplicate { sequence: u64 },
    Gap { expected: u64, actual: u64 },
    SnapshotRequired { sequence: Option<u64> },
    Malformed(FixtureDecodeError),
}

/// Deterministic packet/message corpus covering Ethernet, IPv4, UDP, TCP, gaps, and errors.
#[must_use]
pub fn deterministic_ingest_corpus() -> Vec<FixtureFrame> {
    vec![
        frame(packet(FixtureTransport::Udp, 1, 1_000, 18_400), 2_000),
        frame(packet(FixtureTransport::Tcp, 2, 2_000, 18_410), 3_000),
        frame(packet(FixtureTransport::Udp, 2, 2_000, 18_410), 3_100),
        frame(packet(FixtureTransport::Udp, 4, 4_000, 18_430), 5_000),
        frame(packet(FixtureTransport::Tcp, 3, 3_000, 18_420), 5_100),
        frame(vec![0_u8; 12], 6_000),
    ]
}

fn frame(bytes: Vec<u8>, receive_timestamp_unix_nanos: i64) -> FixtureFrame {
    FixtureFrame {
        bytes,
        metadata: ReceiveMetadata {
            receive_timestamp_unix_nanos,
            timestamp_source: TimestampSource::SocketSoftware,
            queue_id: 0,
        },
    }
}

fn packet(transport: FixtureTransport, sequence: u64, exchange: i64, price: i64) -> Vec<u8> {
    let transport_bytes = match transport {
        FixtureTransport::Udp => UDP_HEADER_BYTES,
        FixtureTransport::Tcp => TCP_HEADER_BYTES,
    };
    let mut bytes =
        vec![
            0_u8;
            ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + transport_bytes + PROVIDER_PAYLOAD_BYTES
        ];
    bytes[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
    let ip = ETHERNET_HEADER_BYTES;
    bytes[ip] = 0x45;
    bytes[ip + 9] = match transport {
        FixtureTransport::Udp => 17,
        FixtureTransport::Tcp => 6,
    };
    let payload = ip + IPV4_HEADER_BYTES + transport_bytes;
    bytes[payload..payload + 4].copy_from_slice(PROVIDER_MAGIC);
    bytes[payload + 4..payload + 12].copy_from_slice(&sequence.to_be_bytes());
    bytes[payload + 12..payload + 20].copy_from_slice(&exchange.to_be_bytes());
    bytes[payload + 20..payload + 28].copy_from_slice(&price.to_be_bytes());
    bytes
}

/// Stateful provider decoder proving gap behavior without retaining borrowed packet memory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FixtureProviderDecoder {
    last_sequence: Option<u64>,
    snapshot_required: bool,
}

impl FixtureProviderDecoder {
    #[must_use]
    pub fn decode(&mut self, packet: &[u8], metadata: ReceiveMetadata) -> ConformanceOutcome {
        let decoded = match decode_provider_packet(packet) {
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

        let timestamps = CanonicalTimestamps {
            exchange_unix_nanos: decoded.exchange_timestamp_unix_nanos,
            provider_receive_unix_nanos: metadata.receive_timestamp_unix_nanos.saturating_sub(200),
            nic_receive_unix_nanos: Some(metadata.receive_timestamp_unix_nanos),
            axiusflow_receive_unix_nanos: metadata.receive_timestamp_unix_nanos,
            normalized_unix_nanos: metadata.receive_timestamp_unix_nanos.saturating_add(100),
            fanout_enqueue_unix_nanos: None,
        };
        let header = CanonicalEventHeader {
            event_id: format!("fixture_event_{}", decoded.sequence),
            event_time_unix_nanos: decoded.exchange_timestamp_unix_nanos,
            publication_time_unix_nanos: metadata.receive_timestamp_unix_nanos.saturating_add(200),
            producer: "fixture_provider_decoder".to_string(),
            correlation_id: "fixture_corpus".to_string(),
            causation_id: String::new(),
            entitlement_revision: "fixture_entitlement_v1".to_string(),
            instrument_id: "instrument_fixture_axf_xnas".to_string(),
            venue_id: "xnas".to_string(),
            source_id: "fixture_provider".to_string(),
            series_identity: None,
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
        let Ok(event) =
            CanonicalMarketEvent::try_new(header, &decoded.price_mantissa.to_be_bytes())
        else {
            return ConformanceOutcome::Malformed(FixtureDecodeError::CanonicalEvent);
        };
        self.last_sequence = Some(decoded.sequence);
        ConformanceOutcome::Accepted(Box::new(event))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DecodedProviderMessage {
    sequence: u64,
    exchange_timestamp_unix_nanos: i64,
    price_mantissa: i64,
}

fn decode_provider_packet(packet: &[u8]) -> Result<DecodedProviderMessage, FixtureDecodeError> {
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
    if packet.len() != payload + PROVIDER_PAYLOAD_BYTES {
        return Err(FixtureDecodeError::InvalidProviderLength(packet.len()));
    }
    if &packet[payload..payload + 4] != PROVIDER_MAGIC {
        return Err(FixtureDecodeError::InvalidProviderMagic);
    }
    let sequence = u64::from_be_bytes(
        packet[payload + 4..payload + 12]
            .try_into()
            .expect("validated provider sequence width"),
    );
    if sequence == 0 {
        return Err(FixtureDecodeError::ZeroSequence);
    }
    Ok(DecodedProviderMessage {
        sequence,
        exchange_timestamp_unix_nanos: i64::from_be_bytes(
            packet[payload + 12..payload + 20]
                .try_into()
                .expect("validated provider timestamp width"),
        ),
        price_mantissa: i64::from_be_bytes(
            packet[payload + 20..payload + 28]
                .try_into()
                .expect("validated provider price width"),
        ),
    })
}
