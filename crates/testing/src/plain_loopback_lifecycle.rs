//! Adapter-owned plain-loopback connection lifecycle evidence.
//!
//! This module owns multi-connection lifecycle stepping, bounded event-queue overflow,
//! reconnect-budget exhaustion, snapshot reset, and silent-peer timeout behavior for the
//! plain-loopback WebSocket owner. It reuses the shared loopback session fixture rather
//! than constructing its own transport configuration.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, binary_market_stream_fixture,
    replay_snapshot_with_generation,
};
use crate::harness_error::{ConformanceHarnessError, market_stream_error, websocket_error};
use crate::loopback_fixture::{
    accept_runtime_connection, configure_runtime_server_stream, plain_loopback_owner,
    plain_loopback_owner_with_timeout, websocket_fixture_config, websocket_fixture_session,
};
use crate::websocket_loopback::projected_websocket_updates_match;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarReplayPort, ReplayStreamUpdate,
};
use axiusflow_market_protocol_adapter::{
    encode_market_bar_stream_frame, try_encode_replay_snapshot_envelope,
};
use axiusflow_stream_websocket_adapter::{
    PlainLoopbackCommand, PlainLoopbackLifecycleConfig, PlainLoopbackLifecycleError,
    PlainLoopbackLifecycleEvent, PlainLoopbackLifecycleState, PlainLoopbackLifecycleStep,
    PlainLoopbackMarketWebSocketOwner, PlainLoopbackWebSocketEndpoint, WebSocketRecoveryReason,
};
use std::{net::TcpListener, num::NonZeroUsize, thread, time::Duration};
use tungstenite::{Message, accept_with_config};

/// Headless evidence for adapter-owned plain-loopback connection lifecycle semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainLoopbackLifecycleConformance {
    passed_checks: u16,
}

impl PlainLoopbackLifecycleConformance {
    const REQUIRED_CHECKS: u16 = 0x1fff;
    const EVENT_OVERFLOW_RECOVERED: u16 = 1 << 7;
    const RECONNECT_EXHAUSTION_AND_RECOVERY: u16 = 0x0f << 8;
    const SILENT_PEER_TIMEOUT: u16 = 1 << 12;

    #[must_use]
    pub const fn passed_checks(self) -> u16 {
        self.passed_checks
    }

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }

    /// Reports whether lifecycle event pressure collapsed to one recovery
    /// event while preserving the last accepted generation.
    #[must_use]
    pub const fn event_overflow_recovered(self) -> bool {
        self.passed_checks & Self::EVENT_OVERFLOW_RECOVERED != 0
    }

    /// Reports whether bounded reconnect exhaustion, reset after a covering
    /// snapshot, a fresh retry budget, and terminal event pressure all passed.
    #[must_use]
    pub const fn bounded_reconnect_recovered(self) -> bool {
        self.passed_checks & Self::RECONNECT_EXHAUSTION_AND_RECOVERY
            == Self::RECONNECT_EXHAUSTION_AND_RECOVERY
    }

    /// Reports whether a connected peer that emitted no message crossed the
    /// exact silence deadline and entered recovery.
    #[must_use]
    pub const fn silent_peer_timeout_recovered(self) -> bool {
        self.passed_checks & Self::SILENT_PEER_TIMEOUT != 0
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

#[cfg(test)]
mod tests {
    use super::PlainLoopbackLifecycleConformance;

    #[test]
    fn named_resilience_evidence_uses_exact_checks() {
        let complete = PlainLoopbackLifecycleConformance {
            passed_checks: 0x1fff,
        };
        assert!(complete.event_overflow_recovered());
        assert!(complete.bounded_reconnect_recovered());
        assert!(complete.silent_peer_timeout_recovered());

        let incomplete = PlainLoopbackLifecycleConformance {
            passed_checks: (1 << 7) | (1 << 12),
        };
        assert!(incomplete.event_overflow_recovered());
        assert!(!incomplete.bounded_reconnect_recovered());
        assert!(incomplete.silent_peer_timeout_recovered());
    }
}
