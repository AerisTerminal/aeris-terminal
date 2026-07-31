//! Deterministic packet corpus and semantic-equivalence harness for Stage 1 ingest profiles.

use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketBarReplayPort, MarketStreamCommand, MarketStreamCommandOffer, MarketStreamEvent,
    MarketStreamPublication, MarketStreamPublicationError, MarketStreamRuntimePort,
    ReplayProvenance, ReplayStreamUpdate, ResnapshotReason,
};
use axiusflow_chart_integration::{
    ChartDataBridge, ChartStreamCoordinator, ChartStreamPollOutcome, ChartStreamRecoveryDispatch,
    ReplayRecoveryCommand, run_origin_gpui_host_sample,
};
use axiusflow_market_protocol_adapter::{
    BinaryMarketBarStreamDecoder, CanonicalMarketBarProjectionError, DecimalConvention,
    ProjectedMarketBarUpdate, encode_market_bar_stream_frame,
    try_encode_canonical_market_bar_delta_envelope, try_encode_canonical_market_bar_payload,
    try_encode_replay_delta_envelope, try_encode_replay_snapshot_chunk_envelopes,
    try_encode_replay_snapshot_envelope, try_project_canonical_market_bar,
    try_project_canonical_market_bar_snapshot,
};
use axiusflow_protocols::protobuf::axiusflow::market::v1::market_bar_stream_envelope;
use axiusflow_realtime::{
    AcceptedCanonicalEvent, AcceptedDirectEvent, BoundedEventBranch, CanonicalEventHeader,
    CanonicalLatestState, CanonicalMarketEvent, CanonicalSeriesIdentity, CanonicalSnapshot,
    CanonicalTimestamps, DirectDurableFanout, FanoutOutcome, FencedPartition,
    LatestStateSnapshotRequest, NicTimestampSource, OverflowAction, PartitionAcceptance,
    PartitionDecision, PartitionOwner, QueueOutcome, QueuePolicy, RealtimeError, SemanticClass,
};
use axiusflow_stream_websocket_adapter::{
    MarketWebSocketConfig, MarketWebSocketError, MarketWebSocketPublication,
    MarketWebSocketSession, MarketWebSocketState, PlainLoopbackBackgroundRuntime,
    PlainLoopbackCommand, PlainLoopbackLifecycleConfig, PlainLoopbackLifecycleError,
    PlainLoopbackLifecycleEvent, PlainLoopbackLifecycleState, PlainLoopbackLifecycleStep,
    PlainLoopbackMarketWebSocketOwner, PlainLoopbackRuntimeCommand, PlainLoopbackRuntimeConfig,
    PlainLoopbackRuntimeConfigError, PlainLoopbackRuntimeEvent, PlainLoopbackRuntimeStartError,
    PlainLoopbackWebSocketEndpoint, WebSocketMessageOutcome, WebSocketRecoveryReason,
};
use axiusflow_transport::{
    FixtureFrame, IngestDriver, QueueBinding, ReceiveBatch, ReceiveMetadata, TimestampSource,
    encode_binary_frame,
};
use core::fmt;
use std::{
    error::Error,
    net::TcpListener,
    num::{NonZeroU64, NonZeroUsize},
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, accept_with_config, client::connect_with_config};

const ETHERNET_HEADER_BYTES: usize = 14;
const IPV4_HEADER_BYTES: usize = 20;
const UDP_HEADER_BYTES: usize = 8;
const TCP_HEADER_BYTES: usize = 20;
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

/// Runs the same bounded receive/release, startup/shutdown, and decoder lifecycle for any
/// profile adapter.
///
/// # Errors
///
/// Returns an error when queue binding, startup, receive, or shutdown fails.
pub fn run_ingest_conformance<Driver>(
    driver: &mut Driver,
) -> Result<Vec<ConformanceOutcome>, ConformanceHarnessError>
where
    Driver: IngestDriver,
{
    run_ingest_conformance_after_start(driver, |_| Ok(()))
}

/// Runs ingest conformance after allowing a caller to seed a started native adapter.
///
/// The callback is intended for deterministic loopback senders. It executes only after
/// queue binding and startup, and before the first receive.
///
/// # Errors
///
/// Returns an error from lifecycle operations, the startup callback, or receive.
pub fn run_ingest_conformance_after_start<Driver>(
    driver: &mut Driver,
    after_start: impl FnOnce(&mut Driver) -> Result<(), ConformanceHarnessError>,
) -> Result<Vec<ConformanceOutcome>, ConformanceHarnessError>
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
    after_start(driver)?;

    let mut outcomes = Vec::new();
    let mut decoder = FixtureProviderDecoder::default();
    loop {
        let batch = driver
            .receive_batch()
            .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        let count = batch.frame_count();
        for index in 0..count {
            let Some(frame) = batch.frame(index) else {
                return Err(ConformanceHarnessError::MissingFrame(index));
            };
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
    Ok(outcomes)
}

/// Compares adapter outcomes while excluding wall-clock receive values that cannot be
/// identical across independently executed native adapters.
///
/// Exchange timestamps, canonical identity, ordering, provenance, payloads, and every
/// non-accepted decision remain exact-match requirements.
#[must_use]
pub fn ingest_outcomes_semantically_equivalent(
    left: &[ConformanceOutcome],
    right: &[ConformanceOutcome],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (ConformanceOutcome::Accepted(left), ConformanceOutcome::Accepted(right)) => {
                    let left_header = left.header();
                    let right_header = right.header();
                    left.payload() == right.payload()
                        && left_header.event_id == right_header.event_id
                        && left_header.event_time_unix_nanos == right_header.event_time_unix_nanos
                        && left_header.producer == right_header.producer
                        && left_header.correlation_id == right_header.correlation_id
                        && left_header.causation_id == right_header.causation_id
                        && left_header.entitlement_revision == right_header.entitlement_revision
                        && left_header.instrument_id == right_header.instrument_id
                        && left_header.venue_id == right_header.venue_id
                        && left_header.source_id == right_header.source_id
                        && left_header.series_identity == right_header.series_identity
                        && left_header.source_sequence == right_header.source_sequence
                        && left_header.partition_id == right_header.partition_id
                        && left_header.ownership_epoch == right_header.ownership_epoch
                        && left_header.timestamps.exchange_unix_nanos
                            == right_header.timestamps.exchange_unix_nanos
                        && left_header.nic_timestamp_source == right_header.nic_timestamp_source
                        && left_header.correction_flags == right_header.correction_flags
                        && left_header.quality_flags == right_header.quality_flags
                        && left_header.schema_version == right_header.schema_version
                        && left_header.semantic_class == right_header.semantic_class
                }
                _ => left == right,
            })
}

/// Headless evidence for the fixture-qualified binary market subscription pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BinaryMarketStreamConformance {
    passed_checks: u8,
}

impl BinaryMarketStreamConformance {
    const REQUIRED_CHECKS: u8 = 0b0111_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises bounded framing, Protobuf projection, reconnect, corruption, and gap behavior.
///
/// This proves a deterministic software stream contract only. It does not prove a
/// live WebSocket, provider connection, entitlement service, or production readiness.
///
/// # Errors
///
/// Returns an error when the embedded fixture or binary pipeline rejects a valid scenario.
pub fn run_binary_market_stream_conformance()
-> Result<BinaryMarketStreamConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let (fragmented_equivalent, coalesced_equivalent) = framing_checks(&fixture)?;
    let (malformed_rejected, reconnect_restored) = malformed_and_reconnect_checks(&fixture)?;
    let checksum_corruption_rejected = checksum_corruption_check(&fixture)?;
    let gap_rejected = gap_check(&fixture)?;
    let schema_change_rejected = decoder_schema_change_preserves_generation(&fixture)?;
    let oversized_rejected = oversized_frame_check(&fixture)?;
    let passed_checks = passed_check_bits([
        fragmented_equivalent,
        coalesced_equivalent,
        malformed_rejected,
        reconnect_restored,
        checksum_corruption_rejected,
        gap_rejected && schema_change_rejected,
        oversized_rejected,
    ]);
    Ok(BinaryMarketStreamConformance { passed_checks })
}

fn decoder_schema_change_preserves_generation(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut session = websocket_fixture_session(fixture, websocket_fixture_config(fixture, 4)?)?;
    session
        .accept_message(Message::binary(fixture.snapshot_frame.clone()))
        .map_err(websocket_error)?;
    let installed = session.current_generation().cloned();
    let mut provenance = fixture.delta.item().provenance().clone();
    provenance.schema_version = provenance.schema_version.saturating_add(1);
    let changed_item =
        axiusflow_application::Provenanced::new(*fixture.delta.item().value(), provenance);
    let changed_delta = axiusflow_application::StreamDelta::try_new(
        fixture.delta.previous_sequence(),
        fixture.delta.sequence(),
        changed_item,
    )
    .map_err(market_stream_error)?;
    let envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.baseline.instrument(),
        fixture.baseline.bar_definition(),
        &changed_delta,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    Ok(session.accept_message(Message::binary(frame)).is_err()
        && session.current_generation() == installed.as_ref()
        && session.state()
            == MarketWebSocketState::RecoveryRequired(WebSocketRecoveryReason::DecodeFailure))
}

/// Headless evidence for one real plain-loopback WebSocket upgrade and bounded session semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebSocketLoopbackConformance {
    passed_checks: u16,
}

impl WebSocketLoopbackConformance {
    const REQUIRED_CHECKS: u16 = 0x03ff;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises a real `ws://127.0.0.1` upgrade plus bounded binary lifecycle recovery.
///
/// This is deterministic local software evidence only. It does not prove TLS,
/// certificate handling, authentication, entitlements, WAN behavior, provider
/// connectivity, evented production operation, or production capacity.
///
/// # Errors
///
/// Returns an error when the loopback socket or a valid fixture scenario fails.
pub fn run_websocket_loopback_conformance()
-> Result<WebSocketLoopbackConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let delta_frame = fixture.joined_frames[fixture.snapshot_frame.len()..].to_vec();
    let split = fixture.snapshot_frame.len() / 2;
    let ping_payload = vec![0x41, 0x58, 0x46];
    let inbound_pong_payload = vec![0x50, 0x4f, 0x4e, 0x47];
    let fragmented = run_loopback_delivery(
        &fixture,
        vec![
            Message::binary(fixture.snapshot_frame[..split].to_vec()),
            Message::Ping(ping_payload.clone().into()),
            Message::Pong(inbound_pong_payload.into()),
            Message::binary(fixture.snapshot_frame[split..].to_vec()),
            Message::binary(delta_frame.clone()),
        ],
        Some(ping_payload),
    )
    .map_err(|error| websocket_error(format!("fragmented loopback: {error}")))?;
    let fragmented_equivalent = projected_websocket_updates_match(
        &fragmented.publications,
        &fixture.baseline,
        &fixture.delta,
    );

    let coalesced = run_loopback_delivery(
        &fixture,
        vec![Message::binary(fixture.joined_frames.clone())],
        None,
    )
    .map_err(|error| websocket_error(format!("coalesced loopback: {error}")))?;
    let coalesced_equivalent = projected_websocket_updates_match(
        &coalesced.publications,
        &fixture.baseline,
        &fixture.delta,
    );
    let network_text_rejected = run_loopback_fault(
        &fixture,
        Message::text("not-binary"),
        WebSocketRecoveryReason::TextMessage,
    )
    .map_err(|error| websocket_error(format!("text fault loopback: {error}")))?;
    let network_close_rejected = run_loopback_fault(
        &fixture,
        Message::Close(None),
        WebSocketRecoveryReason::PeerClosed,
    )
    .map_err(|error| websocket_error(format!("close fault loopback: {error}")))?;
    let (
        bounded_faults_latched,
        transport_fault_latched,
        reconnect_snapshot_required,
        reconnect_recovered,
    ) = websocket_fault_and_reconnect_checks(&fixture, &delta_frame)?;
    let coalesced_overflow_rolled_back = websocket_queue_overflow_check(&fixture)?;

    let checks = [
        fragmented.upgrade_succeeded && coalesced.upgrade_succeeded,
        fragmented_equivalent,
        coalesced_equivalent,
        fragmented.control_observations & LOOPBACK_PING_REPLY == LOOPBACK_PING_REPLY,
        fragmented.control_observations & LOOPBACK_INBOUND_PONG != 0,
        network_text_rejected && network_close_rejected,
        bounded_faults_latched && transport_fault_latched,
        reconnect_snapshot_required,
        reconnect_recovered,
        coalesced_overflow_rolled_back,
    ];
    let passed_checks = checks
        .into_iter()
        .enumerate()
        .fold(0_u16, |bits, (index, passed)| {
            if passed {
                bits | (1_u16 << index)
            } else {
                bits
            }
        });
    Ok(WebSocketLoopbackConformance { passed_checks })
}

const LOOPBACK_PING_OBSERVED: u8 = 0b001;
const LOOPBACK_INBOUND_PONG: u8 = 0b010;
const LOOPBACK_CONTROL_REPLY: u8 = 0b100;
const LOOPBACK_PING_REPLY: u8 = LOOPBACK_PING_OBSERVED | LOOPBACK_CONTROL_REPLY;

struct LoopbackDelivery {
    upgrade_succeeded: bool,
    control_observations: u8,
    publications: Vec<MarketWebSocketPublication>,
}

fn run_loopback_delivery(
    fixture: &BinaryMarketStreamFixture,
    messages: Vec<Message>,
    expected_pong: Option<Vec<u8>>,
) -> Result<LoopbackDelivery, ConformanceHarnessError> {
    let config = websocket_fixture_config(fixture, 8)?;
    let socket_config = config.websocket_config();
    let listener = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let address = listener.local_addr().map_err(websocket_error)?;
    let message_count = messages.len();
    let server = thread::Builder::new()
        .name("axiusflow-websocket-loopback".to_string())
        .spawn(move || -> Result<bool, String> {
            let stream = accept_runtime_connection(&listener, Duration::from_secs(3))?;
            configure_runtime_server_stream(&stream)?;
            let mut socket = accept_with_config(stream, Some(socket_config))
                .map_err(|error| error.to_string())?;
            for message in messages {
                socket.send(message).map_err(|error| error.to_string())?;
            }
            let peer_confirmed_control_reply = if let Some(expected) = expected_pong {
                match socket.read().map_err(|error| error.to_string())? {
                    Message::Pong(actual) => actual.as_ref() == expected.as_slice(),
                    _ => false,
                }
            } else {
                true
            };
            socket.close(None).map_err(|error| error.to_string())?;
            Ok(peer_confirmed_control_reply)
        })
        .map_err(websocket_error)?;

    let url = format!("ws://{address}/market-bars");
    let (mut socket, response) =
        connect_with_config(url, Some(socket_config), 0).map_err(websocket_error)?;
    let mut session = websocket_fixture_session(fixture, config)?;
    let mut control_observations = 0_u8;
    for _ in 0..message_count {
        let message = socket.read().map_err(websocket_error)?;
        let outcome = session.accept_message(message).map_err(websocket_error)?;
        match outcome {
            WebSocketMessageOutcome::Ping => {
                control_observations |= LOOPBACK_PING_OBSERVED;
                socket.flush().map_err(websocket_error)?;
            }
            WebSocketMessageOutcome::Pong => {
                control_observations |= LOOPBACK_INBOUND_PONG;
            }
            WebSocketMessageOutcome::BinaryAccepted { .. } => {}
        }
    }
    let publications = session.drain_publications(
        NonZeroUsize::new(config.publication_capacity()).unwrap_or(NonZeroUsize::MIN),
    );
    drop(socket);
    let peer_confirmed_control_reply = server
        .join()
        .map_err(|_| websocket_error("loopback WebSocket server panicked"))?
        .map_err(websocket_error)?;
    if peer_confirmed_control_reply {
        control_observations |= LOOPBACK_CONTROL_REPLY;
    }
    Ok(LoopbackDelivery {
        upgrade_succeeded: response.status().as_u16() == 101,
        control_observations,
        publications,
    })
}

fn run_loopback_fault(
    fixture: &BinaryMarketStreamFixture,
    message: Message,
    expected_reason: WebSocketRecoveryReason,
) -> Result<bool, ConformanceHarnessError> {
    let config = websocket_fixture_config(fixture, 4)?;
    let socket_config = config.websocket_config();
    let listener = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let address = listener.local_addr().map_err(websocket_error)?;
    let server = thread::Builder::new()
        .name("axiusflow-websocket-fault-loopback".to_string())
        .spawn(move || -> Result<(), String> {
            let stream = accept_runtime_connection(&listener, Duration::from_secs(3))?;
            configure_runtime_server_stream(&stream)?;
            let mut socket = accept_with_config(stream, Some(socket_config))
                .map_err(|error| error.to_string())?;
            socket.send(message).map_err(|error| error.to_string())
        })
        .map_err(websocket_error)?;

    let url = format!("ws://{address}/market-bars");
    let (mut socket, response) =
        connect_with_config(url, Some(socket_config), 0).map_err(websocket_error)?;
    let mut session = websocket_fixture_session(fixture, config)?;
    let message = socket.read().map_err(websocket_error)?;
    let result = session.accept_message(message);
    drop(socket);
    server
        .join()
        .map_err(|_| websocket_error("fault loopback WebSocket server panicked"))?
        .map_err(websocket_error)?;
    let expected_error = match expected_reason {
        WebSocketRecoveryReason::TextMessage => {
            matches!(result, Err(MarketWebSocketError::TextMessageRejected))
        }
        WebSocketRecoveryReason::PeerClosed => {
            matches!(result, Err(MarketWebSocketError::PeerClosed))
        }
        _ => false,
    };
    Ok(response.status().as_u16() == 101
        && expected_error
        && session.state() == MarketWebSocketState::RecoveryRequired(expected_reason))
}

fn websocket_fault_and_reconnect_checks(
    fixture: &BinaryMarketStreamFixture,
    delta_frame: &[u8],
) -> Result<(bool, bool, bool, bool), ConformanceHarnessError> {
    let config = websocket_fixture_config(fixture, 4)?;
    let small_message_bound = NonZeroUsize::new(fixture.snapshot_frame.len().saturating_sub(1))
        .unwrap_or(NonZeroUsize::MIN);
    let small_config = MarketWebSocketConfig::try_new(
        small_message_bound,
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
        NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(websocket_error)?;
    let mut oversized_session = websocket_fixture_session(fixture, small_config)?;
    let oversized_rejected = matches!(
        oversized_session.accept_message(Message::binary(fixture.snapshot_frame.clone())),
        Err(MarketWebSocketError::MessageLimitExceeded { .. })
    ) && oversized_session.state()
        == MarketWebSocketState::RecoveryRequired(WebSocketRecoveryReason::MessageTooLarge);

    let mut transport_session = websocket_fixture_session(fixture, config)?;
    transport_session.notify_transport_failure();
    let transport_fault_latched = transport_session.state()
        == MarketWebSocketState::RecoveryRequired(WebSocketRecoveryReason::TransportFailure)
        && matches!(
            transport_session.accept_message(Message::binary(fixture.snapshot_frame.clone())),
            Err(MarketWebSocketError::RecoveryRequired(
                WebSocketRecoveryReason::TransportFailure
            ))
        );
    let mut first_cause_session = websocket_fixture_session(fixture, config)?;
    let _ = first_cause_session.accept_message(Message::text("not-binary"));
    first_cause_session.notify_transport_failure();
    let first_cause_preserved = first_cause_session.state()
        == MarketWebSocketState::RecoveryRequired(WebSocketRecoveryReason::TextMessage);

    let mut reconnect_session = websocket_fixture_session(fixture, config)?;
    reconnect_session
        .accept_message(Message::binary(fixture.snapshot_frame.clone()))
        .map_err(websocket_error)?;
    let stable_generation = reconnect_session.current_generation().cloned();
    let _ = reconnect_session.drain_publications(NonZeroUsize::MIN);
    reconnect_session.notify_transport_failure();
    let generation_preserved = reconnect_session.current_generation() == stable_generation.as_ref();
    reconnect_session.begin_reconnect();
    let reconnect_snapshot_required = reconnect_session.state()
        == MarketWebSocketState::AwaitingSnapshot
        && matches!(
            reconnect_session.accept_message(Message::binary(delta_frame.to_vec())),
            Err(MarketWebSocketError::Decoder(_))
        )
        && reconnect_session.current_generation() == stable_generation.as_ref();
    reconnect_session.begin_reconnect();
    let reconnect_snapshot = replay_snapshot_with_generation(&fixture.baseline, 2)?;
    let reconnect_envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &reconnect_snapshot,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let reconnect_frame =
        encode_market_bar_stream_frame(&reconnect_envelope, fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
    let reconnect_recovered = reconnect_session
        .accept_message(Message::binary(reconnect_frame))
        .is_ok_and(|outcome| outcome == WebSocketMessageOutcome::BinaryAccepted { published: 1 })
        && reconnect_session.state() == MarketWebSocketState::Streaming
        && reconnect_session.current_generation().is_some();

    Ok((
        oversized_rejected,
        transport_fault_latched && first_cause_preserved && generation_preserved,
        reconnect_snapshot_required,
        reconnect_recovered,
    ))
}

fn websocket_queue_overflow_check(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let config = websocket_fixture_config(fixture, 1)?;
    let mut session = websocket_fixture_session(fixture, config)?;
    session
        .accept_message(Message::binary(fixture.snapshot_frame.clone()))
        .map_err(websocket_error)?;
    let stable_generation = session.current_generation().cloned();
    let _ = session.drain_publications(NonZeroUsize::MIN);
    let replacement = replay_snapshot_with_generation(&fixture.baseline, 2)?;
    let replacement_envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &replacement,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let mut replacement_and_delta =
        encode_market_bar_stream_frame(&replacement_envelope, fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
    replacement_and_delta.extend_from_slice(&fixture.joined_frames[fixture.snapshot_frame.len()..]);
    let overflow_rejected = matches!(
        session.accept_message(Message::binary(replacement_and_delta)),
        Err(MarketWebSocketError::PublicationQueueOverflow {
            requested: 2,
            maximum: 1
        })
    );
    Ok(overflow_rejected
        && session.state()
            == MarketWebSocketState::RecoveryRequired(
                WebSocketRecoveryReason::PublicationQueueOverflow,
            )
        && session.queued_publications() == 0
        && session.current_generation() == stable_generation.as_ref())
}

fn websocket_fixture_config(
    fixture: &BinaryMarketStreamFixture,
    maximum_publications: usize,
) -> Result<MarketWebSocketConfig, ConformanceHarnessError> {
    MarketWebSocketConfig::try_new(
        fixture.maximum_buffered_bytes,
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
        NonZeroUsize::new(maximum_publications).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(websocket_error)
}

fn websocket_fixture_session(
    fixture: &BinaryMarketStreamFixture,
    config: MarketWebSocketConfig,
) -> Result<MarketWebSocketSession, ConformanceHarnessError> {
    MarketWebSocketSession::try_new(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.convention.clone(),
        ReplayProvenance::EmbeddedFixture,
        config,
    )
    .map_err(websocket_error)
}

fn projected_websocket_updates_match(
    publications: &[MarketWebSocketPublication],
    baseline: &axiusflow_application::ReplaySnapshot,
    delta: &axiusflow_protocols::StreamDelta<axiusflow_application::ProvenancedMarketBar>,
) -> bool {
    publications.len() == 2
        && publications
            .iter()
            .all(|publication| publication.subscription_id == BINARY_FIXTURE_SUBSCRIPTION_ID)
        && matches!(
            &publications[0].update,
            ReplayStreamUpdate::Snapshot(snapshot) if snapshot == baseline
        )
        && matches!(
            &publications[1].update,
            ReplayStreamUpdate::Delta(actual) if actual == delta
        )
}

fn websocket_error(error: impl fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::WebSocket(error.to_string())
}

/// Headless evidence for adapter-owned plain-loopback connection lifecycle semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainLoopbackLifecycleConformance {
    passed_checks: u16,
}

impl PlainLoopbackLifecycleConformance {
    const REQUIRED_CHECKS: u16 = 0x1fff;

    #[must_use]
    pub const fn passed_checks(self) -> u16 {
        self.passed_checks
    }

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises bounded command/event ownership, reconnect, exhaustion, and shutdown.
///
/// This proves one blocking owner over local plain WebSocket sockets only. It does not
/// prove TLS, authentication, entitlement enforcement, provider connectivity, WAN
/// behavior, an evented production runtime, desktop integration, or production capacity.
///
/// # Errors
///
/// Returns an error when a valid loopback lifecycle transition fails.
pub fn run_plain_loopback_lifecycle_conformance()
-> Result<PlainLoopbackLifecycleConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let endpoint_restrictions = loopback_endpoint_restrictions(&fixture);
    let lifecycle = run_multi_connection_lifecycle(&fixture)
        .map_err(|error| websocket_error(format!("multi-connection lifecycle: {error}")))?;
    let event_overflow_recovered = run_lifecycle_event_overflow(&fixture)
        .map_err(|error| websocket_error(format!("lifecycle event overflow: {error}")))?;
    let reconnect = run_lifecycle_reconnect_exhaustion(&fixture)
        .map_err(|error| websocket_error(format!("lifecycle reconnect exhaustion: {error}")))?;
    let silent_peer_timeout = run_lifecycle_silent_peer_timeout(&fixture)
        .map_err(|error| websocket_error(format!("silent peer timeout: {error}")))?;
    let checks = [
        endpoint_restrictions,
        lifecycle[0],
        lifecycle[1],
        lifecycle[2],
        lifecycle[3],
        lifecycle[4],
        lifecycle[5],
        event_overflow_recovered,
        reconnect[0],
        reconnect[1],
        reconnect[2],
        reconnect[3],
        silent_peer_timeout,
    ];
    let passed_checks = checks
        .into_iter()
        .enumerate()
        .fold(0_u16, |bits, (index, passed)| {
            if passed {
                bits | (1_u16 << index)
            } else {
                bits
            }
        });
    Ok(PlainLoopbackLifecycleConformance { passed_checks })
}

fn loopback_endpoint_restrictions(fixture: &BinaryMarketStreamFixture) -> bool {
    let Ok(endpoint) = PlainLoopbackWebSocketEndpoint::try_new("ws://127.0.0.1:1/market") else {
        return false;
    };
    let Ok(session_config) = websocket_fixture_config(fixture, 8) else {
        return false;
    };
    let Ok(session) = websocket_fixture_session(fixture, session_config) else {
        return false;
    };
    let Ok(lifecycle_config) = PlainLoopbackLifecycleConfig::try_new(
        NonZeroUsize::MIN,
        NonZeroUsize::MIN,
        NonZeroUsize::MIN,
        Duration::from_secs(1),
    ) else {
        return false;
    };
    let insufficient_event_capacity = matches!(
        PlainLoopbackMarketWebSocketOwner::try_new(endpoint, lifecycle_config, session),
        Err(
            PlainLoopbackLifecycleError::EventCapacityBelowPublicationCapacity {
                event_capacity: 1,
                publication_capacity: 8
            }
        )
    );
    PlainLoopbackWebSocketEndpoint::try_new("wss://127.0.0.1:443/market").is_err()
        && PlainLoopbackWebSocketEndpoint::try_new("ws://192.0.2.1:8080/market").is_err()
        && PlainLoopbackWebSocketEndpoint::try_new("ws://localhost:8080/market").is_err()
        && PlainLoopbackWebSocketEndpoint::try_new("ws://127.0.0.1/market").is_err()
        && PlainLoopbackLifecycleConfig::try_new(
            NonZeroUsize::MIN,
            NonZeroUsize::MIN,
            NonZeroUsize::MIN,
            Duration::ZERO,
        )
        .is_err()
        && insufficient_event_capacity
}

type LifecycleServerHandle = thread::JoinHandle<Result<bool, String>>;

fn run_multi_connection_lifecycle(
    fixture: &BinaryMarketStreamFixture,
) -> Result<[bool; 6], ConformanceHarnessError> {
    let (endpoint, server) = spawn_multi_connection_server(fixture)?;
    let mut owner = plain_loopback_owner(fixture, endpoint, 16, 3)?;
    let initial = exercise_initial_connection(&mut owner, fixture)?;
    let recovery = exercise_close_and_invalid_reconnect(&mut owner)?;
    let replacement = exercise_replacement_and_shutdown(&mut owner, server)?;
    Ok([
        initial[0],
        initial[1],
        recovery[0],
        recovery[1],
        replacement[0],
        replacement[1],
    ])
}

fn spawn_multi_connection_server(
    fixture: &BinaryMarketStreamFixture,
) -> Result<(PlainLoopbackWebSocketEndpoint, LifecycleServerHandle), ConformanceHarnessError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let address = listener.local_addr().map_err(websocket_error)?;
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let websocket_config = websocket_fixture_config(fixture, 8)?.websocket_config();
    let first_payload = fixture.joined_frames.clone();
    let second_payload = fixture.joined_frames[fixture.snapshot_frame.len()..].to_vec();
    let third_snapshot = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 3 })
        .map_err(market_stream_error)?;
    let third_snapshot = replay_snapshot_with_generation(&third_snapshot, 3)?;
    let third_envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &third_snapshot,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let third_payload =
        encode_market_bar_stream_frame(&third_envelope, fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
    let server = thread::Builder::new()
        .name("axiusflow-websocket-owner-lifecycle".to_string())
        .spawn(move || {
            serve_lifecycle_connections(
                &listener,
                websocket_config,
                first_payload,
                second_payload,
                third_payload,
            )
        })
        .map_err(websocket_error)?;
    Ok((endpoint, server))
}

fn serve_lifecycle_connections(
    listener: &TcpListener,
    websocket_config: tungstenite::protocol::WebSocketConfig,
    first_payload: Vec<u8>,
    second_payload: Vec<u8>,
    third_payload: Vec<u8>,
) -> Result<bool, String> {
    let first_stream = accept_runtime_connection(listener, Duration::from_secs(3))?;
    configure_runtime_server_stream(&first_stream)?;
    let mut first = accept_with_config(first_stream, Some(websocket_config))
        .map_err(|error| error.to_string())?;
    first
        .send(Message::binary(first_payload))
        .map_err(|error| error.to_string())?;
    first.close(None).map_err(|error| error.to_string())?;

    let second_stream = accept_runtime_connection(listener, Duration::from_secs(3))?;
    configure_runtime_server_stream(&second_stream)?;
    let mut second = accept_with_config(second_stream, Some(websocket_config))
        .map_err(|error| error.to_string())?;
    second
        .send(Message::binary(second_payload))
        .map_err(|error| error.to_string())?;
    second.close(None).map_err(|error| error.to_string())?;

    let third_stream = accept_runtime_connection(listener, Duration::from_secs(3))?;
    configure_runtime_server_stream(&third_stream)?;
    let mut third = accept_with_config(third_stream, Some(websocket_config))
        .map_err(|error| error.to_string())?;
    third
        .send(Message::binary(third_payload))
        .map_err(|error| error.to_string())?;
    let peer_close = matches!(
        third.read().map_err(|error| error.to_string())?,
        Message::Close(_)
    );
    let _ = third.flush();
    Ok(peer_close)
}

fn exercise_initial_connection(
    owner: &mut PlainLoopbackMarketWebSocketOwner,
    fixture: &BinaryMarketStreamFixture,
) -> Result<[bool; 2], ConformanceHarnessError> {
    owner
        .try_enqueue_command(PlainLoopbackCommand::Connect)
        .map_err(websocket_error)?;
    let command_queue_bounded = matches!(
        owner.try_enqueue_command(PlainLoopbackCommand::Connect),
        Err(PlainLoopbackLifecycleError::CommandQueueFull { maximum: 1 })
    ) && owner.queued_commands() == 1;
    let connected = owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::Connected;
    let message =
        owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::MessageAccepted;
    let events = drain_all_lifecycle_events(owner);
    let published = connected
        && message
        && owner.state()
            == (PlainLoopbackLifecycleState::Streaming {
                connection_epoch: 1,
            })
        && lifecycle_publications_match(&events, &fixture.baseline, &fixture.delta);
    Ok([command_queue_bounded, published])
}

fn exercise_close_and_invalid_reconnect(
    owner: &mut PlainLoopbackMarketWebSocketOwner,
) -> Result<[bool; 2], ConformanceHarnessError> {
    let close_step = owner.step().map_err(websocket_error)?;
    let peer_close_recovered = close_step == PlainLoopbackLifecycleStep::RecoveryScheduled
        && matches!(
            owner.state(),
            PlainLoopbackLifecycleState::ReconnectPending {
                reason: WebSocketRecoveryReason::PeerClosed,
                attempts: 0
            }
        );
    let _ = drain_all_lifecycle_events(owner);

    let connected = owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::Connected;
    let message = owner.step().map_err(websocket_error)?;
    let delta_rejected = connected
        && message == PlainLoopbackLifecycleStep::RecoveryScheduled
        && matches!(
            owner.state(),
            PlainLoopbackLifecycleState::ReconnectPending {
                reason: WebSocketRecoveryReason::PeerClosed,
                attempts: 1
            }
        )
        && owner.current_generation().is_some();
    let _ = drain_all_lifecycle_events(owner);
    Ok([peer_close_recovered, delta_rejected])
}

fn exercise_replacement_and_shutdown(
    owner: &mut PlainLoopbackMarketWebSocketOwner,
    server: LifecycleServerHandle,
) -> Result<[bool; 2], ConformanceHarnessError> {
    let connected = owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::Connected;
    let message =
        owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::MessageAccepted;
    let events = drain_all_lifecycle_events(owner);
    let snapshot_recovered = connected
        && message
        && owner.state()
            == (PlainLoopbackLifecycleState::Streaming {
                connection_epoch: 3,
            })
        && events.iter().any(|event| {
            matches!(
                event,
                PlainLoopbackLifecycleEvent::Connected {
                    connection_epoch: 3,
                    attempt: 2
                }
            )
        })
        && events.iter().any(|event| {
            matches!(
                event,
                PlainLoopbackLifecycleEvent::Publication(publication)
                    if matches!(&publication.update, ReplayStreamUpdate::Snapshot(_))
            )
        });

    owner
        .try_enqueue_command(PlainLoopbackCommand::Shutdown)
        .map_err(websocket_error)?;
    let stopped = owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::Stopped
        && owner.state() == PlainLoopbackLifecycleState::Stopped
        && matches!(
            owner.drain_events(NonZeroUsize::MIN).as_slice(),
            [PlainLoopbackLifecycleEvent::Stopped {
                close_handshake_complete: true,
                dropped_events: 0
            }]
        );
    let peer_closed = server
        .join()
        .map_err(|_| websocket_error("connection-owner lifecycle server panicked"))?
        .map_err(websocket_error)?;
    Ok([snapshot_recovered, stopped && peer_closed])
}

fn drain_all_lifecycle_events(
    owner: &mut PlainLoopbackMarketWebSocketOwner,
) -> Vec<PlainLoopbackLifecycleEvent> {
    owner.drain_events(NonZeroUsize::new(owner.queued_events()).unwrap_or(NonZeroUsize::MIN))
}

fn lifecycle_publications_match(
    events: &[PlainLoopbackLifecycleEvent],
    baseline: &axiusflow_application::ReplaySnapshot,
    delta: &axiusflow_protocols::StreamDelta<axiusflow_application::ProvenancedMarketBar>,
) -> bool {
    let publications = events
        .iter()
        .filter_map(|event| match event {
            PlainLoopbackLifecycleEvent::Publication(publication) => {
                Some(publication.as_ref().clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    projected_websocket_updates_match(&publications, baseline, delta)
}

fn run_lifecycle_event_overflow(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let address = listener.local_addr().map_err(websocket_error)?;
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let websocket_config = websocket_fixture_config(fixture, 8)?.websocket_config();
    let snapshot = fixture.snapshot_frame.clone();
    let delta = fixture.joined_frames[fixture.snapshot_frame.len()..].to_vec();
    let (release_delta, wait_for_delta_release) = std::sync::mpsc::sync_channel(1);
    let (delta_sent, await_delta_sent) = std::sync::mpsc::sync_channel(1);
    let server = thread::Builder::new()
        .name("axiusflow-websocket-owner-overflow".to_string())
        .spawn(move || -> Result<(), String> {
            let stream = accept_runtime_connection(&listener, Duration::from_secs(3))?;
            configure_runtime_server_stream(&stream)?;
            let mut socket = accept_with_config(stream, Some(websocket_config))
                .map_err(|error| error.to_string())?;
            socket
                .send(Message::binary(snapshot))
                .map_err(|error| error.to_string())?;
            wait_for_delta_release
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| error.to_string())?;
            socket
                .send(Message::binary(delta))
                .map_err(|error| error.to_string())?;
            delta_sent.send(()).map_err(|error| error.to_string())
        })
        .map_err(websocket_error)?;
    let mut owner = plain_loopback_owner(fixture, endpoint, 9, 2)?;
    owner
        .try_enqueue_command(PlainLoopbackCommand::Connect)
        .map_err(websocket_error)?;
    owner.step().map_err(websocket_error)?;
    owner.step().map_err(websocket_error)?;
    let stable_generation = owner.current_generation().cloned();
    let snapshot_streaming = owner.state()
        == (PlainLoopbackLifecycleState::Streaming {
            connection_epoch: 1,
        })
        && stable_generation
            .as_ref()
            .is_some_and(|generation| generation.sequence_range() == (1, 2));
    release_delta.send(()).map_err(websocket_error)?;
    await_delta_sent
        .recv_timeout(Duration::from_secs(3))
        .map_err(websocket_error)?;
    let overflow = owner.step();
    server
        .join()
        .map_err(|_| websocket_error("connection-owner overflow server panicked"))?
        .map_err(websocket_error)?;
    let events = owner.drain_events(NonZeroUsize::MIN);
    Ok(snapshot_streaming
        && matches!(
            overflow,
            Err(PlainLoopbackLifecycleError::EventQueueFull { maximum: 9, .. })
        )
        && owner.state()
            == (PlainLoopbackLifecycleState::ReconnectPending {
                reason: WebSocketRecoveryReason::PublicationQueueOverflow,
                attempts: 0,
            })
        && owner.current_generation() == stable_generation.as_ref()
        && matches!(
            events.as_slice(),
            [PlainLoopbackLifecycleEvent::RecoveryRequired {
                reason: WebSocketRecoveryReason::PublicationQueueOverflow,
                dropped_events: 2,
                ..
            }]
        ))
}

fn run_lifecycle_reconnect_exhaustion(
    fixture: &BinaryMarketStreamFixture,
) -> Result<[bool; 4], ConformanceHarnessError> {
    let reservation = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    let address = reservation.local_addr().map_err(websocket_error)?;
    drop(reservation);
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let mut owner = plain_loopback_owner(fixture, endpoint, 9, 2)?;
    owner
        .try_enqueue_command(PlainLoopbackCommand::Connect)
        .map_err(websocket_error)?;
    let first = owner.step().map_err(websocket_error)?;
    let second = owner.step().map_err(websocket_error)?;
    let failed_events = drain_all_lifecycle_events(&mut owner);
    let exhausted = first == PlainLoopbackLifecycleStep::ConnectionAttemptFailed
        && second == PlainLoopbackLifecycleStep::ReconnectExhausted
        && owner.state()
            == (PlainLoopbackLifecycleState::Exhausted {
                attempts: 2,
                reason: WebSocketRecoveryReason::TransportFailure,
            })
        && failed_events.iter().any(|event| {
            matches!(
                event,
                PlainLoopbackLifecycleEvent::ReconnectExhausted {
                    attempts: 2,
                    reason: WebSocketRecoveryReason::TransportFailure,
                    dropped_events: 0
                }
            )
        })
        && !failed_events
            .iter()
            .any(|event| matches!(event, PlainLoopbackLifecycleEvent::Connected { .. }));

    let reset = exercise_snapshot_reset_and_fresh_budget(fixture, address, &mut owner)?;
    Ok([
        exhausted,
        reset[0],
        reset[1],
        run_lifecycle_exhaustion_event_pressure(fixture)?,
    ])
}

fn exercise_snapshot_reset_and_fresh_budget(
    fixture: &BinaryMarketStreamFixture,
    address: std::net::SocketAddr,
    owner: &mut PlainLoopbackMarketWebSocketOwner,
) -> Result<[bool; 2], ConformanceHarnessError> {
    let listener = TcpListener::bind(address).map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let websocket_config = websocket_fixture_config(fixture, 8)?.websocket_config();
    let snapshot = fixture.snapshot_frame.clone();
    let server = thread::Builder::new()
        .name("axiusflow-websocket-owner-exhaustion-reset".to_string())
        .spawn(move || -> Result<(), String> {
            let stream = accept_runtime_connection(&listener, Duration::from_secs(3))?;
            configure_runtime_server_stream(&stream)?;
            let mut socket = accept_with_config(stream, Some(websocket_config))
                .map_err(|error| error.to_string())?;
            socket
                .send(Message::binary(snapshot))
                .map_err(|error| error.to_string())?;
            socket.close(None).map_err(|error| error.to_string())
        })
        .map_err(websocket_error)?;
    owner
        .try_enqueue_command(PlainLoopbackCommand::Connect)
        .map_err(websocket_error)?;
    let connected = owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::Connected;
    let streamed =
        owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::MessageAccepted;
    let recovered_events = drain_all_lifecycle_events(owner);
    let snapshot_recovered = connected
        && streamed
        && owner.state()
            == (PlainLoopbackLifecycleState::Streaming {
                connection_epoch: 1,
            })
        && recovered_events.iter().any(|event| {
            matches!(
                event,
                PlainLoopbackLifecycleEvent::Connected {
                    connection_epoch: 1,
                    attempt: 1
                }
            )
        });

    let recovery = owner.step().map_err(websocket_error)?;
    server
        .join()
        .map_err(|_| websocket_error("exhaustion-reset server panicked"))?
        .map_err(websocket_error)?;
    let recovery_events = drain_all_lifecycle_events(owner);
    let successful_snapshot_reset_budget = recovery
        == PlainLoopbackLifecycleStep::RecoveryScheduled
        && owner.state()
            == (PlainLoopbackLifecycleState::ReconnectPending {
                reason: WebSocketRecoveryReason::PeerClosed,
                attempts: 0,
            })
        && recovery_events.iter().any(|event| {
            matches!(
                event,
                PlainLoopbackLifecycleEvent::RecoveryRequired {
                    reason: WebSocketRecoveryReason::PeerClosed,
                    ..
                }
            )
        });
    let fresh_first = owner.step().map_err(websocket_error)?;
    let fresh_second = owner.step().map_err(websocket_error)?;
    let fresh_events = drain_all_lifecycle_events(owner);
    let fresh_cycle = fresh_first == PlainLoopbackLifecycleStep::ConnectionAttemptFailed
        && fresh_second == PlainLoopbackLifecycleStep::ReconnectExhausted
        && owner.state()
            == (PlainLoopbackLifecycleState::Exhausted {
                attempts: 2,
                reason: WebSocketRecoveryReason::PeerClosed,
            })
        && fresh_events.iter().any(|event| {
            matches!(
                event,
                PlainLoopbackLifecycleEvent::ConnectionAttemptFailed {
                    attempt: 1,
                    remaining: 1
                }
            )
        })
        && fresh_events.iter().any(|event| {
            matches!(
                event,
                PlainLoopbackLifecycleEvent::ReconnectExhausted {
                    attempts: 2,
                    reason: WebSocketRecoveryReason::PeerClosed,
                    dropped_events: 0
                }
            )
        });
    owner
        .try_enqueue_command(PlainLoopbackCommand::Shutdown)
        .map_err(websocket_error)?;
    let stopped = owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::Stopped;
    Ok([
        snapshot_recovered && successful_snapshot_reset_budget,
        fresh_cycle && stopped,
    ])
}

fn run_lifecycle_exhaustion_event_pressure(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let reservation = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    let address = reservation.local_addr().map_err(websocket_error)?;
    drop(reservation);
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let mut owner =
        plain_loopback_owner_with_timeout(fixture, endpoint, 1, 1, 2, Duration::from_secs(3))?;
    owner
        .try_enqueue_command(PlainLoopbackCommand::Connect)
        .map_err(websocket_error)?;
    let first = owner.step().map_err(websocket_error)?;
    let second = owner.step().map_err(websocket_error)?;
    let events = drain_all_lifecycle_events(&mut owner);
    Ok(first == PlainLoopbackLifecycleStep::ConnectionAttemptFailed
        && second == PlainLoopbackLifecycleStep::ReconnectExhausted
        && owner.state()
            == (PlainLoopbackLifecycleState::Exhausted {
                attempts: 2,
                reason: WebSocketRecoveryReason::TransportFailure,
            })
        && matches!(
            events.as_slice(),
            [PlainLoopbackLifecycleEvent::ReconnectExhausted {
                attempts: 2,
                reason: WebSocketRecoveryReason::TransportFailure,
                dropped_events: 1
            }]
        ))
}

fn run_lifecycle_silent_peer_timeout(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let address = listener.local_addr().map_err(websocket_error)?;
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let websocket_config = websocket_fixture_config(fixture, 8)?.websocket_config();
    let server = thread::Builder::new()
        .name("axiusflow-websocket-owner-silent-peer".to_string())
        .spawn(move || -> Result<(), String> {
            let stream = accept_runtime_connection(&listener, Duration::from_secs(3))?;
            configure_runtime_server_stream(&stream)?;
            let socket = accept_with_config(stream, Some(websocket_config))
                .map_err(|error| error.to_string())?;
            thread::sleep(Duration::from_millis(200));
            drop(socket);
            Ok(())
        })
        .map_err(websocket_error)?;
    let mut owner =
        plain_loopback_owner_with_timeout(fixture, endpoint, 16, 8, 2, Duration::from_millis(40))?;
    owner
        .try_enqueue_command(PlainLoopbackCommand::Connect)
        .map_err(websocket_error)?;
    let connected = owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::Connected;
    let started = std::time::Instant::now();
    let timed_out =
        owner.step().map_err(websocket_error)? == PlainLoopbackLifecycleStep::RecoveryScheduled;
    let elapsed = started.elapsed();
    server
        .join()
        .map_err(|_| websocket_error("silent-peer server panicked"))?
        .map_err(websocket_error)?;
    Ok(connected
        && timed_out
        && elapsed >= Duration::from_millis(30)
        && elapsed < Duration::from_millis(160)
        && owner.state()
            == (PlainLoopbackLifecycleState::ReconnectPending {
                reason: WebSocketRecoveryReason::TransportFailure,
                attempts: 1,
            }))
}

fn plain_loopback_owner(
    fixture: &BinaryMarketStreamFixture,
    endpoint: PlainLoopbackWebSocketEndpoint,
    event_capacity: usize,
    connection_attempt_limit: usize,
) -> Result<PlainLoopbackMarketWebSocketOwner, ConformanceHarnessError> {
    plain_loopback_owner_with_timeout(
        fixture,
        endpoint,
        event_capacity,
        8,
        connection_attempt_limit,
        Duration::from_secs(3),
    )
}

fn plain_loopback_owner_with_timeout(
    fixture: &BinaryMarketStreamFixture,
    endpoint: PlainLoopbackWebSocketEndpoint,
    event_capacity: usize,
    publication_capacity: usize,
    connection_attempt_limit: usize,
    io_timeout: Duration,
) -> Result<PlainLoopbackMarketWebSocketOwner, ConformanceHarnessError> {
    let session = websocket_fixture_session(
        fixture,
        websocket_fixture_config(fixture, publication_capacity)?,
    )?;
    let config = PlainLoopbackLifecycleConfig::try_new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(event_capacity).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(connection_attempt_limit).unwrap_or(NonZeroUsize::MIN),
        io_timeout,
    )
    .map_err(websocket_error)?;
    PlainLoopbackMarketWebSocketOwner::try_new(endpoint, config, session).map_err(websocket_error)
}

const BINARY_FIXTURE_SUBSCRIPTION_ID: &str = "fixture_market_bars";

struct BinaryMarketStreamFixture {
    convention: DecimalConvention,
    maximum_frame_bytes: NonZeroUsize,
    maximum_buffered_bytes: NonZeroUsize,
    baseline: axiusflow_application::ReplaySnapshot,
    delta: axiusflow_protocols::StreamDelta<axiusflow_application::ProvenancedMarketBar>,
    snapshot_frame: Vec<u8>,
    joined_frames: Vec<u8>,
    corrupted_frame: Vec<u8>,
    checksum_corrupted: bool,
}

fn binary_market_stream_fixture() -> Result<BinaryMarketStreamFixture, ConformanceHarnessError> {
    let maximum_frame_bytes = NonZeroUsize::new(65_536).unwrap_or(NonZeroUsize::MIN);
    let maximum_buffered_bytes = NonZeroUsize::new(131_072).unwrap_or(NonZeroUsize::MIN);
    let convention =
        DecimalConvention::try_new("usd_minor", "shares").map_err(market_stream_error)?;
    let source = EmbeddedReplaySource;
    let baseline = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .map_err(market_stream_error)?;
    let delta = source
        .load_delta(2)
        .map_err(market_stream_error)?
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing fixture delta".to_string())
        })?;
    let snapshot_envelope =
        try_encode_replay_snapshot_envelope(BINARY_FIXTURE_SUBSCRIPTION_ID, &baseline, &convention)
            .map_err(market_stream_error)?;
    let delta_envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        baseline.instrument(),
        baseline.bar_definition(),
        &delta,
        &convention,
    )
    .map_err(market_stream_error)?;
    let snapshot_frame = encode_market_bar_stream_frame(&snapshot_envelope, maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let delta_frame = encode_market_bar_stream_frame(&delta_envelope, maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let mut joined_frames = snapshot_frame.clone();
    joined_frames.extend_from_slice(&delta_frame);

    let mut corrupted_envelope = snapshot_envelope;
    let checksum_corrupted = corrupt_snapshot_checksum(&mut corrupted_envelope);
    let corrupted_frame = encode_market_bar_stream_frame(&corrupted_envelope, maximum_frame_bytes)
        .map_err(market_stream_error)?;
    Ok(BinaryMarketStreamFixture {
        convention,
        maximum_frame_bytes,
        maximum_buffered_bytes,
        baseline,
        delta,
        snapshot_frame,
        joined_frames,
        corrupted_frame,
        checksum_corrupted,
    })
}

fn corrupt_snapshot_checksum(
    envelope: &mut axiusflow_protocols::protobuf::axiusflow::market::v1::MarketBarStreamEnvelope,
) -> bool {
    match envelope.update.as_mut() {
        Some(market_bar_stream_envelope::Update::Snapshot(snapshot)) => {
            snapshot.checksum.first_mut().is_some_and(|first| {
                *first ^= 0xff;
                true
            })
        }
        Some(market_bar_stream_envelope::Update::Delta(_)) | None => false,
    }
}

fn framing_checks(
    fixture: &BinaryMarketStreamFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let mut fragmented_decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    let mut fragmented_updates = Vec::new();
    let mut observed_partial_frame = false;
    for chunk in fixture.joined_frames.chunks(7) {
        let updates = fragmented_decoder
            .push(chunk)
            .map_err(market_stream_error)?;
        observed_partial_frame |= updates.is_empty() && fragmented_decoder.buffered_bytes() > 0;
        fragmented_updates.extend(updates);
    }
    let fragmented_equivalent = observed_partial_frame
        && projected_updates_match(
            &fragmented_updates,
            BINARY_FIXTURE_SUBSCRIPTION_ID,
            &fixture.baseline,
            &fixture.delta,
        );

    let mut coalesced_decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    let coalesced_updates = coalesced_decoder
        .push(&fixture.joined_frames)
        .map_err(market_stream_error)?;
    let coalesced_equivalent = projected_updates_match(
        &coalesced_updates,
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &fixture.baseline,
        &fixture.delta,
    );
    Ok((fragmented_equivalent, coalesced_equivalent))
}

fn malformed_and_reconnect_checks(
    fixture: &BinaryMarketStreamFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let malformed_payload =
        encode_binary_frame(&[0xff], fixture.maximum_frame_bytes).map_err(market_stream_error)?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    decoder
        .push(&fixture.joined_frames)
        .map_err(market_stream_error)?;
    let malformed_rejected = decoder.push(&malformed_payload).is_err()
        && decoder.is_invalid()
        && decoder.push(&fixture.snapshot_frame).is_err();
    decoder.reset();
    let reconnect_restored = decoder
        .push(&fixture.snapshot_frame)
        .is_ok_and(|updates| updates.len() == 1 && !decoder.is_invalid());
    Ok((malformed_rejected, reconnect_restored))
}

fn checksum_corruption_check(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    Ok(fixture.checksum_corrupted
        && decoder.push(&fixture.corrupted_frame).is_err()
        && decoder.is_invalid())
}

fn gap_check(fixture: &BinaryMarketStreamFixture) -> Result<bool, ConformanceHarnessError> {
    let gap_delta = EmbeddedReplaySource
        .load_delta(3)
        .map_err(market_stream_error)?
        .ok_or_else(|| ConformanceHarnessError::MarketStream("missing gap delta".to_string()))?;
    let gap_envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.baseline.instrument(),
        fixture.baseline.bar_definition(),
        &gap_delta,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let gap_frame = encode_market_bar_stream_frame(&gap_envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    decoder
        .push(&fixture.snapshot_frame)
        .map_err(market_stream_error)?;
    Ok(decoder.push(&gap_frame).is_err() && decoder.is_invalid())
}

fn oversized_frame_check(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let oversized_prefix = u32::try_from(fixture.maximum_frame_bytes.get())
        .unwrap_or(u32::MAX)
        .saturating_add(1)
        .to_be_bytes();
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    Ok(decoder.push(&oversized_prefix).is_err()
        && decoder.is_invalid()
        && decoder.buffered_bytes() == 0)
}

fn passed_check_bits(checks: [bool; 7]) -> u8 {
    let mut passed_checks = 0_u8;
    for (bit, passed) in [
        0b0000_0001,
        0b0000_0010,
        0b0000_0100,
        0b0000_1000,
        0b0001_0000,
        0b0010_0000,
        0b0100_0000,
    ]
    .into_iter()
    .zip(checks)
    {
        if passed {
            passed_checks |= bit;
        }
    }
    passed_checks
}

fn binary_fixture_decoder(
    convention: DecimalConvention,
    maximum_frame_bytes: NonZeroUsize,
    maximum_buffered_bytes: NonZeroUsize,
) -> Result<BinaryMarketBarStreamDecoder, ConformanceHarnessError> {
    BinaryMarketBarStreamDecoder::try_new(
        convention,
        ReplayProvenance::EmbeddedFixture,
        maximum_frame_bytes,
        maximum_buffered_bytes,
    )
    .map_err(market_stream_error)
}

fn projected_updates_match(
    updates: &[ProjectedMarketBarUpdate],
    subscription_id: &str,
    baseline: &axiusflow_application::ReplaySnapshot,
    delta: &axiusflow_protocols::StreamDelta<axiusflow_application::ProvenancedMarketBar>,
) -> bool {
    updates.len() == 2
        && updates
            .iter()
            .all(|update| update.subscription_id == subscription_id)
        && matches!(
            &updates[0].update,
            ReplayStreamUpdate::Snapshot(snapshot) if snapshot == baseline
        )
        && matches!(
            &updates[1].update,
            ReplayStreamUpdate::Delta(actual) if actual == delta
        )
}

fn market_stream_error(error: impl fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::MarketStream(error.to_string())
}

/// Headless evidence for bounded atomic multi-frame snapshot assembly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotChunkConformance {
    passed_checks: u8,
}

impl SnapshotChunkConformance {
    const REQUIRED_CHECKS: u8 = 0b0111_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

struct SnapshotChunkFixture {
    convention: DecimalConvention,
    maximum_frame_bytes: NonZeroUsize,
    maximum_buffered_bytes: NonZeroUsize,
    baseline: axiusflow_application::ReplaySnapshot,
    envelopes: Vec<axiusflow_protocols::protobuf::axiusflow::market::v1::MarketBarStreamEnvelope>,
    frames: Vec<Vec<u8>>,
}

/// Exercises bounded chunk order, identity, atomicity, integrity, and reset behavior.
///
/// This proves an in-process fixture protocol only, not a connected WebSocket,
/// provider, entitlement service, or production snapshot source.
///
/// # Errors
///
/// Returns an error when a valid fixture snapshot cannot traverse the chunk pipeline.
pub fn run_snapshot_chunk_conformance() -> Result<SnapshotChunkConformance, ConformanceHarnessError>
{
    let fixture = snapshot_chunk_fixture()?;
    let (atomic_fragmented, frame_bounds) = fragmented_chunk_check(&fixture)?;
    let coalesced = coalesced_chunk_check(&fixture)?;
    let (out_of_order, reset_recovered) = order_and_reset_check(&fixture)?;
    let identity_mismatch = identity_mismatch_check(&fixture)?;
    let delta_interleaving = delta_interleaving_check(&fixture)?;
    let checksum_corruption = chunk_checksum_check(&fixture)?;
    Ok(SnapshotChunkConformance {
        passed_checks: passed_check_bits([
            atomic_fragmented,
            coalesced,
            out_of_order,
            identity_mismatch,
            delta_interleaving,
            checksum_corruption,
            reset_recovered && frame_bounds,
        ]),
    })
}

fn snapshot_chunk_fixture() -> Result<SnapshotChunkFixture, ConformanceHarnessError> {
    let convention =
        DecimalConvention::try_new("usd_minor", "shares").map_err(market_stream_error)?;
    let maximum_frame_bytes = NonZeroUsize::new(65_536).unwrap_or(NonZeroUsize::MIN);
    let maximum_buffered_bytes = NonZeroUsize::new(131_072).unwrap_or(NonZeroUsize::MIN);
    let baseline = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 5 })
        .map_err(market_stream_error)?;
    let envelopes = try_encode_replay_snapshot_chunk_envelopes(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        "fixture_snapshot_chunks",
        &baseline,
        &convention,
        NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(market_stream_error)?;
    let frames = envelopes
        .iter()
        .map(|envelope| encode_market_bar_stream_frame(envelope, maximum_frame_bytes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(market_stream_error)?;
    Ok(SnapshotChunkFixture {
        convention,
        maximum_frame_bytes,
        maximum_buffered_bytes,
        baseline,
        envelopes,
        frames,
    })
}

fn chunk_decoder(
    fixture: &SnapshotChunkFixture,
) -> Result<BinaryMarketBarStreamDecoder, ConformanceHarnessError> {
    binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )
}

fn fragmented_chunk_check(
    fixture: &SnapshotChunkFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let mut decoder = chunk_decoder(fixture)?;
    let mut projected = Vec::new();
    let mut published_early = false;
    for (frame_index, frame) in fixture.frames.iter().enumerate() {
        for bytes in frame.chunks(11) {
            let updates = decoder.push(bytes).map_err(market_stream_error)?;
            if frame_index + 1 < fixture.frames.len() && !updates.is_empty() {
                published_early = true;
            }
            projected.extend(updates);
        }
    }
    let frame_bounds = fixture.frames.iter().all(|frame| {
        frame.len()
            <= fixture
                .maximum_frame_bytes
                .get()
                .saturating_add(axiusflow_transport::BINARY_FRAME_LENGTH_BYTES)
    });
    Ok((
        !published_early && chunked_snapshot_matches(&projected, fixture),
        frame_bounds,
    ))
}

fn coalesced_chunk_check(fixture: &SnapshotChunkFixture) -> Result<bool, ConformanceHarnessError> {
    let mut bytes = Vec::new();
    for frame in &fixture.frames {
        bytes.extend_from_slice(frame);
    }
    let updates = chunk_decoder(fixture)?
        .push(&bytes)
        .map_err(market_stream_error)?;
    Ok(chunked_snapshot_matches(&updates, fixture))
}

fn order_and_reset_check(
    fixture: &SnapshotChunkFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let second = fixture.frames.get(1).ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing second snapshot chunk".to_string())
    })?;
    let mut decoder = chunk_decoder(fixture)?;
    let out_of_order = decoder.push(second).is_err() && decoder.is_invalid();
    decoder.reset();
    let mut recovered = Vec::new();
    for frame in &fixture.frames {
        recovered.extend(decoder.push(frame).map_err(market_stream_error)?);
    }
    Ok((
        out_of_order,
        chunked_snapshot_matches(&recovered, fixture) && !decoder.is_invalid(),
    ))
}

fn identity_mismatch_check(
    fixture: &SnapshotChunkFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut envelopes = fixture.envelopes.clone();
    let changed = envelopes.get_mut(1).is_some_and(|envelope| {
        let Some(market_bar_stream_envelope::Update::Snapshot(snapshot)) = envelope.update.as_mut()
        else {
            return false;
        };
        snapshot.snapshot_id.push_str("_changed");
        true
    });
    let first = fixture.frames.first().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing first snapshot chunk".to_string())
    })?;
    let changed_frame = envelopes
        .get(1)
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing changed snapshot chunk".to_string())
        })
        .and_then(|envelope| {
            encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes)
                .map_err(market_stream_error)
        })?;
    let mut decoder = chunk_decoder(fixture)?;
    decoder.push(first).map_err(market_stream_error)?;
    Ok(changed && decoder.push(&changed_frame).is_err() && decoder.is_invalid())
}

fn delta_interleaving_check(
    fixture: &SnapshotChunkFixture,
) -> Result<bool, ConformanceHarnessError> {
    let delta = EmbeddedReplaySource
        .load_delta(fixture.baseline.stream().last_sequence())
        .map_err(market_stream_error)?
        .ok_or_else(|| ConformanceHarnessError::MarketStream("missing chunk delta".to_string()))?;
    let envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.baseline.instrument(),
        fixture.baseline.bar_definition(),
        &delta,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let delta_frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let first = fixture.frames.first().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing first snapshot chunk".to_string())
    })?;
    let mut decoder = chunk_decoder(fixture)?;
    decoder.push(first).map_err(market_stream_error)?;
    Ok(decoder.push(&delta_frame).is_err() && decoder.is_invalid())
}

fn chunk_checksum_check(fixture: &SnapshotChunkFixture) -> Result<bool, ConformanceHarnessError> {
    let mut envelopes = fixture.envelopes.clone();
    let mut corrupted = true;
    for envelope in &mut envelopes {
        let Some(market_bar_stream_envelope::Update::Snapshot(snapshot)) = envelope.update.as_mut()
        else {
            corrupted = false;
            continue;
        };
        if let Some(first) = snapshot.checksum.first_mut() {
            *first ^= 0xff;
        } else {
            corrupted = false;
        }
    }
    let frames = envelopes
        .iter()
        .map(|envelope| encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(market_stream_error)?;
    let mut decoder = chunk_decoder(fixture)?;
    let mut rejected_at_completion = false;
    for (index, frame) in frames.iter().enumerate() {
        if let Ok(updates) = decoder.push(frame) {
            if index + 1 < frames.len() && !updates.is_empty() {
                return Ok(false);
            }
        } else {
            rejected_at_completion = index + 1 == frames.len() && decoder.is_invalid();
            break;
        }
    }
    Ok(corrupted && rejected_at_completion)
}

fn chunked_snapshot_matches(
    updates: &[ProjectedMarketBarUpdate],
    fixture: &SnapshotChunkFixture,
) -> bool {
    updates.len() == 1
        && updates[0].subscription_id == BINARY_FIXTURE_SUBSCRIPTION_ID
        && matches!(
            &updates[0].update,
            ReplayStreamUpdate::Snapshot(snapshot) if snapshot == &fixture.baseline
        )
}

/// Headless evidence for the bounded single-writer market-bar client model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketBarClientModelConformance {
    passed_checks: u8,
}

impl MarketBarClientModelConformance {
    const REQUIRED_CHECKS: u8 = 0b0111_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

type FixtureMarketGeneration =
    axiusflow_application::MarketGeneration<axiusflow_application::ProvenancedMarketBar>;

/// Separately reported work in the deterministic replay-to-host benchmark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayBenchmarkStage {
    DecoderAndClientModel,
    OriginFrameConstruction,
    GpuiHostPreparation,
}

/// Bounded nearest-rank timing report for one benchmark stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayBenchmarkStageReport {
    pub stage: ReplayBenchmarkStage,
    pub sample_count: usize,
    pub p50_nanos: u64,
    pub p95_nanos: u64,
    pub p99_nanos: u64,
    pub p99_9_nanos: u64,
    pub maximum_nanos: u64,
}

/// Whether a benchmark boundary was verified or deliberately left unmeasured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayBenchmarkEvidence {
    Verified,
    VerificationFailed,
    NotMeasured,
}

/// Honest headless replay-to-GPUI-host benchmark evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayToGpuiBenchmarkReport {
    pub warmup_iterations: usize,
    pub measurement_iterations: usize,
    pub decoder_and_model: ReplayBenchmarkStageReport,
    pub origin_frame: ReplayBenchmarkStageReport,
    pub gpui_host: ReplayBenchmarkStageReport,
    pub immutable_generation: ReplayBenchmarkEvidence,
    pub origin_consumed_latest_generation: ReplayBenchmarkEvidence,
    pub chart_frame_consumed_by_gpui_host: ReplayBenchmarkEvidence,
    pub renderer_submission: ReplayBenchmarkEvidence,
    pub physical_presentation: ReplayBenchmarkEvidence,
}

impl ReplayToGpuiBenchmarkReport {
    #[must_use]
    pub fn is_complete(self) -> bool {
        self.measurement_iterations > 0
            && self.decoder_and_model.sample_count == self.measurement_iterations
            && self.origin_frame.sample_count == self.measurement_iterations
            && self.gpui_host.sample_count == self.measurement_iterations
            && self.immutable_generation == ReplayBenchmarkEvidence::Verified
            && self.origin_consumed_latest_generation == ReplayBenchmarkEvidence::Verified
            && self.chart_frame_consumed_by_gpui_host == ReplayBenchmarkEvidence::Verified
            && self.renderer_submission == ReplayBenchmarkEvidence::NotMeasured
            && self.physical_presentation == ReplayBenchmarkEvidence::NotMeasured
    }
}

/// Runs deterministic replay through decoder, immutable model, Origin, and GPUI scene planning.
///
/// One warm-up iteration is excluded. This is host-preparation evidence, not a renderer
/// submission, display-vsync, scanout, or presented-pixel measurement.
///
/// # Errors
///
/// Returns an error when any established binary, model, Origin, or host boundary rejects the
/// deterministic fixture.
pub fn run_replay_to_gpui_host_benchmark(
    measurement_iterations: NonZeroUsize,
) -> Result<ReplayToGpuiBenchmarkReport, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let _warmup = run_replay_to_gpui_iteration(&fixture)?;
    let mut model_samples = Vec::with_capacity(measurement_iterations.get());
    let mut origin_samples = Vec::with_capacity(measurement_iterations.get());
    let mut host_samples = Vec::with_capacity(measurement_iterations.get());
    let mut immutable_generation_verified = true;
    let mut origin_consumed_latest_generation = true;
    let mut chart_frame_consumed_by_gpui_host = true;
    for _ in 0..measurement_iterations.get() {
        let sample = run_replay_to_gpui_iteration(&fixture)?;
        model_samples.push(sample.model_nanos);
        origin_samples.push(sample.origin_nanos);
        host_samples.push(sample.host_nanos);
        immutable_generation_verified &= sample.immutable_generation_verified;
        origin_consumed_latest_generation &= sample.origin_consumed_latest_generation;
        chart_frame_consumed_by_gpui_host &= sample.chart_frame_consumed_by_gpui_host;
    }
    Ok(ReplayToGpuiBenchmarkReport {
        warmup_iterations: 1,
        measurement_iterations: measurement_iterations.get(),
        decoder_and_model: benchmark_stage_report(
            ReplayBenchmarkStage::DecoderAndClientModel,
            &mut model_samples,
        ),
        origin_frame: benchmark_stage_report(
            ReplayBenchmarkStage::OriginFrameConstruction,
            &mut origin_samples,
        ),
        gpui_host: benchmark_stage_report(
            ReplayBenchmarkStage::GpuiHostPreparation,
            &mut host_samples,
        ),
        immutable_generation: benchmark_evidence(immutable_generation_verified),
        origin_consumed_latest_generation: benchmark_evidence(origin_consumed_latest_generation),
        chart_frame_consumed_by_gpui_host: benchmark_evidence(chart_frame_consumed_by_gpui_host),
        renderer_submission: ReplayBenchmarkEvidence::NotMeasured,
        physical_presentation: ReplayBenchmarkEvidence::NotMeasured,
    })
}

const fn benchmark_evidence(verified: bool) -> ReplayBenchmarkEvidence {
    if verified {
        ReplayBenchmarkEvidence::Verified
    } else {
        ReplayBenchmarkEvidence::VerificationFailed
    }
}

#[derive(Clone, Copy, Debug)]
struct ReplayToGpuiIteration {
    model_nanos: u64,
    origin_nanos: u64,
    host_nanos: u64,
    immutable_generation_verified: bool,
    origin_consumed_latest_generation: bool,
    chart_frame_consumed_by_gpui_host: bool,
}

fn run_replay_to_gpui_iteration(
    fixture: &BinaryMarketStreamFixture,
) -> Result<ReplayToGpuiIteration, ConformanceHarnessError> {
    let model_started = Instant::now();
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    let mut projected = decoder
        .push(&fixture.joined_frames)
        .map_err(market_stream_error)?;
    if projected.len() != 2 {
        return Err(ConformanceHarnessError::MarketStream(format!(
            "replay benchmark projected {} updates instead of two",
            projected.len()
        )));
    }
    let snapshot_projected = projected.remove(0);
    let delta_projected = projected.remove(0);
    if snapshot_projected.subscription_id != BINARY_FIXTURE_SUBSCRIPTION_ID
        || delta_projected.subscription_id != BINARY_FIXTURE_SUBSCRIPTION_ID
    {
        return Err(ConformanceHarnessError::MarketStream(
            "replay benchmark subscription identity changed".to_string(),
        ));
    }
    let ReplayStreamUpdate::Snapshot(snapshot) = snapshot_projected.update else {
        return Err(ConformanceHarnessError::MarketStream(
            "replay benchmark first update was not a snapshot".to_string(),
        ));
    };
    let origin_update = delta_projected.update.clone();
    let mut model = MarketBarClientModel::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
    let snapshot_generation = expect_published_generation(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(snapshot.clone()))
            .map_err(market_stream_error)?,
        "benchmark snapshot",
    )?;
    let snapshot_range = snapshot_generation.sequence_range();
    let delta_generation = expect_published_generation(
        model
            .apply_update(delta_projected.update)
            .map_err(market_stream_error)?,
        "benchmark delta",
    )?;
    let model_nanos = u64::try_from(model_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let latest_sequence = delta_generation.sequence_range().1;
    let immutable_generation_verified = snapshot_generation.sequence_range() == snapshot_range
        && snapshot_generation.items() == snapshot.bars()
        && delta_generation.generation() == snapshot_generation.generation().saturating_add(1);

    let host = run_origin_gpui_host_sample(&snapshot, origin_update)
        .map_err(|error| ConformanceHarnessError::MarketStream(error.to_string()))?;
    Ok(ReplayToGpuiIteration {
        model_nanos,
        origin_nanos: host.origin_frame_construction_nanos,
        host_nanos: host.gpui_host_preparation_nanos,
        immutable_generation_verified,
        origin_consumed_latest_generation: host.origin_last_source_sequence == latest_sequence,
        chart_frame_consumed_by_gpui_host: host.origin_primitive_count > 0
            && host.gpui_plan_operations > 0
            && host.submission_boundary_ready
            && !host.renderer_submission_performed
            && !host.physical_presentation_measured,
    })
}

fn benchmark_stage_report(
    stage: ReplayBenchmarkStage,
    samples: &mut [u64],
) -> ReplayBenchmarkStageReport {
    samples.sort_unstable();
    ReplayBenchmarkStageReport {
        stage,
        sample_count: samples.len(),
        p50_nanos: benchmark_percentile(samples, 500),
        p95_nanos: benchmark_percentile(samples, 950),
        p99_nanos: benchmark_percentile(samples, 990),
        p99_9_nanos: benchmark_percentile(samples, 999),
        maximum_nanos: samples.last().copied().unwrap_or(0),
    }
}

fn benchmark_percentile(sorted: &[u64], permille: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(permille).saturating_add(999) / 1_000;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Exercises binary projection into immutable bounded client-model generations.
///
/// This proves deterministic fixture/model semantics only. It does not prove a
/// WebSocket connection, live provider, entitlement service, or physical display.
///
/// # Errors
///
/// Returns an error when valid fixture data is rejected by decoding or model validation.
pub fn run_market_bar_client_model_conformance()
-> Result<MarketBarClientModelConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    let mut updates = decoder
        .push(&fixture.joined_frames)
        .map_err(market_stream_error)?;
    let binary_projection_complete = updates.len() == 2
        && updates
            .iter()
            .all(|update| update.subscription_id == BINARY_FIXTURE_SUBSCRIPTION_ID);
    let delta_update = updates.pop().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing projected fixture delta".to_string())
    })?;
    let snapshot_update = updates.pop().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing projected fixture snapshot".to_string())
    })?;

    let mut model = axiusflow_application::MarketBarClientModel::new(
        NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
    );
    let snapshot_generation = expect_published_generation(
        model
            .apply_update(snapshot_update.update)
            .map_err(market_stream_error)?,
        "fixture snapshot",
    )?;
    let snapshot_published = snapshot_generation.sequence_range() == (1, 2)
        && snapshot_generation.items() == fixture.baseline.bars();
    let delta_generation = expect_published_generation(
        model
            .apply_update(delta_update.update)
            .map_err(market_stream_error)?,
        "fixture delta",
    )?;
    let bounded_delta_published = delta_generation.sequence_range() == (2, 3)
        && delta_generation.generation() == snapshot_generation.generation().saturating_add(1)
        && delta_generation.items().last() == Some(fixture.delta.item());
    let prior_generation_immutable = snapshot_generation.sequence_range() == (1, 2)
        && snapshot_generation.items() == fixture.baseline.bars();
    let provenance_retained = delta_generation
        .items()
        .iter()
        .all(|item| axiusflow_application::validate_provenanced_market_bar(item).is_ok());
    let schema_change_latched = client_schema_change_requires_snapshot(&fixture)?;
    let (gap_latched, snapshot_recovered, recovered_generation) =
        exercise_gap_recovery(&mut model, &delta_generation)?;
    let ownership_handoff_blocked = exercise_ownership_handoff(&mut model, &recovered_generation)?;

    Ok(MarketBarClientModelConformance {
        passed_checks: passed_check_bits([
            binary_projection_complete,
            snapshot_published,
            bounded_delta_published,
            prior_generation_immutable,
            provenance_retained && schema_change_latched,
            gap_latched,
            snapshot_recovered && ownership_handoff_blocked,
        ]),
    })
}

fn expect_published_generation(
    outcome: axiusflow_application::MarketBarModelOutcome,
    scenario: &str,
) -> Result<FixtureMarketGeneration, ConformanceHarnessError> {
    if let axiusflow_application::MarketBarModelOutcome::Published(generation) = outcome {
        return Ok(generation);
    }
    Err(ConformanceHarnessError::MarketStream(format!(
        "{scenario} did not publish an immutable generation"
    )))
}

fn client_schema_change_requires_snapshot(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut model = MarketBarClientModel::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN));
    let installed = expect_published_generation(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(fixture.baseline.clone()))
            .map_err(market_stream_error)?,
        "schema fixture snapshot",
    )?;
    let mut provenance = fixture.delta.item().provenance().clone();
    provenance.schema_version = provenance.schema_version.saturating_add(1);
    let changed_item =
        axiusflow_application::Provenanced::new(*fixture.delta.item().value(), provenance);
    let changed_delta = axiusflow_application::StreamDelta::try_new(
        fixture.delta.previous_sequence(),
        fixture.delta.sequence(),
        changed_item,
    )
    .map_err(market_stream_error)?;
    let outcome = model
        .apply_update(ReplayStreamUpdate::Delta(changed_delta))
        .map_err(market_stream_error)?;
    Ok(matches!(
        outcome,
        MarketBarModelOutcome::ResnapshotRequired(ResnapshotReason::SchemaChanged)
    ) && model.current_generation() == Some(&installed)
        && model.requires_snapshot())
}

fn replay_snapshot_with_generation(
    snapshot: &axiusflow_application::ReplaySnapshot,
    generation: u64,
) -> Result<axiusflow_application::ReplaySnapshot, ConformanceHarnessError> {
    let mut evidence = snapshot.evidence().clone();
    evidence.generation = generation;
    evidence.checksum = axiusflow_protocols::compute_market_snapshot_checksum(
        &evidence,
        axiusflow_protocols::MarketSnapshotIdentityRef {
            instrument_id: snapshot.instrument().instrument_id.as_str(),
            instrument_revision: snapshot.instrument().revision,
            bar_definition_id: &snapshot.bar_definition().definition_id,
            bar_definition_version: snapshot.bar_definition().version,
            bar_interval_seconds: snapshot.bar_definition().interval_seconds,
        },
        snapshot.bars().iter().map(|item| {
            let bar = item.value();
            axiusflow_protocols::MarketValueChecksumRef {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
                provenance: item.provenance(),
            }
        }),
    );
    axiusflow_application::ReplaySnapshot::try_new_provenanced(
        snapshot.instrument().clone(),
        snapshot.provenance(),
        snapshot.bar_definition().clone(),
        evidence,
        snapshot.bars().to_vec(),
    )
    .map_err(market_stream_error)
}

fn exercise_gap_recovery(
    model: &mut axiusflow_application::MarketBarClientModel,
    frozen_generation: &FixtureMarketGeneration,
) -> Result<(bool, bool, FixtureMarketGeneration), ConformanceHarnessError> {
    let source = EmbeddedReplaySource;
    let gap_delta = source
        .load_delta(4)
        .map_err(market_stream_error)?
        .ok_or_else(|| ConformanceHarnessError::MarketStream("missing gap delta".to_string()))?;
    let gap_outcome = model
        .apply_update(ReplayStreamUpdate::Delta(gap_delta))
        .map_err(market_stream_error)?;
    let contiguous_delta = source
        .load_delta(3)
        .map_err(market_stream_error)?
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing contiguous delta".to_string())
        })?;
    let latched_outcome = model
        .apply_update(ReplayStreamUpdate::Delta(contiguous_delta))
        .map_err(market_stream_error)?;
    let gap_latched = matches!(
        gap_outcome,
        axiusflow_application::MarketBarModelOutcome::ResnapshotRequired(
            axiusflow_application::ResnapshotReason::SequenceGap
        )
    ) && matches!(
        latched_outcome,
        axiusflow_application::MarketBarModelOutcome::ResnapshotRequired(
            axiusflow_application::ResnapshotReason::SequenceGap
        )
    ) && model.current_generation() == Some(frozen_generation);

    let recovery = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(market_stream_error)?;
    let recovery = replay_snapshot_with_generation(&recovery, 3)?;
    let recovered_generation = expect_published_generation(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(recovery.clone()))
            .map_err(market_stream_error)?,
        "gap recovery snapshot",
    )?;
    let snapshot_recovered = !model.requires_snapshot()
        && recovered_generation.sequence_range() == (3, 4)
        && recovered_generation.items().len() == 2
        && recovered_generation
            .items()
            .iter()
            .eq(recovery.bars().iter().skip(2));
    Ok((gap_latched, snapshot_recovered, recovered_generation))
}

fn exercise_ownership_handoff(
    model: &mut axiusflow_application::MarketBarClientModel,
    frozen_generation: &FixtureMarketGeneration,
) -> Result<bool, ConformanceHarnessError> {
    let source_delta = EmbeddedReplaySource
        .load_delta(4)
        .map_err(market_stream_error)?
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing ownership delta".to_string())
        })?;
    let mut changed_provenance = source_delta.item().provenance().clone();
    changed_provenance.ownership_epoch = changed_provenance.ownership_epoch.saturating_add(1);
    let changed_item =
        axiusflow_application::Provenanced::new(*source_delta.item().value(), changed_provenance);
    let changed_delta = axiusflow_application::StreamDelta::try_new(
        source_delta.previous_sequence(),
        source_delta.sequence(),
        changed_item,
    )
    .map_err(market_stream_error)?;
    let outcome = model
        .apply_update(ReplayStreamUpdate::Delta(changed_delta))
        .map_err(market_stream_error)?;
    Ok(matches!(
        outcome,
        axiusflow_application::MarketBarModelOutcome::ResnapshotRequired(
            axiusflow_application::ResnapshotReason::OwnershipHandoff
        )
    ) && model.requires_snapshot()
        && model.current_generation() == Some(frozen_generation))
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

/// Headless evidence for one bounded partition-owned canonical latest-state projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatestStateSnapshotConformance {
    passed_checks: u8,
}

impl LatestStateSnapshotConformance {
    const REQUIRED_CHECKS: u8 = 0b0011_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises direct-event projection into bounded canonical latest state and atomic
/// chunked recovery consumption using deterministic in-memory fixtures only.
///
/// This does not provide or claim a network snapshot service, live provider,
/// durable broker/archive, TLS, authentication, entitlement enforcement, WAN,
/// desktop production wiring, hardware evidence, or production readiness.
///
/// # Errors
///
/// Returns an error when a valid partition, fanout, snapshot, or client transition fails.
#[allow(clippy::too_many_lines)]
pub fn run_latest_state_snapshot_conformance()
-> Result<LatestStateSnapshotConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let source = EmbeddedReplaySource;
    let source_two = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .map_err(market_stream_error)?;
    let source_four = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(market_stream_error)?;
    let source_five = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 5 })
        .map_err(market_stream_error)?;
    let source_six = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 6 })
        .map_err(market_stream_error)?;
    let projection_identity_rejected =
        canonical_projection_identity_mismatches_are_rejected(&source_four)?;

    let owner = PartitionOwner::try_new(7, "latest_state_owner_1", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence_one = partition.publication_fence();
    let initial = canonical_market_bar_snapshot(&source_two, 1, 1)?;
    let mut latest = CanonicalLatestState::try_new(
        &mut partition,
        &fence_one,
        &initial,
        NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(realtime_error)?;
    let initial_served = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
        )
        .map_err(realtime_error)?;
    let initial_installed = latest.generation() == 1
        && latest.retained_items() == 2
        && initial_served.descriptor().first_sequence.get() == 2
        && initial_served.descriptor().last_sequence.get() == 2;
    let rejected_acceptance_mints_no_token =
        duplicate_and_gap_do_not_mint_publication_tokens(&initial, &source_two, &source_four)?;
    let changed_event_rejected =
        latest_state_identity_changes_are_rejected(&initial, &source_four)?;

    let direct = BoundedEventBranch::try_new(queue_policy("latest_state_direct", 2))
        .map_err(realtime_error)?;
    let durable = BoundedEventBranch::try_new(queue_policy("latest_state_durable", 4))
        .map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence_one.clone(), direct, durable).map_err(realtime_error)?;
    let event_three = canonical_market_bar_event(&source_four.bars()[2], &source_four, 1)?;
    publish_direct_to_latest(
        &mut partition,
        &mut fanout,
        &mut latest,
        &fence_one,
        event_three,
    )?;
    let immutable_generation_two = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let event_four = canonical_market_bar_event(&source_four.bars()[3], &source_four, 1)?;
    publish_direct_to_latest(
        &mut partition,
        &mut fanout,
        &mut latest,
        &fence_one,
        event_four,
    )?;
    let generation_three = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let direct_advanced = immutable_generation_two.descriptor().generation.get() == 2
        && immutable_generation_two.descriptor().first_sequence.get() == 2
        && immutable_generation_two.descriptor().last_sequence.get() == 3
        && generation_three.descriptor().generation.get() == 3
        && generation_three.descriptor().first_sequence.get() == 3
        && generation_three.descriptor().last_sequence.get() == 4
        && latest.retained_items() == 3;

    let skipped_event = canonical_market_bar_event(&source_five.bars()[4], &source_five, 1)?;
    let _skipped_direct =
        accept_publish_pop_direct(&mut partition, &mut fanout, &fence_one, skipped_event)?;
    let gap_event = canonical_market_bar_event(&source_six.bars()[5], &source_six, 1)?;
    let gap_direct = accept_publish_pop_direct(&mut partition, &mut fanout, &fence_one, gap_event)?;
    let gap_latched = latest
        .apply_direct_event(&partition, &fence_one, gap_direct)
        .map_err(realtime_error)?
        == PartitionDecision::Gap {
            expected: 5,
            actual: 6,
        }
        && latest.requires_snapshot()
        && matches!(
            latest.serve_snapshot(
                &partition,
                &fence_one,
                LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
            ),
            Err(RealtimeError::LatestStateSnapshotRequired)
        );

    let recovery = canonical_market_bar_snapshot(&source_six, 1, 4)?;
    latest
        .install_snapshot(&mut partition, &fence_one, &recovery)
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence_one, &recovery)
        .map_err(realtime_error)?;
    let recovered = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let recovered_snapshot = !latest.requires_snapshot()
        && recovered.descriptor().generation.get() == 4
        && recovered.descriptor().first_sequence.get() == 5
        && recovered.descriptor().last_sequence.get() == 6;
    let chunked_installed =
        canonical_snapshot_chunks_install_atomically(&fixture, &source_six, &recovered)?;
    let stale_client_snapshot_preserved =
        stale_client_snapshots_preserve_generation(&source_five, &source_six, &recovery)?;

    partition
        .handoff(PartitionOwner::try_new(7, "latest_state_owner_2", 2).map_err(realtime_error)?)
        .map_err(realtime_error)?;
    let fence_two = partition.publication_fence();
    let handoff_fenced = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
        )
        .is_err()
        && latest
            .serve_snapshot(
                &partition,
                &fence_two,
                LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
            )
            .is_err();
    let replacement = canonical_market_bar_snapshot(&source_five, 2, 1)?;
    latest
        .install_snapshot(&mut partition, &fence_two, &replacement)
        .map_err(realtime_error)?;
    fanout
        .activate_fence(fence_two.clone())
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence_two, &replacement)
        .map_err(realtime_error)?;
    let epoch_two_event = canonical_market_bar_event(&source_six.bars()[5], &source_six, 2)?;
    publish_direct_to_latest(
        &mut partition,
        &mut fanout,
        &mut latest,
        &fence_two,
        epoch_two_event,
    )?;
    let epoch_two = latest
        .serve_snapshot(
            &partition,
            &fence_two,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let handoff_recovered = epoch_two.descriptor().ownership_epoch.get() == 2
        && epoch_two.descriptor().generation.get() == 2
        && epoch_two.descriptor().first_sequence.get() == 4
        && epoch_two.descriptor().last_sequence.get() == 6;

    let passed_checks = [
        initial_installed && projection_identity_rejected,
        direct_advanced && rejected_acceptance_mints_no_token,
        gap_latched && changed_event_rejected,
        recovered_snapshot,
        chunked_installed && stale_client_snapshot_preserved,
        handoff_fenced && handoff_recovered,
    ]
    .into_iter()
    .enumerate()
    .fold(0_u8, |checks, (index, passed)| {
        checks | (u8::from(passed) << index)
    });
    Ok(LatestStateSnapshotConformance { passed_checks })
}

fn canonical_projection_identity_mismatches_are_rejected(
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let event = canonical_market_bar_event(&source.bars()[2], source, 1)?;
    let mut wrong_instrument = source.instrument().clone();
    wrong_instrument.revision = wrong_instrument.revision.saturating_add(1);
    let revision_rejected = matches!(
        try_project_canonical_market_bar(&event, &wrong_instrument, source.bar_definition()),
        Err(CanonicalMarketBarProjectionError::SeriesIdentityMismatch(
            "instrument_revision"
        ))
    );
    let mut wrong_definition = source.bar_definition().clone();
    wrong_definition.version = wrong_definition.version.saturating_add(1);
    let definition_rejected = matches!(
        try_project_canonical_market_bar(&event, source.instrument(), &wrong_definition),
        Err(CanonicalMarketBarProjectionError::SeriesIdentityMismatch(
            "definition_version"
        ))
    );
    let mut missing_header = event.header().clone();
    missing_header.series_identity = None;
    let missing_identity =
        CanonicalMarketEvent::try_new(missing_header, event.payload()).map_err(realtime_error)?;
    let missing_rejected = matches!(
        try_project_canonical_market_bar(
            &missing_identity,
            source.instrument(),
            source.bar_definition(),
        ),
        Err(CanonicalMarketBarProjectionError::MissingSeriesIdentity)
    );
    Ok(revision_rejected && definition_rejected && missing_rejected)
}

fn duplicate_and_gap_do_not_mint_publication_tokens(
    initial: &CanonicalSnapshot,
    source_two: &axiusflow_application::ReplaySnapshot,
    source_four: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let owner =
        PartitionOwner::try_new(7, "acceptance_negative_owner", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    partition
        .install_snapshot(&fence, initial)
        .map_err(realtime_error)?;
    let duplicate = canonical_market_bar_event(&source_two.bars()[1], source_two, 1)?;
    let duplicate_rejected = matches!(
        partition
            .accept_for_publication(&fence, duplicate)
            .map_err(realtime_error)?,
        PartitionAcceptance::Duplicate
    );
    let gap = canonical_market_bar_event(&source_four.bars()[3], source_four, 1)?;
    let gap_rejected = matches!(
        partition
            .accept_for_publication(&fence, gap)
            .map_err(realtime_error)?,
        PartitionAcceptance::Gap {
            expected: 3,
            actual: 4,
        }
    );
    Ok(duplicate_rejected && gap_rejected)
}

fn latest_state_identity_changes_are_rejected(
    initial: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let base = canonical_market_bar_event(&source.bars()[2], source, 1)?;

    let mut series_header = base.header().clone();
    let series = series_header.series_identity.as_mut().ok_or_else(|| {
        ConformanceHarnessError::Realtime("fixture event lacks series identity".to_string())
    })?;
    series.instrument_revision = series.instrument_revision.saturating_add(1);
    let series_changed =
        CanonicalMarketEvent::try_new(series_header, base.payload()).map_err(realtime_error)?;

    let mut schema_header = base.header().clone();
    schema_header.schema_version = schema_header.schema_version.saturating_add(1);
    let schema_changed =
        CanonicalMarketEvent::try_new(schema_header, base.payload()).map_err(realtime_error)?;

    let mut semantic_header = base.header().clone();
    semantic_header.semantic_class = SemanticClass::StateReplace;
    let semantic_changed =
        CanonicalMarketEvent::try_new(semantic_header, base.payload()).map_err(realtime_error)?;

    Ok(
        latest_state_rejects_changed_event(initial, series_changed, "series_identity")?
            && latest_state_rejects_changed_event(initial, schema_changed, "schema_version")?
            && latest_state_rejects_changed_event(initial, semantic_changed, "semantic_class")?,
    )
}

fn latest_state_rejects_changed_event(
    initial: &CanonicalSnapshot,
    event: CanonicalMarketEvent,
    expected_field: &'static str,
) -> Result<bool, ConformanceHarnessError> {
    let owner = PartitionOwner::try_new(7, format!("latest_negative_{expected_field}"), 1)
        .map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    let mut latest = CanonicalLatestState::try_new(
        &mut partition,
        &fence,
        initial,
        NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(realtime_error)?;
    let semantic_class = event.header().semantic_class;
    let overflow_action = match semantic_class {
        SemanticClass::StateReplace => OverflowAction::ConflateNewest,
        SemanticClass::OrderedDelta => OverflowAction::RequestSnapshot,
        SemanticClass::ReliableEvent
        | SemanticClass::AuthoritativeEvent
        | SemanticClass::Snapshot => OverflowAction::RejectBeforeAcceptance,
    };
    let mut direct_policy = queue_policy("latest_negative_direct", 1);
    direct_policy.semantic_class = semantic_class;
    direct_policy.overflow_action = overflow_action;
    let mut durable_policy = queue_policy("latest_negative_durable", 1);
    durable_policy.semantic_class = semantic_class;
    durable_policy.overflow_action = overflow_action;
    let direct = BoundedEventBranch::try_new(direct_policy).map_err(realtime_error)?;
    let durable = BoundedEventBranch::try_new(durable_policy).map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence.clone(), direct, durable).map_err(realtime_error)?;
    let direct = accept_publish_pop_direct(&mut partition, &mut fanout, &fence, event)?;
    let rejected = matches!(
        latest.apply_direct_event(&partition, &fence, direct),
        Err(RealtimeError::SnapshotEventMismatch { field, .. }) if field == expected_field
    );
    let preserved = latest.generation() == 1
        && latest.retained_items() == 2
        && latest.requires_snapshot()
        && matches!(
            latest.serve_snapshot(
                &partition,
                &fence,
                LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
            ),
            Err(RealtimeError::LatestStateSnapshotRequired)
        );
    Ok(rejected && preserved)
}

fn stale_client_snapshots_preserve_generation(
    source_five: &axiusflow_application::ReplaySnapshot,
    source_six: &axiusflow_application::ReplaySnapshot,
    current: &CanonicalSnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let current_replay = try_project_canonical_market_bar_snapshot(
        current,
        source_six.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_six.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let mut model = MarketBarClientModel::new(NonZeroUsize::new(6).unwrap_or(NonZeroUsize::MIN));
    let current_installed = matches!(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(current_replay))
            .map_err(market_stream_error)?,
        MarketBarModelOutcome::Published(_)
    );
    let stale = canonical_market_bar_snapshot(source_five, 1, 3)?;
    let stale_replay = try_project_canonical_market_bar_snapshot(
        &stale,
        source_five.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_five.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let stale_rejected = model
        .apply_update(ReplayStreamUpdate::Snapshot(stale_replay))
        .is_err();
    let stale_preserved = model.current_generation().is_some_and(|generation| {
        generation.ownership_epoch() == 1
            && generation.generation() == 4
            && generation.sequence_range() == (1, 6)
    });

    let epoch_two = canonical_market_bar_snapshot(source_five, 2, 1)?;
    let epoch_two_replay = try_project_canonical_market_bar_snapshot(
        &epoch_two,
        source_five.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_five.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let epoch_two_installed = matches!(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(epoch_two_replay))
            .map_err(market_stream_error)?,
        MarketBarModelOutcome::Published(_)
    );
    let lower_epoch = canonical_market_bar_snapshot(source_six, 1, 5)?;
    let lower_epoch_replay = try_project_canonical_market_bar_snapshot(
        &lower_epoch,
        source_six.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_six.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let lower_epoch_rejected = model
        .apply_update(ReplayStreamUpdate::Snapshot(lower_epoch_replay))
        .is_err();
    let lower_epoch_preserved = model.current_generation().is_some_and(|generation| {
        generation.ownership_epoch() == 2
            && generation.generation() == 1
            && generation.sequence_range() == (1, 5)
    });
    Ok(current_installed
        && stale_rejected
        && stale_preserved
        && epoch_two_installed
        && lower_epoch_rejected
        && lower_epoch_preserved)
}

fn publish_direct_to_latest(
    partition: &mut FencedPartition,
    fanout: &mut DirectDurableFanout,
    latest: &mut CanonicalLatestState,
    fence: &axiusflow_realtime::PublicationFence,
    event: CanonicalMarketEvent,
) -> Result<(), ConformanceHarnessError> {
    let direct = accept_publish_pop_direct(partition, fanout, fence, event)?;
    if latest
        .apply_direct_event(partition, fence, direct)
        .map_err(realtime_error)?
        != PartitionDecision::Accepted
    {
        return Err(ConformanceHarnessError::Realtime(
            "latest-state projection rejected a contiguous direct event".to_string(),
        ));
    }
    Ok(())
}

fn accept_for_fanout(
    partition: &mut FencedPartition,
    fence: &axiusflow_realtime::PublicationFence,
    event: CanonicalMarketEvent,
) -> Result<AcceptedCanonicalEvent, ConformanceHarnessError> {
    match partition
        .accept_for_publication(fence, event)
        .map_err(realtime_error)?
    {
        PartitionAcceptance::Accepted(accepted) => Ok(*accepted),
        outcome => Err(ConformanceHarnessError::Realtime(format!(
            "fixture event was not accepted for publication: {outcome:?}"
        ))),
    }
}

fn fanout_timestamp_mismatch_is_rejected(
    initial: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let owner = PartitionOwner::try_new(7, "timestamp_fixture_owner", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    partition
        .install_snapshot(&fence, initial)
        .map_err(realtime_error)?;
    let direct =
        BoundedEventBranch::try_new(queue_policy("timestamp_direct", 1)).map_err(realtime_error)?;
    let durable = BoundedEventBranch::try_new(queue_policy("timestamp_durable", 1))
        .map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence.clone(), direct, durable).map_err(realtime_error)?;
    let event = canonical_market_bar_event(&source.bars()[2], source, 1)?;
    let accepted = accept_for_fanout(&mut partition, &fence, event)?;
    let declared = canonical_enqueue_time(source, 2);
    let rejected = matches!(
        fanout.publish(&mut partition, &fence, accepted, declared.saturating_add(1),),
        Err(RealtimeError::FanoutEnqueueTimestampMismatch { .. })
    );
    let retried = canonical_market_bar_event(&source.bars()[2], source, 1)?;
    let partition_latched = matches!(
        partition
            .accept_for_publication(&fence, retried)
            .map_err(realtime_error)?,
        PartitionAcceptance::SnapshotRequired
    );
    let direct_metrics = fanout.direct_metrics(declared);
    let durable_metrics = fanout.durable_metrics(declared);
    Ok(rejected
        && partition_latched
        && direct_metrics.items == 0
        && direct_metrics.snapshot_required
        && durable_metrics.items == 0
        && durable_metrics.snapshot_required)
}

fn accept_publish_pop_direct(
    partition: &mut FencedPartition,
    fanout: &mut DirectDurableFanout,
    fence: &axiusflow_realtime::PublicationFence,
    event: CanonicalMarketEvent,
) -> Result<AcceptedDirectEvent, ConformanceHarnessError> {
    let accepted = accept_for_fanout(partition, fence, event)?;
    let enqueued = accepted
        .event()
        .header()
        .timestamps
        .fanout_enqueue_unix_nanos
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("fixture event lacks fanout time".to_string())
        })?;
    if fanout
        .publish(partition, fence, accepted, enqueued)
        .map_err(realtime_error)?
        .direct
        != QueueOutcome::Enqueued
    {
        return Err(ConformanceHarnessError::Realtime(
            "direct branch rejected an accepted fixture event".to_string(),
        ));
    }
    fanout
        .pop_direct(enqueued.saturating_add(10))
        .map(|(direct, _)| direct)
        .ok_or_else(|| ConformanceHarnessError::Realtime("missing direct event".to_string()))
}

fn canonical_snapshot_chunks_install_atomically(
    fixture: &BinaryMarketStreamFixture,
    source: &axiusflow_application::ReplaySnapshot,
    snapshot: &CanonicalSnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let replay = try_project_canonical_market_bar_snapshot(
        snapshot,
        source.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let envelopes = try_encode_replay_snapshot_chunk_envelopes(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        "latest_state_recovery_1",
        &replay,
        &fixture.convention,
        NonZeroUsize::MIN,
    )
    .map_err(market_stream_error)?;
    let matches_snapshot = |session: &MarketWebSocketSession| {
        session.current_generation().is_some_and(|generation| {
            generation.ownership_epoch() == snapshot.descriptor().ownership_epoch.get()
                && generation.generation() == snapshot.descriptor().generation.get()
                && generation.sequence_range()
                    == (
                        snapshot.descriptor().first_sequence.get(),
                        snapshot.descriptor().last_sequence.get(),
                    )
        })
    };

    let mut session = websocket_fixture_session(fixture, websocket_fixture_config(fixture, 2)?)?;
    let final_index = envelopes.len().saturating_sub(1);
    let mut atomic = true;
    for (index, envelope) in envelopes.iter().enumerate() {
        let frame = encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
        let outcome = session
            .accept_message(Message::binary(frame))
            .map_err(websocket_error)?;
        let expected = usize::from(index == final_index);
        atomic &= outcome
            == WebSocketMessageOutcome::BinaryAccepted {
                published: expected,
            }
            && (index == final_index || session.current_generation().is_none());
    }
    let installed = atomic && matches_snapshot(&session);

    let reordered_rejected = if envelopes.len() > 1 {
        let reordered = encode_market_bar_stream_frame(&envelopes[1], fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
        session.accept_message(Message::binary(reordered)).is_err() && matches_snapshot(&session)
    } else {
        false
    };

    let mut malformed_session =
        websocket_fixture_session(fixture, websocket_fixture_config(fixture, 2)?)?;
    for envelope in &envelopes {
        let frame = encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
        malformed_session
            .accept_message(Message::binary(frame))
            .map_err(websocket_error)?;
    }
    let mut malformed = encode_market_bar_stream_frame(&envelopes[0], fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let last = malformed.len().checked_sub(1).ok_or_else(|| {
        ConformanceHarnessError::MarketStream("empty encoded snapshot frame".to_string())
    })?;
    malformed[last] ^= 1;
    let malformed_rejected = malformed_session
        .accept_message(Message::binary(malformed))
        .is_err()
        && matches_snapshot(&malformed_session);

    Ok(installed && reordered_rejected && malformed_rejected)
}

/// Headless evidence for canonical direct fanout through market-bar wire consumption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectMarketBarWireConformance {
    passed_checks: u8,
}

impl DirectMarketBarWireConformance {
    const REQUIRED_CHECKS: u8 = u8::MAX;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises canonical ordered fanout, strict projection, framing, client consumption,
/// epoch handoff, and direct-branch overflow recovery using deterministic fixtures only.
///
/// This does not exercise or claim Redpanda, object storage, TLS, authentication,
/// entitlement enforcement, provider connectivity, WAN behavior, desktop wiring,
/// evented production operation, hardware timestamps, or production readiness.
///
/// # Errors
///
/// Returns an error when any valid canonical, queue, projection, or client transition fails.
#[allow(clippy::too_many_lines)]
pub fn run_direct_market_bar_wire_conformance()
-> Result<DirectMarketBarWireConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let source = EmbeddedReplaySource;
    let source_two = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .map_err(market_stream_error)?;
    let source_four = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(market_stream_error)?;
    let source_five = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 5 })
        .map_err(market_stream_error)?;

    let initial = canonical_market_bar_snapshot(&source_two, 1, 1)?;
    let invalid_payload_rejected =
        invalid_canonical_bar_payload_is_rejected(&initial, &source_two)?
            && wire_publication_timestamp_mismatch_is_rejected(&initial, &source_two, &fixture)?;
    let mut session = websocket_fixture_session(&fixture, websocket_fixture_config(&fixture, 8)?)?;
    install_canonical_snapshot(&mut session, &fixture, &initial, &source_two)?;
    let initial_generation = session.current_generation().is_some_and(|generation| {
        generation.ownership_epoch() == 1
            && generation.generation() == 1
            && generation.sequence_range() == (1, 2)
    });
    let _ = session.drain_publications(NonZeroUsize::MIN);

    let owner = PartitionOwner::try_new(7, "wire_fixture_owner_1", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence_one = partition.publication_fence();
    partition
        .install_snapshot(&fence_one, &initial)
        .map_err(realtime_error)?;
    let direct =
        BoundedEventBranch::try_new(queue_policy("wire_direct", 2)).map_err(realtime_error)?;
    let durable =
        BoundedEventBranch::try_new(queue_policy("wire_durable", 1)).map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence_one.clone(), direct, durable).map_err(realtime_error)?;

    let fanout_timestamp_bound = fanout_timestamp_mismatch_is_rejected(&initial, &source_four)?;
    let event_three = canonical_market_bar_event(&source_four.bars()[2], &source_four, 1)?;
    let accepted_three = accept_for_fanout(&mut partition, &fence_one, event_three)?;
    fanout
        .publish(
            &mut partition,
            &fence_one,
            accepted_three,
            canonical_enqueue_time(&source_four, 2),
        )
        .map_err(realtime_error)?;
    let (direct_three, _) = fanout
        .pop_direct(canonical_enqueue_time(&source_four, 2).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing direct sequence 3".to_string())
        })?;
    let publication_three = deliver_canonical_delta(
        &mut session,
        &fixture,
        &source_four,
        2,
        direct_three.event(),
    )?;
    let direct_delivery = fanout_timestamp_bound
        && initial_generation
        && publication_three.generation.generation() == 2
        && publication_three.generation.sequence_range() == (1, 3);

    let event_four = canonical_market_bar_event(&source_four.bars()[3], &source_four, 1)?;
    let accepted_four = accept_for_fanout(&mut partition, &fence_one, event_four)?;
    let outcomes = fanout
        .publish(
            &mut partition,
            &fence_one,
            accepted_four,
            canonical_enqueue_time(&source_four, 3),
        )
        .map_err(realtime_error)?;
    let branch_independence = outcomes.direct == QueueOutcome::Enqueued
        && outcomes.durable == QueueOutcome::SnapshotRequired
        && fanout
            .durable_metrics(canonical_enqueue_time(&source_four, 3))
            .snapshot_required;
    let (direct_four, _) = fanout
        .pop_direct(canonical_enqueue_time(&source_four, 3).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing direct sequence 4".to_string())
        })?;
    let expected_fanout = direct_four
        .event()
        .header()
        .timestamps
        .fanout_enqueue_unix_nanos;
    let publication_four =
        deliver_canonical_delta(&mut session, &fixture, &source_four, 3, direct_four.event())?;
    let retained = publication_four
        .generation
        .items()
        .last()
        .map(axiusflow_protocols::Provenanced::provenance);
    let provenance_retained = retained.is_some_and(|provenance| {
        provenance.event_id == direct_four.event().header().event_id
            && provenance.source_sequence == 4
            && provenance.partition_id == 7
            && provenance.ownership_epoch == 1
            && provenance.fanout_enqueue_timestamp_unix_nanos == expected_fanout
            && provenance.nic_timestamp_source == 1
    }) && publication_four.generation.generation() == 3
        && publication_four.generation.sequence_range() == (1, 4);

    let stale_event = canonical_market_bar_event(&source_five.bars()[4], &source_five, 1)?;
    let stale_enqueue = canonical_enqueue_time(&source_five, 4);
    let accepted_before_handoff =
        accept_for_fanout(&mut partition, &fence_one, stale_event.clone())?;
    partition
        .handoff(PartitionOwner::try_new(7, "wire_fixture_owner_2", 2).map_err(realtime_error)?)
        .map_err(realtime_error)?;
    let fence_two = partition.publication_fence();
    let partition_stale_rejected = partition.accept(&fence_one, &stale_event).is_err();
    let split_window_rejected = fanout
        .publish(
            &mut partition,
            &fence_one,
            accepted_before_handoff,
            stale_enqueue,
        )
        .is_err()
        && fanout.direct_metrics(stale_enqueue).items == 0;

    let mut reconstructed_old_partition = FencedPartition::new(
        PartitionOwner::try_new(7, "wire_fixture_owner_1", 1).map_err(realtime_error)?,
    );
    let reconstructed_fence = reconstructed_old_partition.publication_fence();
    reconstructed_old_partition
        .install_snapshot(&reconstructed_fence, &initial)
        .map_err(realtime_error)?;
    let reconstructed_event = canonical_market_bar_event(&source_four.bars()[2], &source_four, 1)?;
    let reconstructed_token = accept_for_fanout(
        &mut reconstructed_old_partition,
        &reconstructed_fence,
        reconstructed_event,
    )?;
    let reconstructed_authority_rejected = fanout
        .publish(
            &mut reconstructed_old_partition,
            &reconstructed_fence,
            reconstructed_token,
            canonical_enqueue_time(&source_four, 2),
        )
        .is_err();
    fanout
        .activate_fence(fence_two.clone())
        .map_err(realtime_error)?;
    let stale_writer_fenced =
        partition_stale_rejected && split_window_rejected && reconstructed_authority_rejected;

    let blocked_event = canonical_market_bar_event(&source_five.bars()[4], &source_five, 2)?;
    let blocked_by_handoff = matches!(
        partition
            .accept_for_publication(&fence_two, blocked_event)
            .map_err(realtime_error)?,
        PartitionAcceptance::SnapshotRequired
    ) && fanout
        .pop_direct(canonical_enqueue_time(&source_five, 4))
        .is_none();

    let replacement = canonical_market_bar_snapshot(&source_four, 2, 2)?;
    partition
        .install_snapshot(&fence_two, &replacement)
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence_two, &replacement)
        .map_err(realtime_error)?;
    session.require_resnapshot(ResnapshotReason::OwnershipHandoff);
    session.begin_reconnect();
    install_canonical_snapshot(&mut session, &fixture, &replacement, &source_four)?;
    let _ = session.drain_publications(NonZeroUsize::MIN);
    let event_five = canonical_market_bar_event(&source_five.bars()[4], &source_five, 2)?;
    let accepted_five = accept_for_fanout(&mut partition, &fence_two, event_five)?;
    fanout
        .publish(
            &mut partition,
            &fence_two,
            accepted_five,
            canonical_enqueue_time(&source_five, 4),
        )
        .map_err(realtime_error)?;
    let (direct_five, _) = fanout
        .pop_direct(canonical_enqueue_time(&source_five, 4).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing epoch-2 direct delta".to_string())
        })?;
    let publication_five =
        deliver_canonical_delta(&mut session, &fixture, &source_five, 4, direct_five.event())?;
    let handoff_recovered = blocked_by_handoff
        && publication_five.generation.ownership_epoch() == 2
        && publication_five.generation.generation() == 3
        && publication_five.generation.sequence_range() == (1, 5);

    let (direct_overflow_latched, direct_overflow_recovered) =
        run_direct_branch_overflow_recovery(&fixture, &source_four, &source_five)?;
    let passed_checks = [
        invalid_payload_rejected,
        direct_delivery,
        branch_independence,
        provenance_retained,
        stale_writer_fenced,
        handoff_recovered,
        direct_overflow_latched,
        direct_overflow_recovered,
    ]
    .into_iter()
    .enumerate()
    .fold(0_u8, |checks, (index, passed)| {
        checks | (u8::from(passed) << index)
    });
    Ok(DirectMarketBarWireConformance { passed_checks })
}

fn canonical_market_bar_snapshot(
    source: &axiusflow_application::ReplaySnapshot,
    ownership_epoch: u64,
    generation: u64,
) -> Result<CanonicalSnapshot, ConformanceHarnessError> {
    canonical_market_bar_snapshot_prefix(source, ownership_epoch, generation, source.bars().len())
}

fn canonical_market_bar_snapshot_prefix(
    source: &axiusflow_application::ReplaySnapshot,
    ownership_epoch: u64,
    generation: u64,
    item_count: usize,
) -> Result<CanonicalSnapshot, ConformanceHarnessError> {
    let events = source
        .bars()
        .iter()
        .take(item_count)
        .map(|item| canonical_market_bar_event(item, source, ownership_epoch))
        .collect::<Result<Vec<_>, _>>()?;
    CanonicalSnapshot::try_from_events(7, ownership_epoch, generation, 1, events)
        .map_err(realtime_error)
}

fn canonical_market_bar_event(
    item: &axiusflow_application::ProvenancedMarketBar,
    source: &axiusflow_application::ReplaySnapshot,
    ownership_epoch: u64,
) -> Result<CanonicalMarketEvent, ConformanceHarnessError> {
    let bar = item.value();
    let original = item.provenance();
    let normalized = original.exchange_timestamp_unix_nanos.saturating_add(3_000);
    let payload = try_encode_canonical_market_bar_payload(bar).map_err(market_stream_error)?;
    CanonicalMarketEvent::try_new(
        CanonicalEventHeader {
            event_id: format!("direct_wire_{ownership_epoch}_{}", bar.source_sequence),
            event_time_unix_nanos: original.event_time_unix_nanos,
            publication_time_unix_nanos: original
                .exchange_timestamp_unix_nanos
                .saturating_add(5_000),
            producer: "canonical_market_bar_fixture".to_string(),
            correlation_id: "direct_market_bar_wire_fixture".to_string(),
            causation_id: original.event_id.clone(),
            entitlement_revision: original.entitlement_revision.clone(),
            instrument_id: source.instrument().instrument_id.as_str().to_string(),
            venue_id: source.instrument().venue_id.clone(),
            source_id: original.source_id.clone(),
            series_identity: Some(CanonicalSeriesIdentity {
                instrument_revision: source.instrument().revision,
                definition_id: source.bar_definition().definition_id.clone(),
                definition_version: source.bar_definition().version,
                interval_seconds: source.bar_definition().interval_seconds,
            }),
            source_sequence: bar.source_sequence,
            partition_id: 7,
            ownership_epoch,
            timestamps: CanonicalTimestamps {
                exchange_unix_nanos: original.exchange_timestamp_unix_nanos,
                provider_receive_unix_nanos: original
                    .exchange_timestamp_unix_nanos
                    .saturating_add(1_000),
                nic_receive_unix_nanos: Some(
                    original.exchange_timestamp_unix_nanos.saturating_add(2_000),
                ),
                axiusflow_receive_unix_nanos: original
                    .exchange_timestamp_unix_nanos
                    .saturating_add(2_000),
                normalized_unix_nanos: normalized,
                fanout_enqueue_unix_nanos: Some(normalized.saturating_add(1_000)),
            },
            nic_timestamp_source: Some(NicTimestampSource::SocketSoftware),
            correction_flags: original.correction_flags,
            quality_flags: original.quality_flags,
            schema_version: original.schema_version,
            semantic_class: SemanticClass::OrderedDelta,
        },
        &payload,
    )
    .map_err(realtime_error)
}

fn canonical_enqueue_time(source: &axiusflow_application::ReplaySnapshot, index: usize) -> i64 {
    source.bars()[index]
        .provenance()
        .exchange_timestamp_unix_nanos
        .saturating_add(4_000)
}

fn install_canonical_snapshot(
    session: &mut MarketWebSocketSession,
    fixture: &BinaryMarketStreamFixture,
    snapshot: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<(), ConformanceHarnessError> {
    let projected = try_project_canonical_market_bar_snapshot(
        snapshot,
        source.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &projected,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    match session
        .accept_message(Message::binary(frame))
        .map_err(websocket_error)?
    {
        WebSocketMessageOutcome::BinaryAccepted { published: 1 } => Ok(()),
        outcome => Err(ConformanceHarnessError::WebSocket(format!(
            "canonical snapshot produced unexpected outcome {outcome:?}"
        ))),
    }
}

fn deliver_canonical_delta(
    session: &mut MarketWebSocketSession,
    fixture: &BinaryMarketStreamFixture,
    source: &axiusflow_application::ReplaySnapshot,
    previous_sequence: u64,
    event: &CanonicalMarketEvent,
) -> Result<MarketWebSocketPublication, ConformanceHarnessError> {
    let envelope = try_encode_canonical_market_bar_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        source.instrument(),
        source.bar_definition(),
        previous_sequence,
        event,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let outcome = session
        .accept_message(Message::binary(frame))
        .map_err(websocket_error)?;
    if outcome != (WebSocketMessageOutcome::BinaryAccepted { published: 1 }) {
        return Err(ConformanceHarnessError::WebSocket(format!(
            "canonical delta produced unexpected outcome {outcome:?}"
        )));
    }
    session
        .drain_publications(NonZeroUsize::MIN)
        .pop()
        .ok_or_else(|| {
            ConformanceHarnessError::WebSocket("missing canonical publication".to_string())
        })
}

fn invalid_canonical_bar_payload_is_rejected(
    snapshot: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let first = snapshot.events().first().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing canonical fixture event".to_string())
    })?;
    let malformed = CanonicalMarketEvent::try_new(first.header().clone(), &[0_u8; 8])
        .map_err(realtime_error)?;
    Ok(matches!(
        try_project_canonical_market_bar(&malformed, source.instrument(), source.bar_definition()),
        Err(CanonicalMarketBarProjectionError::InvalidPayloadLength { actual: 8 })
    ))
}

fn wire_publication_timestamp_mismatch_is_rejected(
    snapshot: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let projected = try_project_canonical_market_bar_snapshot(
        snapshot,
        source.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let mut envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &projected,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let Some(market_bar_stream_envelope::Update::Snapshot(wire_snapshot)) =
        envelope.update.as_mut()
    else {
        return Ok(false);
    };
    let Some(metadata) = wire_snapshot
        .bars
        .first_mut()
        .and_then(|bar| bar.header.as_mut())
        .and_then(|header| header.metadata.as_mut())
    else {
        return Ok(false);
    };
    metadata.publication_time_unix_nanos = metadata.publication_time_unix_nanos.saturating_add(1);
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    Ok(decoder.push(&frame).is_err())
}

fn run_direct_branch_overflow_recovery(
    fixture: &BinaryMarketStreamFixture,
    source_four: &axiusflow_application::ReplaySnapshot,
    source_five: &axiusflow_application::ReplaySnapshot,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let owner = PartitionOwner::try_new(7, "overflow_fixture_owner", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    let baseline = canonical_market_bar_snapshot_prefix(source_four, 1, 1, 2)?;
    partition
        .install_snapshot(&fence, &baseline)
        .map_err(realtime_error)?;
    let direct =
        BoundedEventBranch::try_new(queue_policy("overflow_direct", 1)).map_err(realtime_error)?;
    let durable =
        BoundedEventBranch::try_new(queue_policy("overflow_durable", 2)).map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence.clone(), direct, durable).map_err(realtime_error)?;
    for index in [2_usize, 3] {
        let event = canonical_market_bar_event(&source_four.bars()[index], source_four, 1)?;
        let accepted = accept_for_fanout(&mut partition, &fence, event)?;
        fanout
            .publish(
                &mut partition,
                &fence,
                accepted,
                canonical_enqueue_time(source_four, index),
            )
            .map_err(realtime_error)?;
    }
    let direct_overflow_latched = fanout
        .direct_metrics(canonical_enqueue_time(source_four, 3))
        .snapshot_required
        && fanout
            .pop_direct(canonical_enqueue_time(source_four, 3).saturating_add(10))
            .is_none();
    let recovery = canonical_market_bar_snapshot(source_four, 1, 2)?;
    partition
        .install_snapshot(&fence, &recovery)
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence, &recovery)
        .map_err(realtime_error)?;
    let healthy_durable_preserved = [3_u64, 4].into_iter().all(|expected| {
        fanout
            .pop_durable(canonical_enqueue_time(source_four, 3).saturating_add(10))
            .is_some_and(|(event, _)| event.header().source_sequence == expected)
    });
    let obsolete_delta_cleared = fanout
        .pop_direct(canonical_enqueue_time(source_four, 3))
        .is_none();
    let mut session = websocket_fixture_session(fixture, websocket_fixture_config(fixture, 4)?)?;
    install_canonical_snapshot(&mut session, fixture, &recovery, source_four)?;
    let _ = session.drain_publications(NonZeroUsize::MIN);
    let event_five = canonical_market_bar_event(&source_five.bars()[4], source_five, 1)?;
    let accepted_five = accept_for_fanout(&mut partition, &fence, event_five)?;
    fanout
        .publish(
            &mut partition,
            &fence,
            accepted_five,
            canonical_enqueue_time(source_five, 4),
        )
        .map_err(realtime_error)?;
    let (resumed, _) = fanout
        .pop_direct(canonical_enqueue_time(source_five, 4).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing resumed direct delta".to_string())
        })?;
    let publication =
        deliver_canonical_delta(&mut session, fixture, source_five, 4, resumed.event())?;
    Ok((
        direct_overflow_latched && healthy_durable_preserved,
        obsolete_delta_cleared
            && publication.generation.generation() == 3
            && publication.generation.sequence_range() == (1, 5),
    ))
}

fn realtime_error(error: impl fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::Realtime(error.to_string())
}

/// Result of deriving and verifying recovery snapshot integrity from immutable content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotIntegrityOutcome {
    CorruptionRejected,
}

/// Result proving stale-writer rejection, partial branch pressure visibility, and snapshot recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealtimeRecoveryReport {
    pub stale_writer_rejected: bool,
    pub direct_outcome: QueueOutcome,
    pub durable_outcome: QueueOutcome,
    pub durable_gap_visible: bool,
    pub recovered: bool,
    pub snapshot_integrity: SnapshotIntegrityOutcome,
}

/// Exercises fencing, branch independence, gap telemetry, and checksum-bound recovery.
///
/// # Errors
///
/// Returns an error when any real-time contract rejects the valid fixture scenario.
pub fn run_realtime_recovery_conformance() -> Result<RealtimeRecoveryReport, ConformanceHarnessError>
{
    let owner = PartitionOwner::try_new(7, "fixture_owner_1", 1)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let mut partition = FencedPartition::new(owner);
    let old_fence = partition.publication_fence();
    for sequence in [1, 2] {
        let event = fixture_canonical_event(sequence, 1, SemanticClass::OrderedDelta)?;
        if partition
            .accept(&old_fence, &event)
            .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?
            != PartitionDecision::Accepted
        {
            return Err(ConformanceHarnessError::Realtime(
                "contiguous event was not accepted".to_string(),
            ));
        }
    }

    partition
        .handoff(
            PartitionOwner::try_new(7, "fixture_owner_2", 2)
                .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?,
        )
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let active_fence = partition.publication_fence();
    let stale_writer_rejected = partition
        .accept(
            &old_fence,
            &fixture_canonical_event(3, 1, SemanticClass::OrderedDelta)?,
        )
        .is_err();

    let initial_snapshot = fixture_snapshot(2, 1, 4)?;
    if !corrupted_snapshot_is_rejected(&initial_snapshot)? {
        return Err(ConformanceHarnessError::Realtime(
            "canonical snapshot corruption was accepted".to_string(),
        ));
    }
    let snapshot_integrity = SnapshotIntegrityOutcome::CorruptionRejected;
    partition
        .install_snapshot(&active_fence, &initial_snapshot)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;

    let direct = BoundedEventBranch::try_new(queue_policy("direct", 2))
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let durable = BoundedEventBranch::try_new(queue_policy("durable", 1))
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let mut fanout = DirectDurableFanout::try_new(active_fence.clone(), direct, durable)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;

    let event_5 = fixture_canonical_event(5, 2, SemanticClass::OrderedDelta)?;
    let accepted_5 = accept_for_fanout(&mut partition, &active_fence, event_5)?;
    fanout
        .publish(&mut partition, &active_fence, accepted_5, 5_200)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;

    let event_6 = fixture_canonical_event(6, 2, SemanticClass::OrderedDelta)?;
    let accepted_6 = accept_for_fanout(&mut partition, &active_fence, event_6)?;
    let FanoutOutcome {
        direct: direct_outcome,
        durable: durable_outcome,
    } = fanout
        .publish(&mut partition, &active_fence, accepted_6, 6_200)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let durable_gap_visible = fanout.durable_metrics(12_000).last_gap.is_some();

    let recovery_snapshot = fixture_snapshot(2, 2, 6)?;
    partition
        .install_snapshot(&active_fence, &recovery_snapshot)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    fanout
        .recover_from_snapshot(&active_fence, &recovery_snapshot)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let recovered = !fanout.durable_metrics(12_000).snapshot_required;

    Ok(RealtimeRecoveryReport {
        stale_writer_rejected,
        direct_outcome,
        durable_outcome,
        durable_gap_visible,
        recovered,
        snapshot_integrity,
    })
}

fn queue_policy(name: &str, item_capacity: usize) -> QueuePolicy {
    QueuePolicy {
        name: name.to_string(),
        producer: "fixture_partition".to_string(),
        consumer: format!("fixture_{name}_consumer"),
        item_capacity: NonZeroUsize::new(item_capacity).unwrap_or(NonZeroUsize::MIN),
        byte_capacity: NonZeroUsize::new(4_096).unwrap_or(NonZeroUsize::MIN),
        semantic_class: SemanticClass::OrderedDelta,
        overflow_action: OverflowAction::RequestSnapshot,
        maximum_residence_nanos: NonZeroU64::new(10_000).unwrap_or(NonZeroU64::MIN),
        recovery: "install_verified_snapshot".to_string(),
        alert_threshold_items: NonZeroUsize::MIN,
    }
}

fn corrupted_snapshot_is_rejected(
    snapshot: &CanonicalSnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let mut events = snapshot.events().to_vec();
    let mut payload = events[0].payload().to_vec();
    payload[0] ^= 0xff;
    events[0] = CanonicalMarketEvent::try_new(events[0].header().clone(), &payload)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    Ok(CanonicalSnapshot::try_from_parts(snapshot.descriptor().clone(), events).is_err())
}

fn fixture_snapshot(
    ownership_epoch: u64,
    generation: u64,
    last_sequence: u64,
) -> Result<CanonicalSnapshot, ConformanceHarnessError> {
    let events = (1..=last_sequence)
        .map(|sequence| {
            fixture_canonical_event(sequence, ownership_epoch, SemanticClass::OrderedDelta)
        })
        .collect::<Result<Vec<_>, _>>()?;
    CanonicalSnapshot::try_from_events(7, ownership_epoch, generation, 1, events)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))
}

fn fixture_canonical_event(
    sequence: u64,
    ownership_epoch: u64,
    semantic_class: SemanticClass,
) -> Result<CanonicalMarketEvent, ConformanceHarnessError> {
    let receive = i64::try_from(sequence)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000);
    CanonicalMarketEvent::try_new(
        CanonicalEventHeader {
            event_id: format!("recovery_fixture_{ownership_epoch}_{sequence}"),
            event_time_unix_nanos: receive.saturating_sub(500),
            publication_time_unix_nanos: receive.saturating_add(300),
            producer: "fixture_recovery_decoder".to_string(),
            correlation_id: "fixture_recovery".to_string(),
            causation_id: String::new(),
            entitlement_revision: "fixture_entitlement_v1".to_string(),
            instrument_id: "instrument_fixture_axf_xnas".to_string(),
            venue_id: "xnas".to_string(),
            source_id: "fixture_provider".to_string(),
            series_identity: None,
            source_sequence: sequence,
            partition_id: 7,
            ownership_epoch,
            timestamps: CanonicalTimestamps {
                exchange_unix_nanos: receive.saturating_sub(500),
                provider_receive_unix_nanos: receive.saturating_sub(300),
                nic_receive_unix_nanos: Some(receive),
                axiusflow_receive_unix_nanos: receive,
                normalized_unix_nanos: receive.saturating_add(100),
                fanout_enqueue_unix_nanos: Some(receive.saturating_add(200)),
            },
            nic_timestamp_source: Some(NicTimestampSource::SocketSoftware),
            correction_flags: 0,
            quality_flags: 0,
            schema_version: 1,
            semantic_class,
        },
        &sequence.to_be_bytes(),
    )
    .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureDecodeError {
    TruncatedEthernetOrIp,
    UnsupportedEtherType,
    UnsupportedIpv4Header,
    UnsupportedIpProtocol(u8),
    InvalidProviderLength(usize),
    InvalidProviderMagic,
    ZeroSequence,
    CanonicalEvent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConformanceHarnessError {
    Driver(String),
    Realtime(String),
    MarketStream(String),
    WebSocket(String),
    MissingFrame(usize),
    ProfileDivergence,
}

impl fmt::Display for ConformanceHarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ingest conformance failed: {self:?}")
    }
}

impl Error for ConformanceHarnessError {}

/// Headless evidence for the bounded plain-loopback background-runtime-to-chart boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainLoopbackRuntimeChartConformance {
    passed_checks: u16,
}

impl PlainLoopbackRuntimeChartConformance {
    const REQUIRED_CHECKS: u16 = 0x1fff;

    #[must_use]
    pub const fn passed_checks(self) -> u16 {
        self.passed_checks
    }

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises one background loopback owner feeding the existing chart recovery bridge.
///
/// This proves bounded local worker channels, off-consumer-thread socket ownership,
/// update delivery, correlated resnapshot, and graceful shutdown. It does not prove
/// TLS, authentication, entitlement enforcement, provider or WAN connectivity,
/// desktop wiring, physical presentation, evented networking, or production capacity.
///
/// # Errors
///
/// Returns an error when the local fixture cannot complete a bounded transition.
pub fn run_plain_loopback_runtime_chart_conformance()
-> Result<PlainLoopbackRuntimeChartConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let capacity_restrictions = runtime_capacity_restrictions(&fixture)?;
    let exhaustion_ownership = runtime_exhaustion_ownership(&fixture)?;
    let synchronized_pressure = runtime_synchronized_pressure(&fixture)?;
    let neutral_edges = neutral_coordinator_edge_conformance()?;
    let neutral_coordinator = runtime_neutral_coordinator(&fixture)?;
    let recovered = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(websocket_error)?;
    let recovered = replay_snapshot_with_generation(&recovered, 3)?;
    let recovered_frame = runtime_recovery_snapshot_frame(&fixture, &recovered)?;
    let (runtime, server) = start_runtime_chart(&fixture, recovered_frame)?;
    runtime
        .try_send_command(PlainLoopbackRuntimeCommand::Connect)
        .map_err(websocket_error)?;
    let initial = receive_runtime_initial(&runtime)?;
    let initial_publications = initial.snapshot == fixture.baseline
        && initial.delta == fixture.delta
        && initial.snapshot_generation == (1, 2)
        && initial.delta_generation == (1, 3);
    let (mut bridge, chart_applied) = apply_runtime_initial_to_chart(&initial)?;

    bridge.mark_stream_invalid();
    let recovery_command = bridge
        .pending_resnapshot_request()
        .ok_or_else(|| websocket_error("chart bridge did not request runtime recovery"))?;
    let recovery_dispatched = dispatch_chart_recovery(&mut bridge, &runtime)?;
    server.release_recovery.send(()).map_err(websocket_error)?;
    let recovery = receive_runtime_recovery(&runtime, recovery_command)?;
    let snapshot = match &recovery.publication.update {
        ReplayStreamUpdate::Snapshot(snapshot) => snapshot,
        ReplayStreamUpdate::Delta(_) => {
            return Err(websocket_error(
                "runtime correlated a delta as a recovery snapshot",
            ));
        }
    };
    let stale_rejected = !bridge
        .install_recovery_snapshot(recovery_command.request_id.saturating_add(1), snapshot)
        .map_err(websocket_error)?;
    let correlated_installed = bridge
        .install_recovery_snapshot(recovery_command.request_id, snapshot)
        .map_err(websocket_error)?;
    let recovery_metrics = bridge.metrics();
    let chart_recovered = stale_rejected
        && correlated_installed
        && !bridge.requires_snapshot()
        && bridge.expected_sequence() == Some(5)
        && recovery_metrics.completed_recoveries == 1
        && !recovery_metrics.recovery_pending;
    let provenance_retained = recovery.publication.generation.sequence_range() == (1, 4)
        && snapshot.evidence().last_sequence == 4
        && snapshot.bars().last().is_some_and(|bar| {
            bar.provenance().source_sequence == 4
                && bar.provenance().entitlement_revision == "embedded_fixture_entitlement_v1"
        });

    runtime
        .try_send_command(PlainLoopbackRuntimeCommand::Shutdown)
        .map_err(websocket_error)?;
    server.release_shutdown.send(()).map_err(websocket_error)?;
    let stopped = receive_runtime_stopped(&runtime)?;
    runtime.join().map_err(websocket_error)?;
    let peer_closed = server
        .worker
        .join()
        .map_err(|_| websocket_error("runtime chart server panicked"))?
        .map_err(websocket_error)?;
    let checks = [
        capacity_restrictions,
        exhaustion_ownership,
        synchronized_pressure,
        neutral_edges,
        neutral_coordinator,
        initial.connected,
        initial_publications,
        chart_applied,
        recovery_dispatched,
        recovery.stream_invalid && recovery.connected,
        chart_recovered,
        provenance_retained,
        stopped && peer_closed,
    ];
    let passed_checks = checks
        .into_iter()
        .enumerate()
        .fold(0_u16, |bits, (index, passed)| {
            if passed {
                bits | (1_u16 << index)
            } else {
                bits
            }
        });
    Ok(PlainLoopbackRuntimeChartConformance { passed_checks })
}

fn start_runtime_chart(
    fixture: &BinaryMarketStreamFixture,
    recovered_frame: Vec<u8>,
) -> Result<(PlainLoopbackBackgroundRuntime, RuntimeChartServer), ConformanceHarnessError> {
    let server = spawn_runtime_chart_server(fixture, recovered_frame)?;
    let owner = plain_loopback_owner_with_timeout(
        fixture,
        server.endpoint.clone(),
        16,
        8,
        3,
        Duration::from_secs(3),
    )?;
    let runtime_config = PlainLoopbackRuntimeConfig::try_new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
        Duration::from_millis(5),
    )
    .map_err(websocket_error)?;
    let runtime = PlainLoopbackBackgroundRuntime::try_start(owner, runtime_config)
        .map_err(websocket_error)?;
    Ok((runtime, server))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScriptedMarketStreamPortError {
    CommandDisconnected,
    EventDisconnected,
}

impl fmt::Display for ScriptedMarketStreamPortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "scripted market stream port failed: {self:?}")
    }
}

impl Error for ScriptedMarketStreamPortError {}

struct ScriptedMarketStreamPort {
    commands: std::sync::mpsc::SyncSender<MarketStreamCommand>,
    events: std::sync::mpsc::Receiver<MarketStreamEvent>,
}

impl MarketStreamRuntimePort for ScriptedMarketStreamPort {
    type Error = ScriptedMarketStreamPortError;

    fn try_send_stream_command(
        &self,
        command: MarketStreamCommand,
    ) -> Result<MarketStreamCommandOffer, Self::Error> {
        match self.commands.try_send(command) {
            Ok(()) => Ok(MarketStreamCommandOffer::Accepted),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Ok(MarketStreamCommandOffer::Full),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err(ScriptedMarketStreamPortError::CommandDisconnected)
            }
        }
    }

    fn try_recv_stream_event(&self) -> Result<Option<MarketStreamEvent>, Self::Error> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(ScriptedMarketStreamPortError::EventDisconnected)
            }
        }
    }
}

struct ScriptedStreamPublications {
    baseline: MarketStreamPublication,
    delta: MarketStreamPublication,
    unrelated_delta: MarketStreamPublication,
    recovered: MarketStreamPublication,
    mismatched_rejected: bool,
}

fn scripted_stream_publications() -> Result<ScriptedStreamPublications, ConformanceHarnessError> {
    let source = EmbeddedReplaySource;
    let baseline = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 1 })
        .map_err(websocket_error)?;
    let delta = source
        .load_delta(1)
        .map_err(websocket_error)?
        .ok_or_else(|| websocket_error("missing scripted delta"))?;
    let recovered = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 3 })
        .map_err(websocket_error)?;
    let mut streaming_model =
        MarketBarClientModel::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    let baseline_publication = scripted_stream_publication(
        &mut streaming_model,
        ReplayStreamUpdate::Snapshot(baseline.clone()),
    )?;
    let delta_publication = scripted_stream_publication(
        &mut streaming_model,
        ReplayStreamUpdate::Delta(delta.clone()),
    )?;
    let delta_generation = delta_publication.generation();
    let (first_sequence, last_sequence) = delta_generation.sequence_range();
    let unrelated_generation = axiusflow_application::MarketGeneration::try_new(
        delta_generation.partition_id(),
        delta_generation.ownership_epoch(),
        99,
        first_sequence,
        last_sequence,
        delta_generation.items().to_vec(),
    )
    .map_err(websocket_error)?;
    let unrelated_delta = MarketStreamPublication::try_new(
        "scripted-market-stream".to_string(),
        ReplayStreamUpdate::Delta(delta),
        unrelated_generation,
    )
    .map_err(websocket_error)?;
    let mismatched_rejected = matches!(
        MarketStreamPublication::try_new(
            "scripted-market-stream".to_string(),
            ReplayStreamUpdate::Snapshot(baseline),
            delta_publication.generation().clone(),
        ),
        Err(MarketStreamPublicationError::UpdateGenerationMismatch(_))
    );
    let mut recovered_model =
        MarketBarClientModel::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    let recovered_publication = scripted_stream_publication(
        &mut recovered_model,
        ReplayStreamUpdate::Snapshot(recovered),
    )?;
    Ok(ScriptedStreamPublications {
        baseline: baseline_publication,
        delta: delta_publication,
        unrelated_delta,
        recovered: recovered_publication,
        mismatched_rejected,
    })
}

fn scripted_stream_publication(
    model: &mut MarketBarClientModel,
    update: ReplayStreamUpdate,
) -> Result<MarketStreamPublication, ConformanceHarnessError> {
    let generation = match model
        .apply_update(update.clone())
        .map_err(websocket_error)?
    {
        MarketBarModelOutcome::Published(generation) => generation,
        outcome => {
            return Err(websocket_error(format!(
                "scripted update did not publish a generation: {outcome:?}"
            )));
        }
    };
    MarketStreamPublication::try_new("scripted-market-stream".to_string(), update, generation)
        .map_err(websocket_error)
}

fn neutral_coordinator_edge_conformance() -> Result<bool, ConformanceHarnessError> {
    let publications = scripted_stream_publications()?;
    let (command_tx, command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator = ChartStreamCoordinator::new(NonZeroUsize::MIN);

    event_tx
        .try_send(MarketStreamEvent::Publication(Box::new(publications.delta)))
        .map_err(websocket_error)?;
    let prebaseline_delta_rejected = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::PublicationRejectedBeforeSnapshot { .. }
    ) && coordinator.bridge().is_none();
    event_tx
        .try_send(MarketStreamEvent::Publication(Box::new(
            publications.baseline,
        )))
        .map_err(websocket_error)?;
    let baseline_installed = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::InitialSnapshot {
            last_sequence: 1,
            ..
        }
    );
    event_tx
        .try_send(MarketStreamEvent::Publication(Box::new(
            publications.unrelated_delta,
        )))
        .map_err(websocket_error)?;
    let generation_discontinuity_rejected = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::PublicationGenerationDiscontinuity {
            expected_predecessor: Some(1),
            actual_predecessor: Some(98),
            generation: 99,
            last_sequence: 2,
        }
    );
    let discontinuity_did_not_mutate = coordinator.bridge().is_some_and(|bridge| {
        bridge.queued_update_count() == 0 && bridge.expected_sequence() == Some(2)
    }) && coordinator.metrics().publication_generation
        == Some(1);

    let recovery_bounded = scripted_coordinator_recovery(
        &mut coordinator,
        &port,
        &command_rx,
        &event_tx,
        publications.recovered,
    )?;

    drop(event_tx);
    let event_disconnected = matches!(
        coordinator.poll_once(&port),
        Err(
            axiusflow_chart_integration::ChartStreamCoordinatorError::Runtime(
                ScriptedMarketStreamPortError::EventDisconnected
            )
        )
    );
    drop(command_rx);
    let command_disconnected = matches!(
        coordinator.try_shutdown(&port),
        Err(ScriptedMarketStreamPortError::CommandDisconnected)
    );
    let metrics = coordinator.metrics();
    Ok(publications.mismatched_rejected
        && prebaseline_delta_rejected
        && baseline_installed
        && generation_discontinuity_rejected
        && discontinuity_did_not_mutate
        && recovery_bounded
        && event_disconnected
        && command_disconnected
        && metrics.publications_accepted == 1
        && metrics.publications_rejected == 2
        && metrics.recovery_snapshots_installed == 1
        && metrics.recovery_snapshots_rejected == 1)
}

fn scripted_coordinator_recovery(
    coordinator: &mut ChartStreamCoordinator,
    port: &ScriptedMarketStreamPort,
    command_rx: &std::sync::mpsc::Receiver<MarketStreamCommand>,
    event_tx: &std::sync::mpsc::SyncSender<MarketStreamEvent>,
    recovered: MarketStreamPublication,
) -> Result<bool, ConformanceHarnessError> {
    let invalidated = coordinator.mark_stream_invalid(ResnapshotReason::QueueOverflow);
    let request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("scripted coordinator did not request recovery"))?;
    let filler_accepted = port
        .try_send_stream_command(MarketStreamCommand::Connect)
        .map_err(websocket_error)?
        == MarketStreamCommandOffer::Accepted;
    let full_retryable = coordinator
        .try_dispatch_recovery(port)
        .map_err(websocket_error)?
        == ChartStreamRecoveryDispatch::Full
        && coordinator.bridge().is_some_and(|bridge| {
            let metrics = bridge.metrics();
            metrics.recovery_pending && !metrics.recovery_dispatched
        });
    let filler_drained = command_rx
        .try_recv()
        .is_ok_and(|command| command == MarketStreamCommand::Connect);
    let retry_accepted = coordinator
        .try_dispatch_recovery(port)
        .map_err(websocket_error)?
        == ChartStreamRecoveryDispatch::Accepted;
    let correlated_command = command_rx.try_recv().is_ok_and(|command| {
        command
            == MarketStreamCommand::Recover {
                request_id,
                reason: ResnapshotReason::QueueOverflow,
            }
    });

    event_tx
        .try_send(MarketStreamEvent::RecoverySnapshot {
            request_id: request_id.saturating_add(1),
            publication: Box::new(recovered.clone()),
        })
        .map_err(websocket_error)?;
    let uncorrelated_rejected = matches!(
        coordinator.poll_once(port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoverySnapshot {
            installed: false,
            ..
        }
    ) && coordinator
        .bridge()
        .is_some_and(ChartDataBridge::requires_snapshot);
    event_tx
        .try_send(MarketStreamEvent::RecoverySnapshot {
            request_id,
            publication: Box::new(recovered),
        })
        .map_err(websocket_error)?;
    let correlated_installed = matches!(
        coordinator.poll_once(port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoverySnapshot {
            installed: true,
            last_sequence: 3,
            ..
        }
    );
    Ok(invalidated
        && filler_accepted
        && full_retryable
        && filler_drained
        && retry_accepted
        && correlated_command
        && uncorrelated_rejected
        && correlated_installed)
}

fn runtime_neutral_coordinator(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let recovered = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(websocket_error)?;
    let recovered = replay_snapshot_with_generation(&recovered, 3)?;
    let recovered_frame = runtime_recovery_snapshot_frame(fixture, &recovered)?;
    let (runtime, server) = start_runtime_chart(fixture, recovered_frame)?;
    let mut coordinator =
        ChartStreamCoordinator::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    let connect_accepted = coordinator.try_connect(&runtime).map_err(websocket_error)?
        == MarketStreamCommandOffer::Accepted;
    let initial_applied = receive_neutral_coordinator_initial(&mut coordinator, &runtime)
        .map_err(|error| websocket_error(format!("neutral initial: {error}")))?;
    let recovered = recover_neutral_coordinator(&mut coordinator, &runtime, &server)
        .map_err(|error| websocket_error(format!("neutral recovery: {error}")))?;
    let stopped = stop_neutral_coordinator(&mut coordinator, &runtime, &server)
        .map_err(|error| websocket_error(format!("neutral stop: {error}")))?;
    runtime.join().map_err(websocket_error)?;
    let peer_closed = server
        .worker
        .join()
        .map_err(|_| websocket_error("neutral coordinator server panicked"))?
        .map_err(websocket_error)?;
    let metrics = coordinator.metrics();
    Ok(connect_accepted
        && initial_applied
        && recovered
        && stopped
        && peer_closed
        && metrics.bridge_installed
        && metrics.publication_generation == Some(3)
        && metrics.connected_events == 2
        && metrics.publications_accepted == 2
        && metrics.publications_rejected == 0
        && metrics.stream_invalidations == 1
        && metrics.recovery_snapshots_installed == 1
        && metrics.recovery_snapshots_rejected == 0
        && metrics.recovery_exhaustions == 0
        && metrics.reported_dropped_events == 0
        && metrics.stopped)
}

fn receive_neutral_coordinator_initial(
    coordinator: &mut ChartStreamCoordinator,
    runtime: &PlainLoopbackBackgroundRuntime,
) -> Result<bool, ConformanceHarnessError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut connected = false;
    let mut initial_snapshot = false;
    let mut delta_queued = false;
    while !(connected && initial_snapshot && delta_queued) {
        match poll_neutral_coordinator_before(coordinator, runtime, deadline)? {
            ChartStreamPollOutcome::Connected {
                connection_epoch: 1,
                attempt: 1,
            } => connected = true,
            ChartStreamPollOutcome::InitialSnapshot {
                generation: 1,
                first_sequence: 1,
                last_sequence: 2,
            } => initial_snapshot = true,
            ChartStreamPollOutcome::PublicationQueued {
                generation: 2,
                first_sequence: 1,
                last_sequence: 3,
            } => delta_queued = true,
            _ => {}
        }
    }
    let chart_applied = coordinator
        .drain_merged()
        .map_err(websocket_error)?
        .is_some()
        && coordinator.bridge().is_some_and(|bridge| {
            !bridge.requires_snapshot() && bridge.expected_sequence() == Some(4)
        });
    Ok(connected && initial_snapshot && delta_queued && chart_applied)
}

fn recover_neutral_coordinator(
    coordinator: &mut ChartStreamCoordinator,
    runtime: &PlainLoopbackBackgroundRuntime,
    server: &RuntimeChartServer,
) -> Result<bool, ConformanceHarnessError> {
    let invalidated = coordinator.mark_stream_invalid(ResnapshotReason::QueueOverflow);
    let recovery_request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("neutral coordinator did not request recovery"))?;
    let recovery_dispatched = coordinator
        .try_dispatch_recovery(runtime)
        .map_err(websocket_error)?
        == ChartStreamRecoveryDispatch::Accepted;
    let duplicate_dispatch_blocked = coordinator
        .try_dispatch_recovery(runtime)
        .map_err(websocket_error)?
        == ChartStreamRecoveryDispatch::NoPendingRecovery;
    server.release_recovery.send(()).map_err(websocket_error)?;

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut stream_invalid = false;
    let mut reconnected = false;
    let mut recovery_installed = false;
    while !recovery_installed {
        match poll_neutral_coordinator_before(coordinator, runtime, deadline)? {
            ChartStreamPollOutcome::StreamInvalid {
                reason: ResnapshotReason::QueueOverflow,
                dropped_events: 0,
                latched: true,
            } => stream_invalid = true,
            ChartStreamPollOutcome::Connected {
                connection_epoch: 2,
                attempt: 1,
            } => reconnected = true,
            ChartStreamPollOutcome::RecoverySnapshot {
                request_id,
                installed: true,
                generation: 3,
                last_sequence: 4,
            } if request_id == recovery_request_id => recovery_installed = true,
            ChartStreamPollOutcome::RecoveryExhausted { .. } => {
                return Err(websocket_error(
                    "neutral coordinator runtime exhausted during recovery",
                ));
            }
            _ => {}
        }
    }
    let recovered_bridge = coordinator.bridge().is_some_and(|bridge| {
        let metrics = bridge.metrics();
        !bridge.requires_snapshot()
            && bridge.expected_sequence() == Some(5)
            && metrics.completed_recoveries == 1
            && !metrics.recovery_pending
    });
    Ok(invalidated
        && recovery_dispatched
        && duplicate_dispatch_blocked
        && stream_invalid
        && reconnected
        && recovery_installed
        && recovered_bridge)
}

fn stop_neutral_coordinator(
    coordinator: &mut ChartStreamCoordinator,
    runtime: &PlainLoopbackBackgroundRuntime,
    server: &RuntimeChartServer,
) -> Result<bool, ConformanceHarnessError> {
    let shutdown_accepted = coordinator.try_shutdown(runtime).map_err(websocket_error)?
        == MarketStreamCommandOffer::Accepted;
    server.release_shutdown.send(()).map_err(websocket_error)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let ChartStreamPollOutcome::Stopped {
            graceful,
            dropped_events,
        } = poll_neutral_coordinator_before(coordinator, runtime, deadline)?
        {
            return Ok(shutdown_accepted && graceful && dropped_events == 0);
        }
    }
}

fn poll_neutral_coordinator_before(
    coordinator: &mut ChartStreamCoordinator,
    runtime: &PlainLoopbackBackgroundRuntime,
    deadline: std::time::Instant,
) -> Result<ChartStreamPollOutcome, ConformanceHarnessError> {
    loop {
        let outcome = coordinator.poll_once(runtime).map_err(websocket_error)?;
        if outcome != ChartStreamPollOutcome::Idle {
            return Ok(outcome);
        }
        if std::time::Instant::now() >= deadline {
            return Err(websocket_error("neutral coordinator event timed out"));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn runtime_capacity_restrictions(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let zero_wait = matches!(
        PlainLoopbackRuntimeConfig::try_new(NonZeroUsize::MIN, NonZeroUsize::MIN, Duration::ZERO,),
        Err(PlainLoopbackRuntimeConfigError::ZeroBackpressureWait)
    );
    let reservation = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    let address = reservation.local_addr().map_err(websocket_error)?;
    drop(reservation);
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let owner = plain_loopback_owner(fixture, endpoint, 16, 2)?;
    let config = PlainLoopbackRuntimeConfig::try_new(
        NonZeroUsize::MIN,
        NonZeroUsize::MIN,
        Duration::from_millis(1),
    )
    .map_err(websocket_error)?;
    let output_rejected = matches!(
        PlainLoopbackBackgroundRuntime::try_start(owner, config),
        Err(
            PlainLoopbackRuntimeStartError::EventCapacityBelowOwnerCapacity {
                runtime_event_capacity: 1,
                owner_event_capacity: 16
            }
        )
    );
    Ok(zero_wait && output_rejected)
}

fn runtime_exhaustion_ownership(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let reservation = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    let address = reservation.local_addr().map_err(websocket_error)?;
    drop(reservation);
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let owner =
        plain_loopback_owner_with_timeout(fixture, endpoint, 16, 8, 1, Duration::from_millis(100))?;
    let config = PlainLoopbackRuntimeConfig::try_new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
        Duration::from_millis(2),
    )
    .map_err(websocket_error)?;
    let runtime =
        PlainLoopbackBackgroundRuntime::try_start(owner, config).map_err(websocket_error)?;
    let request_id = 41;
    runtime
        .try_send_command(PlainLoopbackRuntimeCommand::Recover {
            request_id,
            reason: axiusflow_application::ResnapshotReason::SequenceGap,
        })
        .map_err(websocket_error)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let exhausted = loop {
        match recv_runtime_event_before(&runtime, deadline)? {
            PlainLoopbackRuntimeEvent::ReconnectExhausted {
                request_id: Some(actual),
                attempts: 1,
                ..
            } => break actual == request_id,
            PlainLoopbackRuntimeEvent::WorkerFailed(error) => {
                return Err(websocket_error(error));
            }
            _ => {}
        }
    };

    let listener = TcpListener::bind(address).map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let websocket_config = websocket_fixture_config(fixture, 8)?.websocket_config();
    let snapshot = fixture.snapshot_frame.clone();
    let (release_shutdown, await_shutdown) = std::sync::mpsc::sync_channel(1);
    let server = thread::Builder::new()
        .name("axiusflow-runtime-exhaustion-ownership".to_string())
        .spawn(move || -> Result<bool, String> {
            let stream = accept_runtime_connection(&listener, Duration::from_secs(3))?;
            configure_runtime_server_stream(&stream)?;
            let mut socket = accept_with_config(stream, Some(websocket_config))
                .map_err(|error| error.to_string())?;
            socket
                .send(Message::binary(snapshot))
                .map_err(|error| error.to_string())?;
            await_shutdown
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| error.to_string())?;
            socket
                .send(Message::Ping(Vec::new().into()))
                .map_err(|error| error.to_string())?;
            socket.flush().map_err(|error| error.to_string())?;
            Ok(observe_runtime_close(&mut socket))
        })
        .map_err(websocket_error)?;
    runtime
        .try_send_command(PlainLoopbackRuntimeCommand::Connect)
        .map_err(websocket_error)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let ordinary_snapshot = loop {
        match recv_runtime_event_before(&runtime, deadline)? {
            PlainLoopbackRuntimeEvent::Publication(publication)
                if matches!(&publication.update, ReplayStreamUpdate::Snapshot(_)) =>
            {
                break true;
            }
            PlainLoopbackRuntimeEvent::RecoverySnapshot { .. } => break false,
            PlainLoopbackRuntimeEvent::WorkerFailed(error) => {
                return Err(websocket_error(error));
            }
            _ => {}
        }
    };
    runtime
        .try_send_command(PlainLoopbackRuntimeCommand::Shutdown)
        .map_err(websocket_error)?;
    release_shutdown.send(()).map_err(websocket_error)?;
    let stopped = receive_runtime_stopped(&runtime)?;
    runtime.join().map_err(websocket_error)?;
    let peer_closed = server
        .join()
        .map_err(|_| websocket_error("runtime exhaustion server panicked"))?
        .map_err(websocket_error)?;
    Ok(exhausted && ordinary_snapshot && stopped && peer_closed)
}

struct RuntimePressureServer {
    endpoint: PlainLoopbackWebSocketEndpoint,
    pong_seen: std::sync::mpsc::Receiver<()>,
    release_snapshot: std::sync::mpsc::SyncSender<()>,
    snapshot_sent: std::sync::mpsc::Receiver<()>,
    worker: thread::JoinHandle<Result<bool, String>>,
}

fn runtime_synchronized_pressure(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let server = spawn_runtime_pressure_server(fixture)?;
    let owner = plain_loopback_owner_with_timeout(
        fixture,
        server.endpoint.clone(),
        1,
        1,
        1,
        Duration::from_secs(3),
    )?;
    let config = PlainLoopbackRuntimeConfig::try_new(
        NonZeroUsize::MIN,
        NonZeroUsize::MIN,
        Duration::from_millis(2),
    )
    .map_err(websocket_error)?;
    let runtime =
        PlainLoopbackBackgroundRuntime::try_start(owner, config).map_err(websocket_error)?;
    runtime
        .try_send_command(PlainLoopbackRuntimeCommand::Connect)
        .map_err(websocket_error)?;
    server
        .pong_seen
        .recv_timeout(Duration::from_secs(3))
        .map_err(websocket_error)?;
    let recovery = MarketStreamCommand::Recover {
        request_id: 7,
        reason: ResnapshotReason::QueueOverflow,
    };
    let first_recovery_accepted = runtime
        .try_send_stream_command(recovery)
        .map_err(websocket_error)?
        == MarketStreamCommandOffer::Accepted;
    let command_bound = first_recovery_accepted
        && runtime
            .try_send_stream_command(recovery)
            .is_ok_and(|offer| offer == MarketStreamCommandOffer::Full);
    server.release_snapshot.send(()).map_err(websocket_error)?;
    server
        .snapshot_sent
        .recv_timeout(Duration::from_secs(3))
        .map_err(websocket_error)?;

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let connected_waited = matches!(
        recv_runtime_event_before(&runtime, deadline)?,
        PlainLoopbackRuntimeEvent::Connected {
            connection_epoch: 1,
            attempt: 1
        }
    );
    let control_waited = matches!(
        recv_runtime_event_before(&runtime, deadline)?,
        PlainLoopbackRuntimeEvent::Control(
            axiusflow_stream_websocket_adapter::WebSocketControlSignal::Ping
        )
    );
    let peer_reset = server
        .worker
        .join()
        .map_err(|_| websocket_error("runtime pressure server panicked"))?
        .map_err(websocket_error)?;
    let (invalidated, publication_observed) = receive_runtime_pressure_outcome(&runtime, deadline)?;
    runtime
        .try_send_command(PlainLoopbackRuntimeCommand::Shutdown)
        .map_err(websocket_error)?;
    let stopped = receive_runtime_stopped(&runtime)?;
    runtime.join().map_err(websocket_error)?;
    let passed = command_bound
        && connected_waited
        && control_waited
        && peer_reset
        && invalidated
        && !publication_observed
        && stopped;
    if !passed {
        return Err(websocket_error(format!(
            "runtime pressure evidence failed: command_bound={command_bound} connected_waited={connected_waited} control_waited={control_waited} peer_reset={peer_reset} invalidated={invalidated} publication_observed={publication_observed} stopped={stopped}"
        )));
    }
    Ok(true)
}

fn receive_runtime_pressure_outcome(
    runtime: &PlainLoopbackBackgroundRuntime,
    deadline: std::time::Instant,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let mut invalidated = false;
    let mut publication_observed = false;
    loop {
        match recv_runtime_event_before(runtime, deadline)? {
            PlainLoopbackRuntimeEvent::StreamInvalid {
                reason:
                    WebSocketRecoveryReason::ModelRecovery(
                        axiusflow_application::ResnapshotReason::QueueOverflow,
                    ),
                ..
            } => invalidated = true,
            PlainLoopbackRuntimeEvent::ReconnectExhausted {
                request_id: Some(7),
                attempts: 1,
                ..
            } => return Ok((invalidated, publication_observed)),
            PlainLoopbackRuntimeEvent::Publication(_)
            | PlainLoopbackRuntimeEvent::RecoverySnapshot { .. } => publication_observed = true,
            PlainLoopbackRuntimeEvent::WorkerFailed(error) => {
                return Err(websocket_error(error));
            }
            _ => {}
        }
    }
}

fn spawn_runtime_pressure_server(
    fixture: &BinaryMarketStreamFixture,
) -> Result<RuntimePressureServer, ConformanceHarnessError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let address = listener.local_addr().map_err(websocket_error)?;
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let websocket_config = websocket_fixture_config(fixture, 1)?.websocket_config();
    let snapshot = fixture.snapshot_frame.clone();
    let (pong_seen_tx, pong_seen) = std::sync::mpsc::sync_channel(1);
    let (release_snapshot, await_snapshot) = std::sync::mpsc::sync_channel(1);
    let (snapshot_sent_tx, snapshot_sent) = std::sync::mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("axiusflow-runtime-pressure-barrier".to_string())
        .spawn(move || -> Result<bool, String> {
            let stream = accept_runtime_connection(&listener, Duration::from_secs(3))?;
            drop(listener);
            configure_runtime_server_stream(&stream)?;
            let mut socket = accept_with_config(stream, Some(websocket_config))
                .map_err(|error| error.to_string())?;
            socket
                .send(Message::Ping(Vec::new().into()))
                .map_err(|error| error.to_string())?;
            let pong = matches!(
                socket.read().map_err(|error| error.to_string())?,
                Message::Pong(_)
            );
            pong_seen_tx.send(()).map_err(|error| error.to_string())?;
            await_snapshot
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| error.to_string())?;
            socket
                .send(Message::binary(snapshot))
                .map_err(|error| error.to_string())?;
            snapshot_sent_tx
                .send(())
                .map_err(|error| error.to_string())?;
            Ok(pong && observe_runtime_reset(&mut socket))
        })
        .map_err(websocket_error)?;
    Ok(RuntimePressureServer {
        endpoint,
        pong_seen,
        release_snapshot,
        snapshot_sent,
        worker,
    })
}

fn runtime_recovery_snapshot_frame(
    fixture: &BinaryMarketStreamFixture,
    recovered: &axiusflow_application::ReplaySnapshot,
) -> Result<Vec<u8>, ConformanceHarnessError> {
    let envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        recovered,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)
}

struct RuntimeChartServer {
    endpoint: PlainLoopbackWebSocketEndpoint,
    release_recovery: std::sync::mpsc::SyncSender<()>,
    release_shutdown: std::sync::mpsc::SyncSender<()>,
    worker: thread::JoinHandle<Result<bool, String>>,
}

fn spawn_runtime_chart_server(
    fixture: &BinaryMarketStreamFixture,
    recovered_frame: Vec<u8>,
) -> Result<RuntimeChartServer, ConformanceHarnessError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(websocket_error)?;
    listener.set_nonblocking(true).map_err(websocket_error)?;
    let address = listener.local_addr().map_err(websocket_error)?;
    let endpoint = PlainLoopbackWebSocketEndpoint::try_new(&format!("ws://{address}/market-bars"))
        .map_err(websocket_error)?;
    let websocket_config = websocket_fixture_config(fixture, 8)?.websocket_config();
    let initial_payload = fixture.joined_frames.clone();
    let (release_recovery, await_recovery) = std::sync::mpsc::sync_channel(1);
    let (release_shutdown, await_shutdown) = std::sync::mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("axiusflow-runtime-chart-loopback".to_string())
        .spawn(move || {
            serve_runtime_chart_connections(
                &listener,
                websocket_config,
                initial_payload,
                recovered_frame,
                &await_recovery,
                &await_shutdown,
            )
        })
        .map_err(websocket_error)?;
    Ok(RuntimeChartServer {
        endpoint,
        release_recovery,
        release_shutdown,
        worker,
    })
}

fn serve_runtime_chart_connections(
    listener: &TcpListener,
    websocket_config: tungstenite::protocol::WebSocketConfig,
    initial_payload: Vec<u8>,
    recovered_frame: Vec<u8>,
    await_recovery: &std::sync::mpsc::Receiver<()>,
    await_shutdown: &std::sync::mpsc::Receiver<()>,
) -> Result<bool, String> {
    let first_stream = accept_runtime_connection(listener, Duration::from_secs(3))?;
    configure_runtime_server_stream(&first_stream)?;
    let mut first = accept_with_config(first_stream, Some(websocket_config))
        .map_err(|error| error.to_string())?;
    first
        .send(Message::binary(initial_payload))
        .map_err(|error| error.to_string())?;
    await_recovery
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| error.to_string())?;
    first
        .send(Message::Ping(Vec::new().into()))
        .map_err(|error| error.to_string())?;
    first.flush().map_err(|error| error.to_string())?;
    let first_reset = observe_runtime_reset(&mut first);

    let second_stream = accept_runtime_connection(listener, Duration::from_secs(3))?;
    configure_runtime_server_stream(&second_stream)?;
    let mut second = accept_with_config(second_stream, Some(websocket_config))
        .map_err(|error| error.to_string())?;
    second
        .send(Message::binary(recovered_frame))
        .map_err(|error| error.to_string())?;
    await_shutdown
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| error.to_string())?;
    second
        .send(Message::Ping(Vec::new().into()))
        .map_err(|error| error.to_string())?;
    second.flush().map_err(|error| error.to_string())?;
    Ok(first_reset && observe_runtime_close(&mut second))
}

fn accept_runtime_connection(
    listener: &TcpListener,
    timeout: Duration,
) -> Result<std::net::TcpStream, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("runtime chart server accept timed out".to_string());
                }
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn configure_runtime_server_stream(stream: &std::net::TcpStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())
}

fn observe_runtime_reset(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) -> bool {
    for _ in 0..4 {
        match socket.read() {
            Ok(Message::Close(_)) | Err(_) => return true,
            Ok(_) => {}
        }
    }
    false
}

fn observe_runtime_close(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) -> bool {
    for _ in 0..4 {
        match socket.read() {
            Ok(Message::Close(_)) => {
                let _ = socket.flush();
                return true;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

struct RuntimeInitialEvidence {
    connected: bool,
    snapshot: axiusflow_application::ReplaySnapshot,
    delta: axiusflow_protocols::StreamDelta<axiusflow_application::ProvenancedMarketBar>,
    snapshot_generation: (u64, u64),
    delta_generation: (u64, u64),
}

fn receive_runtime_initial(
    runtime: &PlainLoopbackBackgroundRuntime,
) -> Result<RuntimeInitialEvidence, ConformanceHarnessError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut connected = false;
    let mut snapshot = None;
    let mut delta = None;
    let mut snapshot_generation = None;
    let mut delta_generation = None;
    while snapshot.is_none() || delta.is_none() {
        match recv_runtime_event_before(runtime, deadline)? {
            PlainLoopbackRuntimeEvent::Connected {
                connection_epoch: 1,
                attempt: 1,
            } => connected = true,
            PlainLoopbackRuntimeEvent::Publication(publication) => match publication.update {
                ReplayStreamUpdate::Snapshot(update) => {
                    snapshot_generation = Some(publication.generation.sequence_range());
                    snapshot = Some(update);
                }
                ReplayStreamUpdate::Delta(update) => {
                    delta_generation = Some(publication.generation.sequence_range());
                    delta = Some(update);
                }
            },
            PlainLoopbackRuntimeEvent::WorkerFailed(error) => {
                return Err(websocket_error(error));
            }
            _ => {}
        }
    }
    Ok(RuntimeInitialEvidence {
        connected,
        snapshot: snapshot.ok_or_else(|| websocket_error("missing runtime snapshot"))?,
        delta: delta.ok_or_else(|| websocket_error("missing runtime delta"))?,
        snapshot_generation: snapshot_generation
            .ok_or_else(|| websocket_error("missing snapshot generation"))?,
        delta_generation: delta_generation
            .ok_or_else(|| websocket_error("missing delta generation"))?,
    })
}

fn apply_runtime_initial_to_chart(
    initial: &RuntimeInitialEvidence,
) -> Result<(ChartDataBridge, bool), ConformanceHarnessError> {
    let mut bridge = ChartDataBridge::try_new(
        NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
        &initial.snapshot,
    )
    .map_err(websocket_error)?;
    let pushed = bridge
        .try_push(ReplayStreamUpdate::Delta(initial.delta.clone()))
        .is_ok();
    let drained = bridge.drain_merged().map_err(websocket_error)?.is_some();
    let applied =
        pushed && drained && !bridge.requires_snapshot() && bridge.expected_sequence() == Some(4);
    Ok((bridge, applied))
}

fn dispatch_chart_recovery(
    bridge: &mut ChartDataBridge,
    runtime: &PlainLoopbackBackgroundRuntime,
) -> Result<bool, ConformanceHarnessError> {
    bridge
        .try_dispatch_recovery(|ReplayRecoveryCommand { request_id, reason }| {
            runtime.try_send_command(PlainLoopbackRuntimeCommand::Recover { request_id, reason })
        })
        .map_err(websocket_error)
}

struct RuntimeRecoveryEvidence {
    stream_invalid: bool,
    connected: bool,
    publication: MarketWebSocketPublication,
}

fn receive_runtime_recovery(
    runtime: &PlainLoopbackBackgroundRuntime,
    command: ReplayRecoveryCommand,
) -> Result<RuntimeRecoveryEvidence, ConformanceHarnessError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut stream_invalid = false;
    let mut connected = false;
    loop {
        match recv_runtime_event_before(runtime, deadline)? {
            PlainLoopbackRuntimeEvent::StreamInvalid {
                reason: WebSocketRecoveryReason::ModelRecovery(reason),
                ..
            } if reason == command.reason => stream_invalid = true,
            PlainLoopbackRuntimeEvent::Connected {
                connection_epoch: 2,
                attempt: 1,
            } => connected = true,
            PlainLoopbackRuntimeEvent::RecoverySnapshot {
                request_id,
                publication,
            } if request_id == command.request_id => {
                return Ok(RuntimeRecoveryEvidence {
                    stream_invalid,
                    connected,
                    publication: *publication,
                });
            }
            PlainLoopbackRuntimeEvent::WorkerFailed(error) => {
                return Err(websocket_error(error));
            }
            PlainLoopbackRuntimeEvent::ReconnectExhausted { .. } => {
                return Err(websocket_error("runtime exhausted during chart recovery"));
            }
            _ => {}
        }
    }
}

fn receive_runtime_stopped(
    runtime: &PlainLoopbackBackgroundRuntime,
) -> Result<bool, ConformanceHarnessError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match recv_runtime_event_before(runtime, deadline)? {
            PlainLoopbackRuntimeEvent::Stopped {
                close_handshake_complete,
                dropped_events,
            } => return Ok(close_handshake_complete && dropped_events == 0),
            PlainLoopbackRuntimeEvent::WorkerFailed(error) => {
                return Err(websocket_error(error));
            }
            _ => {}
        }
    }
}

fn recv_runtime_event_before(
    runtime: &PlainLoopbackBackgroundRuntime,
    deadline: std::time::Instant,
) -> Result<PlainLoopbackRuntimeEvent, ConformanceHarnessError> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(websocket_error("plain-loopback runtime event timed out"));
    }
    runtime
        .recv_event_timeout(remaining)
        .map_err(websocket_error)
}

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
