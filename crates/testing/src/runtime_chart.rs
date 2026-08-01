//! Bounded plain-loopback background-runtime-to-chart evidence.
//!
//! This module owns the scripted market-stream runtime port, the neutral chart stream
//! coordinator scenarios, runtime capacity and ownership restrictions, synchronized
//! back-pressure, and recovery dispatch. It also owns the shared loopback server accept
//! and stream-configuration helpers used by the other WebSocket scenario modules.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, binary_market_stream_fixture,
    replay_snapshot_with_generation,
};
use crate::harness_error::{ConformanceHarnessError, market_stream_error, websocket_error};
use crate::loopback_fixture::{
    accept_runtime_connection, configure_runtime_server_stream, plain_loopback_owner,
    plain_loopback_owner_with_timeout, websocket_fixture_config,
};
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketBarReplayPort, MarketStreamCommand, MarketStreamCommandOffer, MarketStreamEvent,
    MarketStreamPublication, MarketStreamPublicationError, MarketStreamRuntimePort,
    ReplayStreamUpdate, ResnapshotReason,
};
use axiusflow_chart_integration::{
    ChartDataBridge, ChartStreamCoordinator, ChartStreamPollOutcome, ChartStreamRecoveryDispatch,
    ReplayRecoveryCommand,
};
use axiusflow_market_protocol_adapter::{
    encode_market_bar_stream_frame, try_encode_replay_snapshot_envelope,
};
use axiusflow_stream_websocket_adapter::{
    MarketWebSocketPublication, PlainLoopbackBackgroundRuntime, PlainLoopbackRuntimeCommand,
    PlainLoopbackRuntimeConfig, PlainLoopbackRuntimeConfigError, PlainLoopbackRuntimeEvent,
    PlainLoopbackRuntimeStartError, PlainLoopbackWebSocketEndpoint, WebSocketRecoveryReason,
};
use core::fmt;
use std::{error::Error, net::TcpListener, num::NonZeroUsize, thread, time::Duration};
use tungstenite::{Message, accept_with_config};

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
