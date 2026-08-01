//! Bounded loopback WebSocket transport evidence.
//!
//! This module owns the loopback WebSocket session fixture, control-frame delivery,
//! fault and reconnect classification, and bounded queue overflow behavior. The session
//! and config builders are crate-visible because the plain-loopback lifecycle scenarios
//! reuse the same fixture endpoint.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, binary_market_stream_fixture,
    replay_snapshot_with_generation,
};
use crate::harness_error::{ConformanceHarnessError, market_stream_error, websocket_error};
use crate::loopback_fixture::{
    accept_runtime_connection, configure_runtime_server_stream, websocket_fixture_config,
    websocket_fixture_session,
};
use axiusflow_application::ReplayStreamUpdate;
use axiusflow_market_protocol_adapter::{
    encode_market_bar_stream_frame, try_encode_replay_snapshot_envelope,
};
use axiusflow_stream_websocket_adapter::{
    MarketWebSocketConfig, MarketWebSocketError, MarketWebSocketPublication, MarketWebSocketState,
    WebSocketMessageOutcome, WebSocketRecoveryReason,
};
use std::{net::TcpListener, num::NonZeroUsize, thread, time::Duration};
use tungstenite::{Message, accept_with_config, client::connect_with_config};

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

pub(crate) fn projected_websocket_updates_match(
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
