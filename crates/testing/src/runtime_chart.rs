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
    MarketStreamPublication, MarketStreamPublicationError, MarketStreamRuntimePort, ReplaySnapshot,
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
    queued_tail_delta: MarketStreamPublication,
    unrelated_delta: MarketStreamPublication,
    fallback_delta: MarketStreamPublication,
    fallback_tail_delta: MarketStreamPublication,
    fallback_partial: MarketStreamPublication,
    fallback_next_delta: MarketStreamPublication,
    fallback_stale: MarketStreamPublication,
    fallback_older: MarketStreamPublication,
    fallback_new_epoch: MarketStreamPublication,
    fallback_new_series: MarketStreamPublication,
    recovered: MarketStreamPublication,
    mismatched_rejected: bool,
}

struct ScriptedFallbackPublications {
    queued_tail_delta: MarketStreamPublication,
    delta: MarketStreamPublication,
    tail_delta: MarketStreamPublication,
    next_delta: MarketStreamPublication,
    partial: MarketStreamPublication,
    stale: MarketStreamPublication,
    older: MarketStreamPublication,
    new_epoch: MarketStreamPublication,
    new_series: MarketStreamPublication,
    recovered: MarketStreamPublication,
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
            ReplayStreamUpdate::Snapshot(baseline.clone()),
            delta_publication.generation().clone(),
        ),
        Err(MarketStreamPublicationError::UpdateGenerationMismatch(_))
    );
    let fallback = scripted_fallback_publications(&baseline, &mut streaming_model)?;
    Ok(ScriptedStreamPublications {
        baseline: baseline_publication,
        delta: delta_publication,
        queued_tail_delta: fallback.queued_tail_delta,
        unrelated_delta,
        fallback_delta: fallback.delta,
        fallback_tail_delta: fallback.tail_delta,
        fallback_partial: fallback.partial,
        fallback_next_delta: fallback.next_delta,
        fallback_stale: fallback.stale,
        fallback_older: fallback.older,
        fallback_new_epoch: fallback.new_epoch,
        fallback_new_series: fallback.new_series,
        recovered: fallback.recovered,
        mismatched_rejected,
    })
}

fn scripted_fallback_publications(
    baseline: &ReplaySnapshot,
    streaming_model: &mut MarketBarClientModel,
) -> Result<ScriptedFallbackPublications, ConformanceHarnessError> {
    let source = EmbeddedReplaySource;
    let recovered = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 3 })
        .map_err(websocket_error)?;
    let recovered = replay_snapshot_with_generation(&recovered, 3)?;
    let older = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .map_err(websocket_error)?;
    let older = replay_snapshot_with_generation(&older, 2)?;
    let partial = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(websocket_error)?;
    let partial = replay_snapshot_with_generation(&partial, 4)?;
    let new_epoch = replay_snapshot_with_ownership_epoch(
        baseline,
        baseline.evidence().ownership_epoch.saturating_add(1),
    )?;
    let new_series = replay_snapshot_with_bar_definition_id(baseline, "fallback-series")?;
    let queued_tail_delta = source
        .load_delta(2)
        .map_err(websocket_error)?
        .ok_or_else(|| websocket_error("missing queued fallback tail delta"))?;
    let delta = source
        .load_delta(3)
        .map_err(websocket_error)?
        .ok_or_else(|| websocket_error("missing fallback delta"))?;
    let tail_delta = source
        .load_delta(4)
        .map_err(websocket_error)?
        .ok_or_else(|| websocket_error("missing fallback tail delta"))?;
    let next_delta = source
        .load_delta(5)
        .map_err(websocket_error)?
        .ok_or_else(|| websocket_error("missing fallback next delta"))?;
    let model_capacity = NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN);
    let mut stale_model = MarketBarClientModel::new(model_capacity);
    let mut older_model = MarketBarClientModel::new(model_capacity);
    let mut partial_model = MarketBarClientModel::new(model_capacity);
    let mut new_epoch_model = MarketBarClientModel::new(model_capacity);
    let mut new_series_model = MarketBarClientModel::new(model_capacity);
    let stale = scripted_stream_publication(
        &mut stale_model,
        ReplayStreamUpdate::Snapshot(baseline.clone()),
    )?;
    let older = scripted_stream_publication(&mut older_model, ReplayStreamUpdate::Snapshot(older))?;
    let partial =
        scripted_stream_publication(&mut partial_model, ReplayStreamUpdate::Snapshot(partial))?;
    let new_epoch = scripted_stream_publication(
        &mut new_epoch_model,
        ReplayStreamUpdate::Snapshot(new_epoch),
    )?;
    let new_series = scripted_stream_publication(
        &mut new_series_model,
        ReplayStreamUpdate::Snapshot(new_series),
    )?;
    let recovered_model = &mut MarketBarClientModel::new(model_capacity);
    let recovered =
        scripted_stream_publication(recovered_model, ReplayStreamUpdate::Snapshot(recovered))?;
    let queued_tail_delta = scripted_stream_publication(
        streaming_model,
        ReplayStreamUpdate::Delta(queued_tail_delta),
    )?;
    let delta = scripted_stream_publication(streaming_model, ReplayStreamUpdate::Delta(delta))?;
    let tail_delta =
        scripted_stream_publication(streaming_model, ReplayStreamUpdate::Delta(tail_delta))?;
    let next_delta =
        scripted_stream_publication(streaming_model, ReplayStreamUpdate::Delta(next_delta))?;
    Ok(ScriptedFallbackPublications {
        queued_tail_delta,
        delta,
        tail_delta,
        next_delta,
        partial,
        stale,
        older,
        new_epoch,
        new_series,
        recovered,
    })
}

fn replay_snapshot_with_ownership_epoch(
    snapshot: &ReplaySnapshot,
    ownership_epoch: u64,
) -> Result<ReplaySnapshot, ConformanceHarnessError> {
    let bars = snapshot
        .bars()
        .iter()
        .cloned()
        .map(|item| {
            let (bar, mut provenance) = item.into_parts();
            provenance.ownership_epoch = ownership_epoch;
            axiusflow_application::Provenanced::new(bar, provenance)
        })
        .collect::<Vec<_>>();
    let mut evidence = snapshot.evidence().clone();
    evidence.ownership_epoch = ownership_epoch;
    evidence.checksum = axiusflow_protocols::compute_market_snapshot_checksum(
        &evidence,
        axiusflow_protocols::MarketSnapshotIdentityRef {
            instrument_id: snapshot.instrument().instrument_id.as_str(),
            instrument_revision: snapshot.instrument().revision,
            bar_definition_id: &snapshot.bar_definition().definition_id,
            bar_definition_version: snapshot.bar_definition().version,
            bar_interval_seconds: snapshot.bar_definition().interval_seconds,
            bar_trades_per_bar: snapshot.bar_definition().trades_per_bar,
        },
        bars.iter().map(|item| {
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
    ReplaySnapshot::try_new_provenanced(
        snapshot.instrument().clone(),
        snapshot.provenance(),
        snapshot.bar_definition().clone(),
        evidence,
        bars,
    )
    .map_err(websocket_error)
}

fn replay_snapshot_with_bar_definition_id(
    snapshot: &ReplaySnapshot,
    definition_id: &str,
) -> Result<ReplaySnapshot, ConformanceHarnessError> {
    let mut bar_definition = snapshot.bar_definition().clone();
    bar_definition.definition_id = definition_id.to_string();
    let bars = snapshot.bars().to_vec();
    let mut evidence = snapshot.evidence().clone();
    evidence.checksum = axiusflow_protocols::compute_market_snapshot_checksum(
        &evidence,
        axiusflow_protocols::MarketSnapshotIdentityRef {
            instrument_id: snapshot.instrument().instrument_id.as_str(),
            instrument_revision: snapshot.instrument().revision,
            bar_definition_id: &bar_definition.definition_id,
            bar_definition_version: bar_definition.version,
            bar_interval_seconds: bar_definition.interval_seconds,
            bar_trades_per_bar: bar_definition.trades_per_bar,
        },
        bars.iter().map(|item| {
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
    ReplaySnapshot::try_new_provenanced(
        snapshot.instrument().clone(),
        snapshot.provenance(),
        bar_definition,
        evidence,
        bars,
    )
    .map_err(websocket_error)
}

fn replay_snapshot_with_schema_version(
    snapshot: &ReplaySnapshot,
    schema_version: u32,
) -> Result<ReplaySnapshot, ConformanceHarnessError> {
    let bars = snapshot
        .bars()
        .iter()
        .cloned()
        .map(|item| {
            let (bar, mut provenance) = item.into_parts();
            provenance.schema_version = schema_version;
            axiusflow_application::Provenanced::new(bar, provenance)
        })
        .collect::<Vec<_>>();
    let mut evidence = snapshot.evidence().clone();
    evidence.schema_version = schema_version;
    evidence.checksum = axiusflow_protocols::compute_market_snapshot_checksum(
        &evidence,
        axiusflow_protocols::MarketSnapshotIdentityRef {
            instrument_id: snapshot.instrument().instrument_id.as_str(),
            instrument_revision: snapshot.instrument().revision,
            bar_definition_id: &snapshot.bar_definition().definition_id,
            bar_definition_version: snapshot.bar_definition().version,
            bar_interval_seconds: snapshot.bar_definition().interval_seconds,
            bar_trades_per_bar: snapshot.bar_definition().trades_per_bar,
        },
        bars.iter().map(|item| {
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
    ReplaySnapshot::try_new_provenanced(
        snapshot.instrument().clone(),
        snapshot.provenance(),
        snapshot.bar_definition().clone(),
        evidence,
        bars,
    )
    .map_err(websocket_error)
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
    let fallback_rebaselines = coordinator_fallback_rebaseline_conformance()?;
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
            publications.baseline.clone(),
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
        let metrics = bridge.metrics();
        bridge.queued_update_count() == 0
            && bridge.requires_snapshot()
            && metrics.recovery_pending
            && metrics.resnapshot_requests == 1
    }) && coordinator.metrics().publication_generation
        == Some(1);
    event_tx
        .try_send(MarketStreamEvent::Publication(Box::new(
            publications.recovered.clone(),
        )))
        .map_err(websocket_error)?;
    let ordinary_snapshot_rejected = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::PublicationRejectedAwaitingRecovery {
            generation: 3,
            last_sequence: 3,
            ..
        }
    );

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
        && ordinary_snapshot_rejected
        && fallback_rebaselines
        && recovery_bounded
        && event_disconnected
        && command_disconnected
        && metrics.publications_accepted == 1
        && metrics.publications_rejected == 3
        && metrics.stream_invalidations == 1
        && metrics.recovery_snapshots_installed == 1
        && metrics.recovery_snapshots_rejected == 2)
}

fn coordinator_fallback_rebaseline_conformance() -> Result<bool, ConformanceHarnessError> {
    Ok(coordinator_rebaseline_after_exhaustion()?
        && coordinator_rebaseline_identity_change()?
        && coordinator_accepts_unchanged_transport_snapshot()?
        && coordinator_accepts_schema_version_transition()?
        && coordinator_refreshes_pending_recovery_reason()?
        && coordinator_reinvalidates_queued_fallback()?)
}

fn coordinator_accepts_unchanged_transport_snapshot() -> Result<bool, ConformanceHarnessError> {
    let publications = scripted_stream_publications()?;
    let capacity = NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN);
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator = ChartStreamCoordinator::new(capacity);
    let baseline = publications.baseline.clone();
    let baseline_installed = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, baseline.clone())?,
        ChartStreamPollOutcome::InitialSnapshot { .. }
    );
    let invalidated = coordinator.mark_stream_invalid(ResnapshotReason::TransportReset);
    let request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("transport-reset fixture did not request recovery"))?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(request_id),
            attempts: 3,
            reason: ResnapshotReason::TransportReset,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let unchanged_queued = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, baseline)?,
        ChartStreamPollOutcome::PublicationQueued { generation: 1, .. }
    );
    let unchanged_installed = coordinator
        .drain_merged()
        .map_err(websocket_error)?
        .is_some_and(|merged| merged.snapshot().is_some())
        && coordinator
            .bridge()
            .is_some_and(|bridge| !bridge.requires_snapshot());
    Ok(baseline_installed && invalidated && exhausted && unchanged_queued && unchanged_installed)
}

fn coordinator_accepts_schema_version_transition() -> Result<bool, ConformanceHarnessError> {
    let source = EmbeddedReplaySource;
    let baseline = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 1 })
        .map_err(websocket_error)?;
    let schema_version = baseline.evidence().schema_version.saturating_add(1);
    let changed = replay_snapshot_with_schema_version(&baseline, schema_version)?;
    let capacity = NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN);
    let baseline = scripted_stream_publication(
        &mut MarketBarClientModel::new(capacity),
        ReplayStreamUpdate::Snapshot(baseline),
    )?;
    let changed = scripted_stream_publication(
        &mut MarketBarClientModel::new(capacity),
        ReplayStreamUpdate::Snapshot(changed),
    )?;
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator = ChartStreamCoordinator::new(capacity);
    let baseline_installed = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, baseline)?,
        ChartStreamPollOutcome::InitialSnapshot { .. }
    );
    let invalidated = coordinator.mark_stream_invalid(ResnapshotReason::SchemaChanged);
    let request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("schema-change fixture did not request recovery"))?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(request_id),
            attempts: 3,
            reason: ResnapshotReason::SchemaChanged,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let changed_queued = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, changed)?,
        ChartStreamPollOutcome::PublicationQueued { generation: 1, .. }
    );
    let changed_installed = coordinator
        .drain_merged()
        .map_err(websocket_error)?
        .is_some_and(|merged| {
            merged
                .snapshot()
                .is_some_and(|snapshot| snapshot.evidence().schema_version == schema_version)
        })
        && coordinator
            .bridge()
            .is_some_and(|bridge| !bridge.requires_snapshot());
    Ok(baseline_installed && invalidated && exhausted && changed_queued && changed_installed)
}

fn coordinator_refreshes_pending_recovery_reason() -> Result<bool, ConformanceHarnessError> {
    let publications = scripted_stream_publications()?;
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator =
        ChartStreamCoordinator::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    let baseline_installed = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, publications.baseline)?,
        ChartStreamPollOutcome::InitialSnapshot { .. }
    );
    let first_invalidated = coordinator.mark_stream_invalid(ResnapshotReason::SequenceGap);
    let first_request = coordinator
        .bridge()
        .and_then(ChartDataBridge::pending_resnapshot_request)
        .ok_or_else(|| websocket_error("reason-refresh fixture did not request recovery"))?;
    let refreshed = coordinator.mark_stream_invalid(ResnapshotReason::OwnershipHandoff);
    let refreshed_request = coordinator
        .bridge()
        .and_then(ChartDataBridge::pending_resnapshot_request)
        .ok_or_else(|| websocket_error("reason-refresh fixture did not replace recovery"))?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(refreshed_request.request_id),
            attempts: 3,
            reason: ResnapshotReason::OwnershipHandoff,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let fallback_queued = matches!(
        poll_scripted_publication(
            &mut coordinator,
            &port,
            &event_tx,
            publications.fallback_new_epoch,
        )?,
        ChartStreamPollOutcome::PublicationQueued { .. }
    );
    let fallback_installed = coordinator
        .drain_merged()
        .map_err(websocket_error)?
        .is_some_and(|merged| merged.snapshot().is_some());
    Ok(baseline_installed
        && first_invalidated
        && refreshed
        && refreshed_request.request_id != first_request.request_id
        && refreshed_request.reason == ResnapshotReason::OwnershipHandoff
        && exhausted
        && fallback_queued
        && fallback_installed)
}

fn coordinator_reinvalidates_queued_fallback() -> Result<bool, ConformanceHarnessError> {
    let publications = scripted_stream_publications()?;
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator =
        ChartStreamCoordinator::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    let baseline_installed = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, publications.baseline)?,
        ChartStreamPollOutcome::InitialSnapshot { .. }
    );
    let first_invalidated = coordinator.mark_stream_invalid(ResnapshotReason::SequenceGap);
    let _first_request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| {
            websocket_error("first reinvalidation fixture recovery was not requested")
        })?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: None,
            attempts: 3,
            reason: ResnapshotReason::SequenceGap,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let first_exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let fallback_queued = matches!(
        poll_scripted_publication(
            &mut coordinator,
            &port,
            &event_tx,
            publications.fallback_older,
        )?,
        ChartStreamPollOutcome::PublicationQueued { .. }
    );
    let second_invalidated = coordinator.mark_stream_invalid(ResnapshotReason::SequenceGap);
    let queued_fallback_cleared = coordinator
        .bridge()
        .is_some_and(|bridge| bridge.queued_update_count() == 0);
    let second_request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| {
            websocket_error("second reinvalidation fixture recovery was not requested")
        })?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(second_request_id),
            attempts: 3,
            reason: ResnapshotReason::SequenceGap,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let second_exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let delta_remains_fenced = matches!(
        poll_scripted_publication(
            &mut coordinator,
            &port,
            &event_tx,
            publications.fallback_delta,
        )?,
        ChartStreamPollOutcome::PublicationRejectedAwaitingRecovery { .. }
    );
    Ok(baseline_installed
        && first_invalidated
        && first_exhausted
        && fallback_queued
        && second_invalidated
        && queued_fallback_cleared
        && second_exhausted
        && delta_remains_fenced)
}

fn coordinator_rebaseline_after_exhaustion() -> Result<bool, ConformanceHarnessError> {
    let publications = scripted_stream_publications()?;
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator =
        ChartStreamCoordinator::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    event_tx
        .try_send(MarketStreamEvent::Publication(Box::new(
            publications.baseline.clone(),
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
            publications.delta.clone(),
        )))
        .map_err(websocket_error)?;
    let queued_before_invalidation = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::PublicationQueued {
            generation: 2,
            last_sequence: 2,
            ..
        }
    ) && coordinator
        .bridge()
        .is_some_and(|bridge| bridge.queued_update_count() == 1);
    event_tx
        .try_send(MarketStreamEvent::Publication(Box::new(
            publications.queued_tail_delta.clone(),
        )))
        .map_err(websocket_error)?;
    let tail_queued_before_invalidation = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::PublicationQueued {
            generation: 3,
            last_sequence: 3,
            ..
        }
    ) && coordinator
        .bridge()
        .is_some_and(|bridge| bridge.queued_update_count() == 2);
    let invalidated = coordinator.mark_stream_invalid(ResnapshotReason::SequenceGap);
    let request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("exhaustion fixture did not request recovery"))?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(request_id),
            attempts: 3,
            reason: ResnapshotReason::SequenceGap,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let canceled = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted {
            request_id: Some(actual),
            canceled: true,
            ..
        } if actual == request_id
    );
    let fallback_rebaseline =
        rebaseline_canceled_coordinator(&mut coordinator, &port, &event_tx, publications)?;
    let metrics = coordinator.metrics();
    Ok(baseline_installed
        && queued_before_invalidation
        && tail_queued_before_invalidation
        && invalidated
        && canceled
        && fallback_rebaseline
        && metrics.publications_accepted == 8
        && metrics.publications_rejected == 3
        && metrics.stream_invalidations == 0
        && metrics.recovery_exhaustions == 1)
}

fn coordinator_rebaseline_identity_change() -> Result<bool, ConformanceHarnessError> {
    Ok(
        rebaseline_discards_incompatible_tail(FallbackIdentityChange::OwnershipEpoch)?
            && rebaseline_discards_incompatible_tail(FallbackIdentityChange::Series)?
            && rebaseline_fences_stale_delta_after_snapshot()?
            && rebaseline_rejects_series_ownership_rollback()?,
    )
}

fn rebaseline_fences_stale_delta_after_snapshot() -> Result<bool, ConformanceHarnessError> {
    let source = EmbeddedReplaySource;
    let baseline = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 1 })
        .map_err(websocket_error)?;
    let stale_delta = source
        .load_delta(1)
        .map_err(websocket_error)?
        .ok_or_else(|| websocket_error("missing stale rebaseline delta"))?;
    let ownership_epoch = baseline.evidence().ownership_epoch.saturating_add(1);
    let fallback = replay_snapshot_with_ownership_epoch(&baseline, ownership_epoch)?;
    let valid_delta = {
        let (bar, mut provenance) = stale_delta.item().clone().into_parts();
        provenance.ownership_epoch = ownership_epoch;
        axiusflow_application::StreamDelta::try_new(
            stale_delta.previous_sequence(),
            stale_delta.sequence(),
            axiusflow_application::Provenanced::new(bar, provenance),
        )
        .map_err(websocket_error)?
    };
    let capacity = NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN);
    let mut baseline_model = MarketBarClientModel::new(capacity);
    let baseline_publication =
        scripted_stream_publication(&mut baseline_model, ReplayStreamUpdate::Snapshot(baseline))?;
    let stale_delta_publication =
        scripted_stream_publication(&mut baseline_model, ReplayStreamUpdate::Delta(stale_delta))?;
    let mut fallback_model = MarketBarClientModel::new(capacity);
    let fallback_publication =
        scripted_stream_publication(&mut fallback_model, ReplayStreamUpdate::Snapshot(fallback))?;
    let valid_delta_publication =
        scripted_stream_publication(&mut fallback_model, ReplayStreamUpdate::Delta(valid_delta))?;
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator = ChartStreamCoordinator::new(capacity);
    let baseline_installed = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, baseline_publication,)?,
        ChartStreamPollOutcome::InitialSnapshot { .. }
    );
    let invalidated = coordinator.mark_stream_invalid(ResnapshotReason::OwnershipHandoff);
    let request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("stale-delta fixture did not request recovery"))?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(request_id),
            attempts: 3,
            reason: ResnapshotReason::OwnershipHandoff,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let fallback_queued = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, fallback_publication,)?,
        ChartStreamPollOutcome::PublicationQueued { generation: 1, .. }
    );
    let stale_delta_rejected = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, stale_delta_publication,)?,
        ChartStreamPollOutcome::PublicationRejectedAwaitingRecovery { generation: 2, .. }
    );
    let valid_delta_queued = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, valid_delta_publication,)?,
        ChartStreamPollOutcome::PublicationQueued { generation: 2, .. }
    );
    let fallback_and_valid_delta_applied = coordinator
        .drain_merged()
        .map_err(websocket_error)?
        .is_some_and(|merged| {
            merged.accepted_deltas().len() == 1
                && merged
                    .snapshot()
                    .is_some_and(|snapshot| snapshot.evidence().ownership_epoch == ownership_epoch)
        })
        && coordinator.bridge().is_some_and(|bridge| {
            !bridge.requires_snapshot() && bridge.expected_sequence() == Some(3)
        })
        && coordinator.metrics().publication_generation == Some(2);
    Ok(baseline_installed
        && invalidated
        && exhausted
        && fallback_queued
        && stale_delta_rejected
        && valid_delta_queued
        && fallback_and_valid_delta_applied)
}

fn rebaseline_rejects_series_ownership_rollback() -> Result<bool, ConformanceHarnessError> {
    let source = EmbeddedReplaySource;
    let baseline = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 1 })
        .map_err(websocket_error)?;
    let baseline = replay_snapshot_with_ownership_epoch(&baseline, 2)?;
    let fallback = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .map_err(websocket_error)?;
    let fallback = replay_snapshot_with_bar_definition_id(&fallback, "stale-fallback-series")?;
    let capacity = NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN);
    let baseline = scripted_stream_publication(
        &mut MarketBarClientModel::new(capacity),
        ReplayStreamUpdate::Snapshot(baseline),
    )?;
    let fallback = scripted_stream_publication(
        &mut MarketBarClientModel::new(capacity),
        ReplayStreamUpdate::Snapshot(fallback),
    )?;
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator = ChartStreamCoordinator::new(capacity);
    let baseline_installed = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, baseline)?,
        ChartStreamPollOutcome::InitialSnapshot { .. }
    );
    let invalidated = coordinator.mark_stream_invalid(ResnapshotReason::OwnershipHandoff);
    let request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("ownership rollback fixture did not request recovery"))?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(request_id),
            attempts: 3,
            reason: ResnapshotReason::OwnershipHandoff,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let rollback_rejected = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, fallback)?,
        ChartStreamPollOutcome::PublicationSnapshotSuperseded { .. }
    );
    Ok(baseline_installed
        && invalidated
        && exhausted
        && rollback_rejected
        && coordinator.bridge().is_some_and(|bridge| {
            bridge.requires_snapshot()
                && bridge.queued_update_count() == 0
                && bridge.metrics().rejected_stale_snapshots == 1
        }))
}

#[derive(Clone, Copy)]
enum FallbackIdentityChange {
    OwnershipEpoch,
    Series,
}

fn rebaseline_discards_incompatible_tail(
    identity_change: FallbackIdentityChange,
) -> Result<bool, ConformanceHarnessError> {
    let publications = scripted_stream_publications()?;
    let (fallback, reason) = match identity_change {
        FallbackIdentityChange::OwnershipEpoch => (
            publications.fallback_new_epoch,
            ResnapshotReason::OwnershipHandoff,
        ),
        FallbackIdentityChange::Series => (
            publications.fallback_new_series,
            ResnapshotReason::SchemaChanged,
        ),
    };
    let expected_ownership_epoch = fallback.generation().ownership_epoch();
    let expected_last_sequence = fallback.generation().sequence_range().1;
    let expected_definition_id = match fallback.update() {
        ReplayStreamUpdate::Snapshot(snapshot) => snapshot.bar_definition().definition_id.clone(),
        ReplayStreamUpdate::Delta(_) => {
            return Err(websocket_error(
                "identity-change fallback was not a snapshot",
            ));
        }
    };
    let (command_tx, _command_rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
    let port = ScriptedMarketStreamPort {
        commands: command_tx,
        events: event_rx,
    };
    let mut coordinator =
        ChartStreamCoordinator::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    let prefix_queued = queue_identity_change_prefix(
        &mut coordinator,
        &port,
        &event_tx,
        publications.baseline,
        publications.delta,
        publications.queued_tail_delta,
    )?;
    let invalidated = coordinator.mark_stream_invalid(reason);
    let request_id = coordinator
        .bridge()
        .and_then(|bridge| bridge.metrics().recovery_request_id)
        .ok_or_else(|| websocket_error("identity-change fixture did not request recovery"))?;
    event_tx
        .try_send(MarketStreamEvent::RecoveryExhausted {
            request_id: Some(request_id),
            attempts: 3,
            reason,
            dropped_events: 0,
        })
        .map_err(websocket_error)?;
    let exhausted = matches!(
        coordinator.poll_once(&port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoveryExhausted { canceled: true, .. }
    );
    let intermediate_fallback_queued = drain_and_queue_intermediate_fallback(
        &mut coordinator,
        &port,
        &event_tx,
        publications.fallback_older,
    )?;
    let snapshot_queued = matches!(
        poll_scripted_publication(&mut coordinator, &port, &event_tx, fallback)?,
        ChartStreamPollOutcome::PublicationQueued {
            last_sequence,
            ..
        } if last_sequence == expected_last_sequence
    );
    let incompatible_tail_discarded = coordinator
        .bridge()
        .is_some_and(|bridge| bridge.queued_update_count() == 1);
    let merged = coordinator.drain_merged().map_err(websocket_error)?;
    let rebaselined_without_incompatible_tail = merged.is_some_and(|merged| {
        merged.accepted_deltas().is_empty()
            && merged.snapshot().is_some_and(|snapshot| {
                snapshot.evidence().ownership_epoch == expected_ownership_epoch
                    && snapshot.bar_definition().definition_id == expected_definition_id
            })
    });
    Ok(prefix_queued
        && invalidated
        && exhausted
        && intermediate_fallback_queued
        && snapshot_queued
        && incompatible_tail_discarded
        && rebaselined_without_incompatible_tail)
}

fn queue_identity_change_prefix(
    coordinator: &mut ChartStreamCoordinator,
    port: &ScriptedMarketStreamPort,
    event_tx: &std::sync::mpsc::SyncSender<MarketStreamEvent>,
    baseline: MarketStreamPublication,
    delta: MarketStreamPublication,
    tail_delta: MarketStreamPublication,
) -> Result<bool, ConformanceHarnessError> {
    let baseline_installed = matches!(
        poll_scripted_publication(coordinator, port, event_tx, baseline)?,
        ChartStreamPollOutcome::InitialSnapshot {
            last_sequence: 1,
            ..
        }
    );
    let first_delta_queued = matches!(
        poll_scripted_publication(coordinator, port, event_tx, delta)?,
        ChartStreamPollOutcome::PublicationQueued {
            last_sequence: 2,
            ..
        }
    );
    let tail_delta_queued = matches!(
        poll_scripted_publication(coordinator, port, event_tx, tail_delta)?,
        ChartStreamPollOutcome::PublicationQueued {
            last_sequence: 3,
            ..
        }
    );
    Ok(baseline_installed && first_delta_queued && tail_delta_queued)
}

fn drain_and_queue_intermediate_fallback(
    coordinator: &mut ChartStreamCoordinator,
    port: &ScriptedMarketStreamPort,
    event_tx: &std::sync::mpsc::SyncSender<MarketStreamEvent>,
    fallback: MarketStreamPublication,
) -> Result<bool, ConformanceHarnessError> {
    let drained = coordinator
        .drain_merged()
        .map_err(websocket_error)?
        .is_some();
    let queued = matches!(
        poll_scripted_publication(coordinator, port, event_tx, fallback)?,
        ChartStreamPollOutcome::PublicationQueued { .. }
    );
    Ok(drained && queued)
}

fn rebaseline_canceled_coordinator(
    coordinator: &mut ChartStreamCoordinator,
    port: &ScriptedMarketStreamPort,
    event_tx: &std::sync::mpsc::SyncSender<MarketStreamEvent>,
    publications: ScriptedStreamPublications,
) -> Result<bool, ConformanceHarnessError> {
    let cancellation_retains_snapshot_requirement = coordinator.bridge().is_some_and(|bridge| {
        let metrics = bridge.metrics();
        bridge.requires_snapshot() && !metrics.recovery_pending
    });
    let stale_snapshot_superseded = matches!(
        poll_scripted_publication(coordinator, port, event_tx, publications.fallback_stale,)?,
        ChartStreamPollOutcome::PublicationSnapshotSuperseded {
            generation: 1,
            last_sequence: 1,
            ..
        }
    );
    let delta_remains_fenced = matches!(
        poll_scripted_publication(coordinator, port, event_tx, publications.unrelated_delta,)?,
        ChartStreamPollOutcome::PublicationRejectedAwaitingRecovery {
            generation: 99,
            last_sequence: 2,
            ..
        }
    ) && coordinator
        .bridge()
        .is_some_and(|bridge| bridge.requires_snapshot() && !bridge.metrics().recovery_pending);
    let ordinary_snapshot_queued = matches!(
        poll_scripted_publication(coordinator, port, event_tx, publications.fallback_older)?,
        ChartStreamPollOutcome::PublicationQueued {
            generation: 2,
            last_sequence: 2,
            ..
        }
    );
    let delta_behind_snapshot_queued = matches!(
        poll_scripted_publication(coordinator, port, event_tx, publications.fallback_delta)?,
        ChartStreamPollOutcome::PublicationQueued {
            generation: 4,
            last_sequence: 4,
            ..
        }
    );
    let tail_delta_behind_snapshot_queued = matches!(
        poll_scripted_publication(
            coordinator,
            port,
            event_tx,
            publications.fallback_tail_delta,
        )?,
        ChartStreamPollOutcome::PublicationQueued {
            generation: 5,
            last_sequence: 5,
            ..
        }
    );
    let partial_snapshot_preserved_tail = matches!(
        poll_scripted_publication(coordinator, port, event_tx, publications.fallback_partial,)?,
        ChartStreamPollOutcome::PublicationQueued {
            generation: 4,
            last_sequence: 4,
            ..
        }
    );
    let next_delta_preserved_generation = matches!(
        poll_scripted_publication(
            coordinator,
            port,
            event_tx,
            publications.fallback_next_delta,
        )?,
        ChartStreamPollOutcome::PublicationQueued {
            generation: 6,
            last_sequence: 6,
            ..
        }
    );
    let older_snapshot_superseded = matches!(
        poll_scripted_publication(coordinator, port, event_tx, publications.recovered)?,
        ChartStreamPollOutcome::PublicationSnapshotSuperseded {
            generation: 3,
            last_sequence: 3,
            ..
        }
    );
    let rebaselined = drain_preserved_rebaseline_tail(coordinator)?;
    Ok(cancellation_retains_snapshot_requirement
        && stale_snapshot_superseded
        && delta_remains_fenced
        && ordinary_snapshot_queued
        && delta_behind_snapshot_queued
        && tail_delta_behind_snapshot_queued
        && partial_snapshot_preserved_tail
        && next_delta_preserved_generation
        && older_snapshot_superseded
        && rebaselined)
}

fn drain_preserved_rebaseline_tail(
    coordinator: &mut ChartStreamCoordinator,
) -> Result<bool, ConformanceHarnessError> {
    let queue_preserved = coordinator.bridge().is_some_and(|bridge| {
        let metrics = bridge.metrics();
        bridge.queued_update_count() == 3
            && metrics.queue_overflows == 0
            && !metrics.recovery_pending
            && metrics.rejected_stale_snapshots == 2
    });
    let drained = coordinator
        .drain_merged()
        .map_err(websocket_error)?
        .is_some();
    Ok(queue_preserved
        && drained
        && coordinator.bridge().is_some_and(|bridge| {
            !bridge.requires_snapshot() && bridge.expected_sequence() == Some(7)
        }))
}

fn poll_scripted_publication(
    coordinator: &mut ChartStreamCoordinator,
    port: &ScriptedMarketStreamPort,
    event_tx: &std::sync::mpsc::SyncSender<MarketStreamEvent>,
    publication: MarketStreamPublication,
) -> Result<ChartStreamPollOutcome, ConformanceHarnessError> {
    event_tx
        .try_send(MarketStreamEvent::Publication(Box::new(publication)))
        .map_err(websocket_error)?;
    coordinator.poll_once(port).map_err(websocket_error)
}

fn scripted_coordinator_recovery(
    coordinator: &mut ChartStreamCoordinator,
    port: &ScriptedMarketStreamPort,
    command_rx: &std::sync::mpsc::Receiver<MarketStreamCommand>,
    event_tx: &std::sync::mpsc::SyncSender<MarketStreamEvent>,
    recovered: MarketStreamPublication,
) -> Result<bool, ConformanceHarnessError> {
    let invalidated = coordinator.bridge().is_some_and(|bridge| {
        let metrics = bridge.metrics();
        bridge.requires_snapshot() && metrics.recovery_pending && metrics.resnapshot_requests == 1
    });
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
                reason: ResnapshotReason::SequenceGap,
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
    let mismatched_retryable = reject_mismatched_recovery_and_retry(
        coordinator,
        port,
        command_rx,
        event_tx,
        request_id,
        &recovered,
    )?;
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
        && mismatched_retryable
        && correlated_installed)
}

fn reject_mismatched_recovery_and_retry(
    coordinator: &mut ChartStreamCoordinator,
    port: &ScriptedMarketStreamPort,
    command_rx: &std::sync::mpsc::Receiver<MarketStreamCommand>,
    event_tx: &std::sync::mpsc::SyncSender<MarketStreamEvent>,
    request_id: u64,
    recovered: &MarketStreamPublication,
) -> Result<bool, ConformanceHarnessError> {
    let mismatched = MarketStreamPublication::try_new(
        "mismatched-scripted-market-stream".to_string(),
        recovered.update().clone(),
        recovered.generation().clone(),
    )
    .map_err(websocket_error)?;
    event_tx
        .try_send(MarketStreamEvent::RecoverySnapshot {
            request_id,
            publication: Box::new(mismatched),
        })
        .map_err(websocket_error)?;
    let retryable = matches!(
        coordinator.poll_once(port).map_err(websocket_error)?,
        ChartStreamPollOutcome::RecoverySnapshot {
            installed: false,
            ..
        }
    ) && coordinator.bridge().is_some_and(|bridge| {
        let metrics = bridge.metrics();
        metrics.recovery_pending && !metrics.recovery_dispatched
    });
    let retry_accepted = coordinator
        .try_dispatch_recovery(port)
        .map_err(websocket_error)?
        == ChartStreamRecoveryDispatch::Accepted;
    let retry_command = command_rx.try_recv().is_ok_and(|command| {
        command
            == MarketStreamCommand::Recover {
                request_id,
                reason: ResnapshotReason::SequenceGap,
            }
    });
    Ok(retryable && retry_accepted && retry_command)
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
