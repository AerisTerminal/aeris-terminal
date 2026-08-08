//! Headless desktop burst and frame-conflation evidence.

use crate::{
    market_worker::{
        FixtureMarketWorker, MarketWorkerMessage, MarketWorkerReceiver, MarketWorkerSender,
        market_worker_channel,
    },
    rithmic_live_chart::{RithmicChartGeneration, RithmicLiveChart, RithmicLiveChartError},
};
use axiusflow_application::ReplaySnapshot;
use axiusflow_instruments::InstrumentPrecision;
use axiusflow_market_data::{
    AggressorSide, BookSide, DepthDelta, DepthLevel, DepthSnapshot, EventMetadata, MarketEvent,
    MarketTrade, OrderBookRecoveryReason, OrderBookState, QualifiedTimestamp,
};
use axiusflow_provider_history::{
    HandoffCoordinator, HandoffState, SequencedHistory, VerifiedHistorySnapshot,
};
use axiusflow_terminal_ui::{DomSelection, DomUpdateOutcome, ReadOnlyDom};
use serde::Serialize;
use std::{
    error::Error,
    fs,
    io::Write,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{Pid, ProcessesToUpdate, System};

const BURST_UPDATES: usize = 10_000;
const MAXIMUM_WORKING_SET_GROWTH_BYTES: u64 = 64 * 1_024 * 1_024;
const ENDURANCE_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const ENDURANCE_BURST_UPDATES: usize = 1_000;
const ENDURANCE_BURST_EVERY_FRAMES: u64 = 60;
const ENDURANCE_CHECKPOINT_INTERVAL: Duration = Duration::from_mins(1);
const ENDURANCE_QUALIFICATION_DURATION: Duration = Duration::from_hours(8);

#[derive(Default)]
pub(crate) struct FramePollGate {
    scheduled: bool,
}

impl FramePollGate {
    pub(crate) const fn try_schedule(&mut self, window_active: bool) -> bool {
        if !window_active || self.scheduled {
            return false;
        }
        self.scheduled = true;
        true
    }

    pub(crate) const fn complete(&mut self) {
        self.scheduled = false;
    }
}

#[derive(Serialize)]
struct DesktopBurstEvidence {
    schema_version: u32,
    evidence_scope: &'static str,
    burst_updates: usize,
    mailbox_capacity: usize,
    retained_items: usize,
    retained_selection_generation: usize,
    retained_series_generation: usize,
    frame_requests_while_pending: usize,
    accepted_frame_requests_while_pending: usize,
    accepted_frame_requests_after_completion: usize,
    bounded_latest_state_conflation: bool,
    single_frame_drain_gate: bool,
    working_set_baseline_bytes: u64,
    working_set_current_bytes: u64,
    working_set_sampled_high_water_bytes: u64,
    working_set_sampled_growth_bytes: u64,
    maximum_working_set_growth_bytes: u64,
    working_set_within_bound: bool,
    gap_recovery: DesktopGapRecoveryEvidence,
}

#[derive(Serialize)]
struct DesktopGapRecoveryEvidence {
    trade_ordering_fault_rejected: bool,
    history: HistoryGapRecoveryEvidence,
    depth: DepthGapRecoveryEvidence,
}

#[derive(Serialize)]
struct HistoryGapRecoveryEvidence {
    history_gap_requires_snapshot: bool,
    history_covering_snapshot_recovers: bool,
}

#[derive(Serialize)]
struct DepthGapRecoveryEvidence {
    depth_gap_clears_book: bool,
    depth_covering_snapshot_recovers: bool,
}

struct ProcessMemoryProbe {
    system: System,
    pid: Pid,
    baseline_bytes: u64,
    current_bytes: u64,
    high_water_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EnduranceCompletionState {
    Incomplete,
    Completed,
}

#[derive(Serialize)]
struct DesktopEnduranceEvidence {
    schema_version: u32,
    evidence_scope: &'static str,
    completion_state: EnduranceCompletionState,
    checkpoint_sequence: u64,
    checkpoint_unix_milliseconds: u128,
    requested_duration_seconds: u64,
    required_qualification_duration_seconds: u64,
    elapsed_milliseconds: u128,
    frame_cycles: u64,
    updates_published: u64,
    periodic_burst_updates: usize,
    mailbox_capacity: usize,
    mailbox_high_water_items: usize,
    last_generation: usize,
    stale_or_gapped_publications: u64,
    working_set_baseline_bytes: u64,
    working_set_current_bytes: u64,
    working_set_sampled_high_water_bytes: u64,
    maximum_working_set_growth_bytes: u64,
    working_set_within_bound: bool,
    clean_stop: bool,
    readiness_qualified: bool,
}

impl ProcessMemoryProbe {
    fn new() -> Result<Self, Box<dyn Error>> {
        let pid = sysinfo::get_current_pid()?;
        let mut probe = Self {
            system: System::new(),
            pid,
            baseline_bytes: 0,
            current_bytes: 0,
            high_water_bytes: 0,
        };
        probe.sample()?;
        probe.baseline_bytes = probe.current_bytes;
        Ok(probe)
    }

    fn sample(&mut self) -> Result<(), Box<dyn Error>> {
        self.system
            .refresh_processes(ProcessesToUpdate::Some(&[self.pid]));
        self.current_bytes = self
            .system
            .process(self.pid)
            .ok_or("current process is absent from the system process table")?
            .memory();
        self.high_water_bytes = self.high_water_bytes.max(self.current_bytes);
        Ok(())
    }
}

fn collect_evidence() -> Result<DesktopBurstEvidence, Box<dyn Error>> {
    let mut memory = ProcessMemoryProbe::new()?;
    let mut fixture = FixtureMarketWorker::try_new()?;
    let snapshot = fixture.publish_snapshot(2)?.snapshot;
    let capacity = NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN);
    let (sender, receiver) = market_worker_channel(capacity);

    for generation in 1..=BURST_UPDATES {
        let generation = NonZeroUsize::new(generation).unwrap_or(NonZeroUsize::MIN);
        sender
            .send(MarketWorkerMessage::RithmicLive {
                selection_generation: NonZeroUsize::MIN,
                series_generation: generation,
                snapshot: snapshot.clone(),
            })
            .map_err(|()| "desktop burst mailbox disconnected")?;
        if generation.get().is_multiple_of(128) {
            memory.sample()?;
        }
    }

    let (retained_items, mailbox_capacity) = sender.occupancy();
    let (messages, disconnected) = receiver.drain();
    let (retained_selection_generation, retained_series_generation) = match messages.as_slice() {
        [
            MarketWorkerMessage::RithmicLive {
                selection_generation,
                series_generation,
                ..
            },
        ] if !disconnected => (selection_generation.get(), series_generation.get()),
        _ => return Err("desktop burst did not retain exactly one live snapshot".into()),
    };

    let mut gate = FramePollGate::default();
    let accepted_frame_requests_while_pending = (0..BURST_UPDATES)
        .filter(|_| gate.try_schedule(true))
        .count();
    gate.complete();
    let accepted_frame_requests_after_completion = usize::from(gate.try_schedule(true));

    let bounded_latest_state_conflation = retained_items == 1
        && mailbox_capacity == capacity.get()
        && retained_selection_generation == 1
        && retained_series_generation == BURST_UPDATES;
    let single_frame_drain_gate =
        accepted_frame_requests_while_pending == 1 && accepted_frame_requests_after_completion == 1;
    memory.sample()?;
    let working_set_within_bound = memory
        .high_water_bytes
        .saturating_sub(memory.baseline_bytes)
        <= MAXIMUM_WORKING_SET_GROWTH_BYTES;
    let working_set_sampled_growth_bytes = memory
        .high_water_bytes
        .saturating_sub(memory.baseline_bytes);
    if !bounded_latest_state_conflation || !single_frame_drain_gate || !working_set_within_bound {
        return Err("desktop burst/frame conflation contract failed".into());
    }
    let gap_recovery = collect_gap_recovery_evidence()?;

    Ok(DesktopBurstEvidence {
        schema_version: 2,
        evidence_scope: "deterministic_desktop_burst_and_frame_conflation",
        burst_updates: BURST_UPDATES,
        mailbox_capacity,
        retained_items,
        retained_selection_generation,
        retained_series_generation,
        frame_requests_while_pending: BURST_UPDATES,
        accepted_frame_requests_while_pending,
        accepted_frame_requests_after_completion,
        bounded_latest_state_conflation,
        single_frame_drain_gate,
        working_set_baseline_bytes: memory.baseline_bytes,
        working_set_current_bytes: memory.current_bytes,
        working_set_sampled_high_water_bytes: memory.high_water_bytes,
        working_set_sampled_growth_bytes,
        maximum_working_set_growth_bytes: MAXIMUM_WORKING_SET_GROWTH_BYTES,
        working_set_within_bound,
        gap_recovery,
    })
}

fn collect_gap_recovery_evidence() -> Result<DesktopGapRecoveryEvidence, Box<dyn Error>> {
    let evidence = DesktopGapRecoveryEvidence {
        trade_ordering_fault_rejected: collect_trade_gap_evidence()?,
        history: collect_history_gap_evidence()?,
        depth: collect_depth_gap_evidence()?,
    };
    if !evidence.trade_ordering_fault_rejected
        || !evidence.history.history_gap_requires_snapshot
        || !evidence.history.history_covering_snapshot_recovers
        || !evidence.depth.depth_gap_clears_book
        || !evidence.depth.depth_covering_snapshot_recovers
    {
        return Err("desktop gap recovery contract failed".into());
    }
    Ok(evidence)
}

fn collect_trade_gap_evidence() -> Result<bool, Box<dyn Error>> {
    let mut fixture = FixtureMarketWorker::try_new()?;
    let snapshot = fixture.publish_snapshot(2)?.snapshot;
    let generation = RithmicChartGeneration {
        selection: NonZeroUsize::MIN,
        series: NonZeroUsize::MIN,
    };
    let seed = snapshot
        .bars()
        .last()
        .ok_or("fixture snapshot did not contain a chart seed")?;
    let first_trade_timestamp = seed
        .provenance()
        .exchange_timestamp_unix_nanos
        .checked_add(1)
        .ok_or("fixture timestamp overflowed")?;
    let trade = MarketTrade {
        metadata: EventMetadata {
            provider_id: seed.provenance().source_id.clone(),
            instrument_id: snapshot.instrument().instrument_id.as_str().to_string(),
            entitlement_id: seed.provenance().entitlement_revision.clone(),
            source_sequence: 100,
            session_generation: 7,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(first_trade_timestamp),
                provider_unix_nanos: Some(first_trade_timestamp),
                received_unix_nanos: first_trade_timestamp,
            },
        },
        trade_id: "readiness-trade-100".to_string(),
        price: seed.value().close,
        quantity: 1,
        aggressor: AggressorSide::Unknown,
    };
    let mut chart = RithmicLiveChart::from_history(generation, &snapshot)?;
    chart.apply_trade(generation, &trade)?;
    Ok(matches!(
        chart.apply_trade(generation, &trade),
        Err(RithmicLiveChartError::OutOfOrderTrade)
    ))
}

fn collect_history_gap_evidence() -> Result<HistoryGapRecoveryEvidence, Box<dyn Error>> {
    let mut history = HandoffCoordinator::new(NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN));
    history.install_snapshot(history_snapshot(1, 2)?)?;
    let gap_result = history.push_live(sequenced_history(4));
    let history_gap_requires_snapshot = gap_result.is_err()
        && matches!(
            history.state(),
            HandoffState::SnapshotRequired {
                minimum_generation: 1,
                minimum_watermark: 4
            }
        );
    history.install_snapshot(history_snapshot(2, 4)?)?;
    let history_covering_snapshot_recovers = matches!(
        history.state(),
        HandoffState::Live {
            generation: 2,
            last_sequence: 4
        }
    );
    Ok(HistoryGapRecoveryEvidence {
        history_gap_requires_snapshot,
        history_covering_snapshot_recovers,
    })
}

fn collect_depth_gap_evidence() -> Result<DepthGapRecoveryEvidence, Box<dyn Error>> {
    let mut dom = ReadOnlyDom::new(NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN));
    dom.select(DomSelection {
        provider_id: "rithmic".to_string(),
        instrument_id: "mnq".to_string(),
        entitlement_id: "test".to_string(),
        session_generation: 7,
        selection_generation: 1,
        precision: InstrumentPrecision::try_new(2, 0)?,
    });
    dom.apply_event(&depth_snapshot(10))?;
    dom.apply_event(&depth_delta(11))?;
    let depth_gap = dom.apply_event(&depth_delta(13));
    let recovering = dom
        .frame()
        .ok_or("DOM selection disappeared during recovery")?;
    let depth_gap_clears_book = depth_gap.is_err()
        && recovering.state == OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        && recovering.rows.is_empty();
    let depth_covering_snapshot_recovers = matches!(
        dom.apply_event(&depth_snapshot(13))?,
        DomUpdateOutcome::Published(frame)
            if frame.state == OrderBookState::Ready && frame.source_watermark == 13
    );
    Ok(DepthGapRecoveryEvidence {
        depth_gap_clears_book,
        depth_covering_snapshot_recovers,
    })
}

fn sequenced_history(sequence: u64) -> SequencedHistory<u64> {
    SequencedHistory {
        sequence: NonZeroU64::new(sequence).unwrap_or(NonZeroU64::MIN),
        value: sequence,
    }
}

fn history_snapshot(
    generation: u64,
    watermark: u64,
) -> Result<VerifiedHistorySnapshot<u64>, Box<dyn Error>> {
    let items = (1..=watermark).map(sequenced_history).collect();
    Ok(VerifiedHistorySnapshot::try_new(
        NonZeroU64::new(generation).unwrap_or(NonZeroU64::MIN),
        items,
    )?)
}

fn depth_metadata(sequence: u64) -> EventMetadata {
    EventMetadata {
        provider_id: "rithmic".to_string(),
        instrument_id: "mnq".to_string(),
        entitlement_id: "test".to_string(),
        source_sequence: sequence,
        session_generation: 7,
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: Some(i64::try_from(sequence).unwrap_or(i64::MAX)),
            provider_unix_nanos: None,
            received_unix_nanos: i64::try_from(sequence).unwrap_or(i64::MAX),
        },
    }
}

fn depth_snapshot(sequence: u64) -> MarketEvent {
    MarketEvent::DepthSnapshot(DepthSnapshot {
        metadata: depth_metadata(sequence),
        bids: vec![DepthLevel {
            price: 20_000,
            quantity: 2,
            order_count: Some(1),
        }],
        asks: vec![DepthLevel {
            price: 20_025,
            quantity: 3,
            order_count: Some(1),
        }],
    })
}

fn depth_delta(sequence: u64) -> MarketEvent {
    MarketEvent::DepthDelta(DepthDelta {
        metadata: depth_metadata(sequence),
        side: BookSide::Bid,
        level: DepthLevel {
            price: 20_000,
            quantity: 4,
            order_count: Some(2),
        },
    })
}

pub(crate) fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    let report = collect_evidence()?;
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "desktop_burst_conformance=passed updates={} retained={} frame_schedules={} report={}",
        report.burst_updates,
        report.retained_items,
        report.accepted_frame_requests_while_pending,
        report_path.display()
    );
    Ok(())
}

fn validate_endurance_duration(duration: Duration) -> Result<(), Box<dyn Error>> {
    if duration.is_zero() || duration > ENDURANCE_QUALIFICATION_DURATION {
        return Err(
            "desktop endurance duration must be between one nanosecond and eight hours".into(),
        );
    }
    Ok(())
}

fn endurance_evidence(
    completion_state: EnduranceCompletionState,
    checkpoint_sequence: u64,
    duration: Duration,
    snapshot: &EnduranceEvidenceSnapshot<'_>,
) -> DesktopEnduranceEvidence {
    let readiness_qualified = completion_state == EnduranceCompletionState::Completed
        && duration == ENDURANCE_QUALIFICATION_DURATION
        && snapshot.elapsed >= ENDURANCE_QUALIFICATION_DURATION
        && snapshot.working_set_within_bound
        && snapshot.clean_stop;
    DesktopEnduranceEvidence {
        schema_version: 2,
        evidence_scope: "headless_desktop_continuous_endurance",
        completion_state,
        checkpoint_sequence,
        checkpoint_unix_milliseconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis()),
        requested_duration_seconds: duration.as_secs(),
        required_qualification_duration_seconds: ENDURANCE_QUALIFICATION_DURATION.as_secs(),
        elapsed_milliseconds: snapshot.elapsed.as_millis(),
        frame_cycles: snapshot.counters.frame_cycles,
        updates_published: snapshot.counters.updates_published,
        periodic_burst_updates: ENDURANCE_BURST_UPDATES,
        mailbox_capacity: snapshot.mailbox_capacity,
        mailbox_high_water_items: snapshot.counters.mailbox_high_water_items,
        last_generation: snapshot.counters.last_generation,
        stale_or_gapped_publications: snapshot.counters.stale_or_gapped_publications,
        working_set_baseline_bytes: snapshot.memory.baseline_bytes,
        working_set_current_bytes: snapshot.memory.current_bytes,
        working_set_sampled_high_water_bytes: snapshot.memory.high_water_bytes,
        maximum_working_set_growth_bytes: MAXIMUM_WORKING_SET_GROWTH_BYTES,
        working_set_within_bound: snapshot.working_set_within_bound,
        clean_stop: snapshot.clean_stop,
        readiness_qualified,
    }
}

#[derive(Default)]
struct EnduranceCounters {
    frame_cycles: u64,
    updates_published: u64,
    last_generation: usize,
    mailbox_high_water_items: usize,
    stale_or_gapped_publications: u64,
}

struct EnduranceEvidenceSnapshot<'a> {
    elapsed: Duration,
    counters: &'a EnduranceCounters,
    mailbox_capacity: usize,
    memory: &'a ProcessMemoryProbe,
    working_set_within_bound: bool,
    clean_stop: bool,
}

fn run_endurance_frame(
    sender: &MarketWorkerSender,
    receiver: &MarketWorkerReceiver,
    snapshot: &ReplaySnapshot,
    gate: &mut FramePollGate,
    counters: &mut EnduranceCounters,
) -> Result<(), Box<dyn Error>> {
    counters.frame_cycles = counters.frame_cycles.saturating_add(1);
    let updates = if counters
        .frame_cycles
        .is_multiple_of(ENDURANCE_BURST_EVERY_FRAMES)
    {
        ENDURANCE_BURST_UPDATES
    } else {
        1
    };
    for _ in 0..updates {
        counters.last_generation = counters
            .last_generation
            .checked_add(1)
            .ok_or("desktop endurance generation overflowed")?;
        let generation = NonZeroUsize::new(counters.last_generation).unwrap_or(NonZeroUsize::MIN);
        sender
            .send(MarketWorkerMessage::RithmicLive {
                selection_generation: NonZeroUsize::MIN,
                series_generation: generation,
                snapshot: snapshot.clone(),
            })
            .map_err(|()| "desktop endurance mailbox disconnected")?;
        counters.updates_published = counters.updates_published.saturating_add(1);
    }
    counters.mailbox_high_water_items = counters.mailbox_high_water_items.max(sender.occupancy().0);
    if !gate.try_schedule(true) {
        return Err("desktop endurance frame gate rejected an idle frame".into());
    }
    let (messages, disconnected) = receiver.drain();
    gate.complete();
    if !matches!(
        messages.as_slice(),
        [MarketWorkerMessage::RithmicLive {
            selection_generation,
            series_generation,
            ..
        }] if !disconnected
            && selection_generation.get() == 1
            && series_generation.get() == counters.last_generation
    ) {
        counters.stale_or_gapped_publications =
            counters.stale_or_gapped_publications.saturating_add(1);
    }
    Ok(())
}

fn collect_endurance_with_checkpoints(
    duration: Duration,
    checkpoint_interval: Duration,
    mut checkpoint: impl FnMut(&DesktopEnduranceEvidence) -> Result<(), Box<dyn Error>>,
) -> Result<DesktopEnduranceEvidence, Box<dyn Error>> {
    validate_endurance_duration(duration)?;
    if checkpoint_interval.is_zero() {
        return Err("desktop endurance checkpoint interval must be nonzero".into());
    }
    let mut fixture = FixtureMarketWorker::try_new()?;
    let snapshot = fixture.publish_snapshot(2)?.snapshot;
    let capacity = NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN);
    let (sender, receiver) = market_worker_channel(capacity);
    let mut memory = ProcessMemoryProbe::new()?;
    let started = Instant::now();
    let deadline = started + duration;
    let mut next_memory_sample = started + Duration::from_secs(1);
    let mut next_checkpoint = started + checkpoint_interval;
    let mut checkpoint_sequence = 0_u64;
    let mut counters = EnduranceCounters::default();
    let mut gate = FramePollGate::default();

    while Instant::now() < deadline {
        let frame_started = Instant::now();
        run_endurance_frame(&sender, &receiver, &snapshot, &mut gate, &mut counters)?;

        let now = Instant::now();
        if now >= next_memory_sample {
            memory.sample()?;
            next_memory_sample = now + Duration::from_secs(1);
        }
        if now >= next_checkpoint {
            checkpoint_sequence = checkpoint_sequence.saturating_add(1);
            let working_set_within_bound = memory
                .high_water_bytes
                .saturating_sub(memory.baseline_bytes)
                <= MAXIMUM_WORKING_SET_GROWTH_BYTES;
            let evidence = endurance_evidence(
                EnduranceCompletionState::Incomplete,
                checkpoint_sequence,
                duration,
                &EnduranceEvidenceSnapshot {
                    elapsed: started.elapsed(),
                    counters: &counters,
                    mailbox_capacity: capacity.get(),
                    memory: &memory,
                    working_set_within_bound,
                    clean_stop: false,
                },
            );
            checkpoint(&evidence)?;
            next_checkpoint = now + checkpoint_interval;
        }
        if let Some(remaining) = ENDURANCE_FRAME_INTERVAL.checked_sub(frame_started.elapsed()) {
            thread::sleep(remaining.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
    memory.sample()?;
    let working_set_within_bound = memory
        .high_water_bytes
        .saturating_sub(memory.baseline_bytes)
        <= MAXIMUM_WORKING_SET_GROWTH_BYTES;
    let clean_stop = counters.stale_or_gapped_publications == 0
        && counters.mailbox_high_water_items <= capacity.get()
        && sender.occupancy().0 == 0;
    if !working_set_within_bound || !clean_stop {
        return Err("desktop endurance bounds or continuity failed".into());
    }

    Ok(endurance_evidence(
        EnduranceCompletionState::Completed,
        checkpoint_sequence.saturating_add(1),
        duration,
        &EnduranceEvidenceSnapshot {
            elapsed: started.elapsed(),
            counters: &counters,
            mailbox_capacity: capacity.get(),
            memory: &memory,
            working_set_within_bound,
            clean_stop,
        },
    ))
}

#[cfg(test)]
fn collect_endurance(duration: Duration) -> Result<DesktopEnduranceEvidence, Box<dyn Error>> {
    collect_endurance_with_checkpoints(duration, ENDURANCE_CHECKPOINT_INTERVAL, |_| Ok(()))
}

pub(crate) fn run_endurance(report_path: &Path, duration: Duration) -> Result<(), Box<dyn Error>> {
    validate_endurance_duration(duration)?;
    let initial_memory = ProcessMemoryProbe::new()?;
    let initial_counters = EnduranceCounters::default();
    let initial = endurance_evidence(
        EnduranceCompletionState::Incomplete,
        0,
        duration,
        &EnduranceEvidenceSnapshot {
            elapsed: Duration::ZERO,
            counters: &initial_counters,
            mailbox_capacity: 32,
            memory: &initial_memory,
            working_set_within_bound: true,
            clean_stop: false,
        },
    );
    write_endurance_evidence_atomically(report_path, &initial)?;
    let report = collect_endurance_with_checkpoints(
        duration,
        ENDURANCE_CHECKPOINT_INTERVAL,
        |checkpoint| write_endurance_evidence_atomically(report_path, checkpoint),
    )?;
    write_endurance_evidence_atomically(report_path, &report)?;
    println!(
        "desktop_endurance=completed readiness_qualified={} seconds={} frames={} updates={} memory_high_water={} report={}",
        report.readiness_qualified,
        report.requested_duration_seconds,
        report.frame_cycles,
        report.updates_published,
        report.working_set_sampled_high_water_bytes,
        report_path.display()
    );
    Ok(())
}

fn write_endurance_evidence_atomically(
    report_path: &Path,
    report: &DesktopEnduranceEvidence,
) -> Result<(), Box<dyn Error>> {
    let mut encoded = serde_json::to_vec_pretty(report)?;
    encoded.push(b'\n');
    write_bytes_atomically(report_path, &encoded)
}

fn write_bytes_atomically(path: &Path, contents: &[u8]) -> Result<(), Box<dyn Error>> {
    let temporary_path = temporary_sibling(path)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary_path, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn temporary_sibling(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let file_name = path
        .file_name()
        .ok_or("desktop endurance report path must name a file")?
        .to_string_lossy();
    Ok(path.with_file_name(format!(".{file_name}.{}.partial", std::process::id())))
}

#[cfg(test)]
mod tests {
    use super::{
        BURST_UPDATES, ENDURANCE_QUALIFICATION_DURATION, EnduranceCompletionState,
        EnduranceCounters, EnduranceEvidenceSnapshot, FramePollGate, ProcessMemoryProbe,
        collect_endurance_with_checkpoints, collect_evidence, collect_gap_recovery_evidence,
        endurance_evidence, write_endurance_evidence_atomically,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn frame_gate_accepts_one_request_until_completion() {
        let mut gate = FramePollGate::default();
        assert!(!gate.try_schedule(false));
        assert!(gate.try_schedule(true));
        assert!(!gate.try_schedule(true));
        gate.complete();
        assert!(gate.try_schedule(true));
    }

    #[test]
    fn burst_retains_only_the_latest_generation() {
        let evidence = collect_evidence().expect("burst evidence passes");
        assert_eq!(evidence.retained_items, 1);
        assert_eq!(evidence.retained_series_generation, BURST_UPDATES);
        assert!(evidence.bounded_latest_state_conflation);
        assert!(evidence.single_frame_drain_gate);
        assert!(evidence.working_set_within_bound);
        assert!(evidence.gap_recovery.trade_ordering_fault_rejected);
        assert!(
            evidence
                .gap_recovery
                .history
                .history_covering_snapshot_recovers
        );
        assert!(evidence.gap_recovery.depth.depth_covering_snapshot_recovers);
    }

    #[test]
    fn gaps_fail_closed_and_covering_snapshots_recover() {
        let evidence = collect_gap_recovery_evidence().expect("gap recovery evidence passes");
        assert!(evidence.trade_ordering_fault_rejected);
        assert!(evidence.history.history_gap_requires_snapshot);
        assert!(evidence.history.history_covering_snapshot_recovers);
        assert!(evidence.depth.depth_gap_clears_book);
        assert!(evidence.depth.depth_covering_snapshot_recovers);
    }

    #[test]
    fn short_endurance_preserves_continuity_and_bounds() {
        let evidence = super::collect_endurance(std::time::Duration::from_millis(80))
            .expect("short endurance evidence passes");
        assert!(evidence.frame_cycles > 1);
        assert_eq!(evidence.stale_or_gapped_publications, 0);
        assert_eq!(evidence.mailbox_high_water_items, 1);
        assert!(evidence.working_set_within_bound);
        assert!(evidence.clean_stop);
        assert_eq!(
            evidence.completion_state,
            EnduranceCompletionState::Completed
        );
        assert!(!evidence.readiness_qualified);
    }

    #[test]
    fn interrupted_endurance_retains_an_atomic_incomplete_checkpoint() {
        let directory = std::env::temp_dir().join(format!(
            "axiusflow-endurance-checkpoint-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time follows the Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir(&directory).expect("checkpoint test directory is created");
        let report_path = directory.join("endurance.json");
        let memory = ProcessMemoryProbe::new().expect("process memory is available");
        let counters = EnduranceCounters::default();
        let initial = endurance_evidence(
            EnduranceCompletionState::Incomplete,
            0,
            Duration::from_millis(120),
            &EnduranceEvidenceSnapshot {
                elapsed: Duration::ZERO,
                counters: &counters,
                mailbox_capacity: 32,
                memory: &memory,
                working_set_within_bound: true,
                clean_stop: false,
            },
        );
        write_endurance_evidence_atomically(&report_path, &initial)
            .expect("initial checkpoint is written");

        let result = collect_endurance_with_checkpoints(
            Duration::from_millis(120),
            Duration::from_millis(20),
            |checkpoint| {
                write_endurance_evidence_atomically(&report_path, checkpoint)?;
                Err("simulated interruption after checkpoint".into())
            },
        );
        assert!(result.is_err());
        let persisted: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&report_path).expect("checkpoint remains readable"),
        )
        .expect("checkpoint remains valid JSON");
        assert_eq!(persisted["completion_state"], "incomplete");
        assert!(persisted["checkpoint_sequence"].as_u64().unwrap_or(0) >= 1);
        assert_eq!(persisted["clean_stop"], false);
        assert_eq!(persisted["readiness_qualified"], false);
        assert!(
            std::fs::read_dir(&directory)
                .expect("checkpoint directory remains readable")
                .all(|entry| !entry
                    .expect("checkpoint directory entry is readable")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".partial"))
        );
        std::fs::remove_dir_all(directory).expect("checkpoint test directory is removed");
    }

    #[test]
    fn only_an_exact_clean_eight_hour_completion_qualifies() {
        let memory = ProcessMemoryProbe::new().expect("process memory is available");
        let counters = EnduranceCounters {
            frame_cycles: 1,
            updates_published: 1,
            last_generation: 1,
            mailbox_high_water_items: 1,
            stale_or_gapped_publications: 0,
        };
        let exact = endurance_evidence(
            EnduranceCompletionState::Completed,
            481,
            ENDURANCE_QUALIFICATION_DURATION,
            &EnduranceEvidenceSnapshot {
                elapsed: ENDURANCE_QUALIFICATION_DURATION,
                counters: &counters,
                mailbox_capacity: 32,
                memory: &memory,
                working_set_within_bound: true,
                clean_stop: true,
            },
        );
        assert!(exact.readiness_qualified);

        let short = endurance_evidence(
            EnduranceCompletionState::Completed,
            2,
            Duration::from_secs(5),
            &EnduranceEvidenceSnapshot {
                elapsed: ENDURANCE_QUALIFICATION_DURATION,
                counters: &counters,
                mailbox_capacity: 32,
                memory: &memory,
                working_set_within_bound: true,
                clean_stop: true,
            },
        );
        assert!(!short.readiness_qualified);

        let unclean = endurance_evidence(
            EnduranceCompletionState::Completed,
            481,
            ENDURANCE_QUALIFICATION_DURATION,
            &EnduranceEvidenceSnapshot {
                elapsed: ENDURANCE_QUALIFICATION_DURATION,
                counters: &counters,
                mailbox_capacity: 32,
                memory: &memory,
                working_set_within_bound: true,
                clean_stop: false,
            },
        );
        assert!(!unclean.readiness_qualified);
    }
}
