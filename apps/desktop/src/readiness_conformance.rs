//! Headless desktop burst and frame-conflation evidence.
//!
//! Everything measured here is **synthetic**: a fixture worker drives a real
//! mailbox, frame gate, and client model, with no engine process, no IPC, and no
//! provider. That makes it a bounds-and-conflation check, not evidence that the
//! desktop works against a venue. The only thing that can say that is the live
//! market gate, whose result this report carries verbatim — including
//! [`LiveMarketGate::NotRun`], which is never reported as a pass.

use crate::frame_poll_gate::FramePollGate;

use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    ReplaySnapshot, ReplayStreamUpdate, ResnapshotReason,
};
use axiusflow_desktop::market_worker::{
    FixtureMarketWorker, MarketWorkerMessage, MarketWorkerReceiver, MarketWorkerSender,
    market_worker_channel,
};
use axiusflow_instruments::InstrumentPrecision;
use axiusflow_market_data::{
    BookSide, DepthDelta, DepthLevel, DepthSnapshot, EventMetadata, MarketEvent,
    OrderBookRecoveryReason, OrderBookState, QualifiedTimestamp,
};
use axiusflow_terminal_ui::{DomSelection, DomUpdateOutcome, ReadOnlyDom};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{Pid, ProcessesToUpdate, System};

const BURST_UPDATES: usize = 10_000;
/// Where each live market gate records its own outcome, relative to the
/// repository root. A gate is a separate, credentialed, network-bound run; a
/// missing file means it did not run, which is not a pass.
const LIVE_GATE_REPORTS: [(&str, &str); 2] = [
    ("coinbase", ".cache/evidence/live_market_gate_coinbase.json"),
    ("rithmic", ".cache/evidence/live_market_gate_rithmic.json"),
];
const LIVE_GATE_SCHEMA_VERSION: u32 = 1;
const LIVE_GATE_EVIDENCE_SCOPE: &str = "engine_live_market_gate";
const LIVE_GATE_MAXIMUM_AGE_SECONDS: u64 = 24 * 60 * 60;
const LIVE_GATE_MAXIMUM_FUTURE_SKEW_SECONDS: u64 = 5 * 60;
const LIVE_GATE_MAXIMUM_REPORT_BYTES: u64 = 16 * 1_024;
const MAXIMUM_WORKING_SET_GROWTH_BYTES: u64 = 64 * 1_024 * 1_024;
const ENDURANCE_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const ENDURANCE_BURST_UPDATES: usize = 1_000;
const ENDURANCE_BURST_EVERY_FRAMES: u64 = 60;
const ENDURANCE_CHECKPOINT_INTERVAL: Duration = Duration::from_mins(1);
const ENDURANCE_QUALIFICATION_DURATION: Duration = Duration::from_hours(8);

/// What a conformance report is actually evidence of.
///
/// The four states are kept distinct because collapsing them is how a suite
/// starts reporting green for work nobody ran. A deterministic pass says the
/// bounds hold in a fixture; it says nothing about a venue.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LiveMarketGate {
    /// No live gate has recorded a result. Never a pass.
    NotRun,
    Passed,
    Failed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedLiveMarketGate {
    schema_version: u32,
    evidence_scope: String,
    provider: String,
    outcome: RecordedLiveMarketGateOutcome,
    completion_state: RecordedLiveMarketGateCompletion,
    recorded_at_unix_seconds: u64,
    source_revision: String,
    source_clean: bool,
    binary_path: PathBuf,
    binary_sha256: String,
    detail: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecordedLiveMarketGateOutcome {
    Passed,
    Failed,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RecordedLiveMarketGateCompletion {
    Incomplete,
    Completed,
}

/// Reads what each live market gate recorded, and reports the worst of them.
///
/// A failure anywhere is a failure; a gate that has not run leaves the whole
/// result "not run", because a provider nobody exercised cannot be reported as
/// working on the strength of another one that was.
fn live_market_gate() -> LiveMarketGate {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Some(source_revision) = clean_source_revision(&repository) else {
        return LiveMarketGate::NotRun;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut combined = LiveMarketGate::Passed;
    for (provider, report) in LIVE_GATE_REPORTS {
        match recorded_gate(&repository.join(report), provider, &source_revision, now) {
            LiveMarketGate::Failed => return LiveMarketGate::Failed,
            LiveMarketGate::NotRun => combined = LiveMarketGate::NotRun,
            LiveMarketGate::Passed => {}
        }
    }
    combined
}

fn recorded_gate(
    path: &Path,
    expected_provider: &str,
    expected_source_revision: &str,
    now_unix_seconds: u64,
) -> LiveMarketGate {
    let Ok(file) = fs::File::open(path) else {
        return LiveMarketGate::NotRun;
    };
    let mut contents = Vec::new();
    if file
        .take(LIVE_GATE_MAXIMUM_REPORT_BYTES + 1)
        .read_to_end(&mut contents)
        .is_err()
        || u64::try_from(contents.len()).unwrap_or(u64::MAX) > LIVE_GATE_MAXIMUM_REPORT_BYTES
    {
        return LiveMarketGate::NotRun;
    }
    let Ok(report) = serde_json::from_slice::<RecordedLiveMarketGate>(&contents) else {
        return LiveMarketGate::NotRun;
    };
    let freshest_allowed = now_unix_seconds.saturating_add(LIVE_GATE_MAXIMUM_FUTURE_SKEW_SECONDS);
    let age = now_unix_seconds.saturating_sub(report.recorded_at_unix_seconds);
    let expected_binary_name = format!("live_market_gate_{expected_provider}.bin");
    if report.schema_version != LIVE_GATE_SCHEMA_VERSION
        || report.evidence_scope != LIVE_GATE_EVIDENCE_SCOPE
        || report.provider != expected_provider
        || report.completion_state != RecordedLiveMarketGateCompletion::Completed
        || !report.source_clean
        || report.source_revision != expected_source_revision
        || report.recorded_at_unix_seconds > freshest_allowed
        || age > LIVE_GATE_MAXIMUM_AGE_SECONDS
        || report.detail.len() > 4 * 1_024
        || report.binary_path.to_str() != Some(expected_binary_name.as_str())
        || report.binary_sha256.len() != 64
        || !report
            .binary_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return LiveMarketGate::NotRun;
    }
    let Some(report_directory) = path.parent() else {
        return LiveMarketGate::NotRun;
    };
    let Ok(binary_sha256) = file_sha256_hex(&report_directory.join(&report.binary_path)) else {
        return LiveMarketGate::NotRun;
    };
    if !binary_sha256.eq_ignore_ascii_case(&report.binary_sha256) {
        return LiveMarketGate::NotRun;
    }
    match report.outcome {
        RecordedLiveMarketGateOutcome::Passed => LiveMarketGate::Passed,
        RecordedLiveMarketGateOutcome::Failed => LiveMarketGate::Failed,
    }
}

fn clean_source_revision(repository: &Path) -> Option<String> {
    let revision = git_output(repository, &["rev-parse", "HEAD"])?;
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let status = git_output(
        repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    status.is_empty().then_some(revision)
}

fn git_output(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

fn file_sha256_hex(path: &Path) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 16 * 1_024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(lower_hex(hasher.finalize()))
}

fn lower_hex(bytes: impl IntoIterator<Item = u8>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.into_iter();
    let mut encoded = String::with_capacity(bytes.size_hint().0.saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[derive(Serialize)]
struct DesktopBurstEvidence {
    schema_version: u32,
    evidence_scope: &'static str,
    /// What the live gate reported, carried through so a reader never has to
    /// infer venue behaviour from a fixture run.
    live_market_gate: LiveMarketGate,
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
    chart_ordering_fault_rejected: bool,
    history: HistoryGapRecoveryEvidence,
    depth: DepthGapRecoveryEvidence,
    generation_fencing: GenerationFencingEvidence,
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

#[derive(Serialize)]
struct GenerationFencingEvidence {
    chart: ChartGenerationFencingEvidence,
    history: HistoryGenerationFencingEvidence,
    dom: DomGenerationFencingEvidence,
}

#[derive(Serialize)]
struct ChartGenerationFencingEvidence {
    stale_chart_snapshot_rejected_without_mutation: bool,
}

#[derive(Serialize)]
struct HistoryGenerationFencingEvidence {
    stale_history_snapshot_rejected: bool,
    newer_history_snapshot_recovers: bool,
}

#[derive(Serialize)]
struct DomGenerationFencingEvidence {
    retired_dom_selection_ignored_without_mutation: bool,
    current_dom_selection_recovers: bool,
}

pub(super) struct ProcessMemoryProbe {
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
    /// The synthetic bounds held for the full qualification window. This is a
    /// statement about the mailbox, the frame gate, and the working set under a
    /// fixture load — not about the desktop against a venue.
    synthetic_bounds_qualified: bool,
    live_market_gate: LiveMarketGate,
}

impl ProcessMemoryProbe {
    pub(super) fn new() -> Result<Self, Box<dyn Error>> {
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

    pub(super) fn sample(&mut self) -> Result<(), Box<dyn Error>> {
        self.system
            .refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
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
                update: ReplayStreamUpdate::Snapshot(snapshot.clone()),
            })
            .map_err(|_| "desktop burst mailbox disconnected")?;
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
    let accepted_frame_requests_while_pending =
        (0..BURST_UPDATES).filter(|_| gate.try_schedule()).count();
    gate.complete();
    let accepted_frame_requests_after_completion = usize::from(gate.try_schedule());

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
        schema_version: 4,
        evidence_scope: "deterministic_desktop_burst_and_frame_conflation",
        live_market_gate: live_market_gate(),
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
        chart_ordering_fault_rejected: collect_chart_ordering_evidence()?,
        history: collect_history_gap_evidence()?,
        depth: collect_depth_gap_evidence()?,
        generation_fencing: collect_generation_fencing_evidence()?,
    };
    if !evidence.chart_ordering_fault_rejected
        || !evidence.history.history_gap_requires_snapshot
        || !evidence.history.history_covering_snapshot_recovers
        || !evidence.depth.depth_gap_clears_book
        || !evidence.depth.depth_covering_snapshot_recovers
        || !evidence
            .generation_fencing
            .chart
            .stale_chart_snapshot_rejected_without_mutation
        || !evidence
            .generation_fencing
            .history
            .stale_history_snapshot_rejected
        || !evidence
            .generation_fencing
            .history
            .newer_history_snapshot_recovers
        || !evidence
            .generation_fencing
            .dom
            .retired_dom_selection_ignored_without_mutation
        || !evidence
            .generation_fencing
            .dom
            .current_dom_selection_recovers
    {
        return Err("desktop gap recovery contract failed".into());
    }
    Ok(evidence)
}

fn collect_chart_ordering_evidence() -> Result<bool, Box<dyn Error>> {
    let mut fixture = FixtureMarketWorker::try_new()?;
    let snapshot = fixture.publish_snapshot(2)?.snapshot;
    let mut model = MarketBarClientModel::new(NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN));
    model.apply_update(ReplayStreamUpdate::Snapshot(snapshot.clone()))?;
    let before = model.current_generation().cloned();
    Ok(model
        .apply_update(ReplayStreamUpdate::Snapshot(snapshot))
        .is_err()
        && model.current_generation() == before.as_ref())
}

fn collect_generation_fencing_evidence() -> Result<GenerationFencingEvidence, Box<dyn Error>> {
    let mut fixture = FixtureMarketWorker::try_new()?;
    let stale_snapshot = fixture.publish_snapshot(2)?.snapshot;
    let current_snapshot = fixture.publish_snapshot(2)?.snapshot;
    let mut model = MarketBarClientModel::new(NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN));
    let installed = model.apply_update(ReplayStreamUpdate::Snapshot(stale_snapshot.clone()))?;
    if !matches!(installed, MarketBarModelOutcome::Published(_)) {
        return Err("initial chart snapshot was not published".into());
    }
    let advanced = model.apply_update(ReplayStreamUpdate::Snapshot(current_snapshot))?;
    if !matches!(advanced, MarketBarModelOutcome::Published(_)) {
        return Err("newer chart snapshot was not published".into());
    }
    let before = model.current_generation().cloned();
    let stale_chart_snapshot_rejected_without_mutation = model
        .apply_update(ReplayStreamUpdate::Snapshot(stale_snapshot))
        .is_err()
        && model.current_generation() == before.as_ref();

    let source = EmbeddedReplaySource;
    let initial_history = source.load_snapshot(LoadEmbeddedReplay { bar_count: 2 })?;
    let mut history = MarketBarClientModel::new(NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN));
    history.apply_update(ReplayStreamUpdate::Snapshot(initial_history.clone()))?;
    let gap = source
        .load_delta(3)?
        .ok_or("embedded history fixture has no gap delta")?;
    let gap_outcome = history.apply_update(ReplayStreamUpdate::Delta(gap))?;
    let before_stale = history.current_generation().cloned();
    let stale_history_snapshot_rejected = history
        .apply_update(ReplayStreamUpdate::Snapshot(initial_history))
        .is_err()
        && history.current_generation() == before_stale.as_ref();
    let replacement_history = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })?
        .try_with_publication_generation(2)?;
    let newer_history_snapshot_recovers = matches!(
        history.apply_update(ReplayStreamUpdate::Snapshot(replacement_history))?,
        MarketBarModelOutcome::Published(generation)
            if matches!(gap_outcome, MarketBarModelOutcome::ResnapshotRequired(
                ResnapshotReason::SequenceGap
            )) && generation.sequence_range().1 == 4
    );

    let maximum_levels = NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN);
    let mut dom = ReadOnlyDom::new(maximum_levels);
    dom.select(dom_selection("mnq", 7, 1)?);
    dom.apply_event(&depth_snapshot_for("mnq", 7, 10))?;
    dom.select(dom_selection("es", 8, 2)?);
    let retired_outcome = dom.apply_event(&depth_snapshot_for("mnq", 7, 11))?;
    let retired_frame = dom
        .frame()
        .ok_or("DOM selection disappeared during generation fencing")?;
    let retired_dom_selection_ignored_without_mutation = retired_outcome
        == DomUpdateOutcome::Ignored
        && retired_frame.selection_generation == 2
        && retired_frame.session_generation == 8
        && retired_frame.source_watermark == 0
        && retired_frame.rows.is_empty();
    let current_dom_selection_recovers = matches!(
        dom.apply_event(&depth_snapshot_for("es", 8, 1))?,
        DomUpdateOutcome::Published(frame)
            if frame.selection_generation == 2
                && frame.session_generation == 8
                && frame.source_watermark == 1
                && !frame.rows.is_empty()
    );

    Ok(GenerationFencingEvidence {
        chart: ChartGenerationFencingEvidence {
            stale_chart_snapshot_rejected_without_mutation,
        },
        history: HistoryGenerationFencingEvidence {
            stale_history_snapshot_rejected,
            newer_history_snapshot_recovers,
        },
        dom: DomGenerationFencingEvidence {
            retired_dom_selection_ignored_without_mutation,
            current_dom_selection_recovers,
        },
    })
}

fn collect_history_gap_evidence() -> Result<HistoryGapRecoveryEvidence, Box<dyn Error>> {
    let source = EmbeddedReplaySource;
    let initial = source.load_snapshot(LoadEmbeddedReplay { bar_count: 2 })?;
    let mut history = MarketBarClientModel::new(NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN));
    history.apply_update(ReplayStreamUpdate::Snapshot(initial))?;
    let gap = source
        .load_delta(3)?
        .ok_or("embedded history fixture has no gap delta")?;
    let history_gap_requires_snapshot = matches!(
        history.apply_update(ReplayStreamUpdate::Delta(gap))?,
        MarketBarModelOutcome::ResnapshotRequired(ResnapshotReason::SequenceGap)
    );
    let replacement = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })?
        .try_with_publication_generation(2)?;
    let history_covering_snapshot_recovers = matches!(
        history.apply_update(ReplayStreamUpdate::Snapshot(replacement))?,
        MarketBarModelOutcome::Published(generation) if generation.sequence_range().1 == 4
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

fn depth_metadata(sequence: u64) -> EventMetadata {
    depth_metadata_for("mnq", 7, sequence)
}

fn depth_metadata_for(
    instrument_id: &str,
    session_generation: u64,
    sequence: u64,
) -> EventMetadata {
    EventMetadata {
        provider_id: "rithmic".to_string(),
        instrument_id: instrument_id.to_string(),
        entitlement_id: "test".to_string(),
        source_sequence: sequence,
        session_generation,
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: Some(i64::try_from(sequence).unwrap_or(i64::MAX)),
            provider_unix_nanos: None,
            received_unix_nanos: i64::try_from(sequence).unwrap_or(i64::MAX),
        },
    }
}

fn depth_snapshot(sequence: u64) -> MarketEvent {
    depth_snapshot_for("mnq", 7, sequence)
}

fn depth_snapshot_for(instrument_id: &str, session_generation: u64, sequence: u64) -> MarketEvent {
    MarketEvent::DepthSnapshot(DepthSnapshot {
        metadata: depth_metadata_for(instrument_id, session_generation, sequence),
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

fn dom_selection(
    instrument_id: &str,
    session_generation: u64,
    selection_generation: u64,
) -> Result<DomSelection, Box<dyn Error>> {
    Ok(DomSelection {
        provider_id: "rithmic".to_string(),
        instrument_id: instrument_id.to_string(),
        entitlement_id: "test".to_string(),
        session_generation,
        selection_generation,
        precision: InstrumentPrecision::try_new(2, 0)?,
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

#[cfg(feature = "diagnostics")]
pub(crate) fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    let report = collect_evidence()?;
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "desktop_burst_conformance=deterministic_passed live_market_gate={} updates={} retained={} frame_schedules={} report={}",
        live_gate_label(report.live_market_gate),
        report.burst_updates,
        report.retained_items,
        report.accepted_frame_requests_while_pending,
        report_path.display()
    );
    Ok(())
}

#[cfg(feature = "diagnostics")]
const fn live_gate_label(gate: LiveMarketGate) -> &'static str {
    match gate {
        LiveMarketGate::NotRun => "not_run",
        LiveMarketGate::Passed => "passed",
        LiveMarketGate::Failed => "failed",
    }
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
    let synthetic_bounds_qualified = completion_state == EnduranceCompletionState::Completed
        && duration == ENDURANCE_QUALIFICATION_DURATION
        && snapshot.elapsed >= ENDURANCE_QUALIFICATION_DURATION
        && snapshot.working_set_within_bound
        && snapshot.clean_stop;
    DesktopEnduranceEvidence {
        schema_version: 2,
        evidence_scope: "synthetic_headless_desktop_continuous_endurance",
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
        synthetic_bounds_qualified,
        live_market_gate: live_market_gate(),
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
                update: ReplayStreamUpdate::Snapshot(snapshot.clone()),
            })
            .map_err(|_| "desktop endurance mailbox disconnected")?;
        counters.updates_published = counters.updates_published.saturating_add(1);
    }
    counters.mailbox_high_water_items = counters.mailbox_high_water_items.max(sender.occupancy().0);
    if !gate.try_schedule() {
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

#[cfg(feature = "diagnostics")]
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
        "desktop_endurance=synthetic_completed synthetic_bounds_qualified={} live_market_gate={} seconds={} frames={} updates={} memory_high_water={} report={}",
        report.synthetic_bounds_qualified,
        live_gate_label(report.live_market_gate),
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
        collect_generation_fencing_evidence, collect_history_gap_evidence, endurance_evidence,
        file_sha256_hex, recorded_gate, write_endurance_evidence_atomically,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn live_gate_report(
        binary_name: &std::path::Path,
        outcome: &str,
        completion_state: &str,
        provider: &str,
        revision: &str,
        recorded_at: u64,
        binary_sha256: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "evidence_scope": "engine_live_market_gate",
            "provider": provider,
            "outcome": outcome,
            "completion_state": completion_state,
            "recorded_at_unix_seconds": recorded_at,
            "source_revision": revision,
            "source_clean": true,
            "binary_path": binary_name,
            "binary_sha256": binary_sha256,
            "detail": "fixture"
        })
    }

    fn write_json(path: &std::path::Path, value: &serde_json::Value) {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(value).expect("report fixture serializes"),
        )
        .expect("report fixture is written");
    }

    struct LiveGateFixture {
        directory: std::path::PathBuf,
        report_path: std::path::PathBuf,
        binary_sha256: String,
        source_revision: String,
        now: u64,
    }

    impl LiveGateFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "axiusflow-live-gate-evidence-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time follows the Unix epoch")
                    .as_nanos()
            ));
            std::fs::create_dir(&directory).expect("evidence test directory is created");
            let binary_path = directory.join("live_market_gate_coinbase.bin");
            std::fs::write(&binary_path, b"candidate binary")
                .expect("candidate binary fixture is written");
            let binary_sha256 = file_sha256_hex(&binary_path).expect("candidate binary is hashed");
            let report_path = directory.join("live_market_gate_coinbase.json");
            Self {
                directory,
                report_path,
                binary_sha256,
                source_revision: "a".repeat(40),
                now: 2_000_000,
            }
        }

        fn report(&self, outcome: &str, completion: &str) -> serde_json::Value {
            live_gate_report(
                std::path::Path::new("live_market_gate_coinbase.bin"),
                outcome,
                completion,
                "coinbase",
                &self.source_revision,
                self.now,
                &self.binary_sha256,
            )
        }
    }

    impl Drop for LiveGateFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn run_provider_neutral_handoff_scenario(provider: &str) {
        let recovery = collect_history_gap_evidence()
            .unwrap_or_else(|error| panic!("{provider} hydration failed: {error}"));
        assert!(
            recovery.history_gap_requires_snapshot,
            "{provider} must latch a sequence gap"
        );
        assert!(
            recovery.history_covering_snapshot_recovers,
            "{provider} must recover from a covering snapshot"
        );
        let fencing = collect_generation_fencing_evidence()
            .unwrap_or_else(|error| panic!("{provider} fencing failed: {error}"));
        assert!(
            fencing.history.stale_history_snapshot_rejected,
            "{provider} must reject stale history"
        );
        assert!(
            fencing.history.newer_history_snapshot_recovers,
            "{provider} must accept newer covering history"
        );
    }

    #[test]
    fn provider_neutral_hydration_fencing_and_recovery_match_both_boundaries() {
        for provider in ["coinbase", "rithmic"] {
            run_provider_neutral_handoff_scenario(provider);
        }
    }

    #[test]
    fn frame_gate_coalesces_requests_without_consulting_window_focus() {
        // The gate must never be the reason a visible chart stops draining its
        // feed. It takes no focus argument precisely so that polling cannot be
        // switched off by the trader looking at another window.
        let mut gate = FramePollGate::default();
        assert!(gate.try_schedule());
        assert!(!gate.try_schedule());
        gate.complete();
        assert!(gate.try_schedule());
    }

    #[test]
    fn live_gate_accepts_only_completed_outcomes() {
        let fixture = LiveGateFixture::new();
        write_json(&fixture.report_path, &fixture.report("passed", "completed"));
        assert_eq!(
            recorded_gate(
                &fixture.report_path,
                "coinbase",
                &fixture.source_revision,
                fixture.now
            ),
            super::LiveMarketGate::Passed
        );

        write_json(&fixture.report_path, &fixture.report("failed", "completed"));
        assert_eq!(
            recorded_gate(
                &fixture.report_path,
                "coinbase",
                &fixture.source_revision,
                fixture.now
            ),
            super::LiveMarketGate::Failed
        );
    }

    #[test]
    fn live_gate_rejects_interrupted_stale_or_mismatched_evidence() {
        let fixture = LiveGateFixture::new();
        for invalid in [
            fixture.report("passed", "incomplete"),
            live_gate_report(
                std::path::Path::new("live_market_gate_coinbase.bin"),
                "passed",
                "completed",
                "rithmic",
                &fixture.source_revision,
                fixture.now,
                &fixture.binary_sha256,
            ),
            live_gate_report(
                std::path::Path::new("live_market_gate_coinbase.bin"),
                "passed",
                "completed",
                "coinbase",
                &"b".repeat(40),
                fixture.now,
                &fixture.binary_sha256,
            ),
            live_gate_report(
                std::path::Path::new("live_market_gate_coinbase.bin"),
                "passed",
                "completed",
                "coinbase",
                &fixture.source_revision,
                fixture.now - super::LIVE_GATE_MAXIMUM_AGE_SECONDS - 1,
                &fixture.binary_sha256,
            ),
            live_gate_report(
                std::path::Path::new("live_market_gate_coinbase.bin"),
                "passed",
                "completed",
                "coinbase",
                &fixture.source_revision,
                fixture.now,
                &"0".repeat(64),
            ),
        ] {
            write_json(&fixture.report_path, &invalid);
            assert_eq!(
                recorded_gate(
                    &fixture.report_path,
                    "coinbase",
                    &fixture.source_revision,
                    fixture.now
                ),
                super::LiveMarketGate::NotRun
            );
        }

        std::fs::write(&fixture.report_path, br#"{"outcome":"passed"}"#)
            .expect("malformed evidence fixture is written");
        assert_eq!(
            recorded_gate(
                &fixture.report_path,
                "coinbase",
                &fixture.source_revision,
                fixture.now
            ),
            super::LiveMarketGate::NotRun
        );
    }

    #[test]
    fn burst_retains_only_the_latest_generation() {
        let evidence = collect_evidence().expect("burst evidence passes");
        assert_eq!(evidence.retained_items, 1);
        assert_eq!(evidence.retained_series_generation, BURST_UPDATES);
        assert!(evidence.bounded_latest_state_conflation);
        assert!(evidence.single_frame_drain_gate);
        assert!(evidence.working_set_within_bound);
        assert!(evidence.gap_recovery.chart_ordering_fault_rejected);
        assert!(
            evidence
                .gap_recovery
                .history
                .history_covering_snapshot_recovers
        );
        assert!(evidence.gap_recovery.depth.depth_covering_snapshot_recovers);
        assert!(
            evidence
                .gap_recovery
                .generation_fencing
                .chart
                .stale_chart_snapshot_rejected_without_mutation
        );
        assert!(
            evidence
                .gap_recovery
                .generation_fencing
                .dom
                .retired_dom_selection_ignored_without_mutation
        );
    }

    #[test]
    fn gaps_fail_closed_and_covering_snapshots_recover() {
        let evidence = collect_gap_recovery_evidence().expect("gap recovery evidence passes");
        assert!(evidence.chart_ordering_fault_rejected);
        assert!(evidence.history.history_gap_requires_snapshot);
        assert!(evidence.history.history_covering_snapshot_recovers);
        assert!(evidence.depth.depth_gap_clears_book);
        assert!(evidence.depth.depth_covering_snapshot_recovers);
        assert!(
            evidence
                .generation_fencing
                .chart
                .stale_chart_snapshot_rejected_without_mutation
        );
        assert!(
            evidence
                .generation_fencing
                .history
                .stale_history_snapshot_rejected
        );
        assert!(
            evidence
                .generation_fencing
                .history
                .newer_history_snapshot_recovers
        );
        assert!(
            evidence
                .generation_fencing
                .dom
                .retired_dom_selection_ignored_without_mutation
        );
        assert!(
            evidence
                .generation_fencing
                .dom
                .current_dom_selection_recovers
        );
    }

    #[test]
    fn stale_generations_never_mutate_current_models() {
        let evidence =
            collect_generation_fencing_evidence().expect("generation fencing evidence passes");
        assert!(
            evidence
                .chart
                .stale_chart_snapshot_rejected_without_mutation
        );
        assert!(evidence.history.stale_history_snapshot_rejected);
        assert!(evidence.history.newer_history_snapshot_recovers);
        assert!(evidence.dom.retired_dom_selection_ignored_without_mutation);
        assert!(evidence.dom.current_dom_selection_recovers);
    }

    #[test]
    fn short_endurance_preserves_continuity_and_bounds() {
        // The window carries scheduling margin: an 80 ms window managed a
        // single frame on a loaded shared macOS runner, which is runner
        // starvation rather than a broken loop. The continuity and bounds
        // assertions below are the actual subject; the frame count only
        // guards against a vacuous run.
        let evidence = super::collect_endurance(std::time::Duration::from_millis(320))
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
        assert!(!evidence.synthetic_bounds_qualified);
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
        assert_eq!(persisted["synthetic_bounds_qualified"], false);
        // A conformance report never claims a live gate it did not run.
        assert_eq!(persisted["live_market_gate"], "not_run");
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
        assert!(exact.synthetic_bounds_qualified);

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
        assert!(!short.synthetic_bounds_qualified);

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
        assert!(!unclean.synthetic_bounds_qualified);
    }
}
