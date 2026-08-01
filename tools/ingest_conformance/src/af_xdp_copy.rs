//! Privileged Linux `AF_XDP` generic/copy-mode conformance.
//!
//! This lane receives real Ethernet frames through `AF_XDP`. It proves only software-accessible
//! copy-mode behavior on a veth pair and never claims zero-copy, hardware, provider, or production
//! readiness.

use axiusflow_linux_af_xdp_adapter::{AfXdpConfig, AfXdpCopyDriver, COPY_DRIVER_EVIDENCE_ID};
use axiusflow_testing::{
    ConformanceHarnessError, ConformanceOutcome, deterministic_ingest_corpus,
    deterministic_market_bar_packet_corpus, ingest_outcomes_semantically_equivalent,
    run_ingest_conformance, run_ingest_conformance_after_start,
    run_market_bar_packet_to_origin_conformance_after_start,
};
use axiusflow_transport::{
    ActiveIngestMode, DriverLifecycle, FixtureFrame, IngestDriver, QueueBinding, ReadinessState,
    ReceiveBatch,
};
use serde::Serialize;
use std::{
    env,
    error::Error,
    fs::{self, File},
    io::Write,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_1_af_xdp_copy_conformance";
const ETHERNET_HEADER_BYTES: usize = 14;
const AXIUSFLOW_EXPERIMENTAL_ETHERTYPE: [u8; 2] = [0x88, 0xb5];
const FRAME_COUNT: usize = 64;
const MAXIMUM_FRAME_BYTES: usize = 2_048;
const REPLAY_SENDER_SOURCE: &str = include_str!("../send_pcap.py");
const XSK_RS_VERSION: &str = "0.8.0";
const XSK_RS_SOURCE_COMMIT: &str = "c0b110cd3b6763fdcfc41996b3cea8c9f259614f";
const XSK_RS_REGISTRY_CHECKSUM: &str =
    "d1fef46e3505c5055082f52ada0a7f8e5dcaebdbb9eccf8e978c32382c159270";
static PCAP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
struct TargetEvidence {
    os: &'static str,
    architecture: &'static str,
    family: &'static str,
}

#[derive(Serialize)]
struct DependencyEvidence {
    name: &'static str,
    version: &'static str,
    source_commit: &'static str,
    registry_checksum: &'static str,
    libxdp_sys_version: &'static str,
    libxdp_sys_registry_checksum: &'static str,
    reviewed_evidence_id: &'static str,
}

#[derive(Serialize)]
struct ModeEvidence {
    active_mode: &'static str,
    generic_skb_mode_forced: bool,
    copy_mode_forced: bool,
    zero_copy_verified: bool,
}

#[derive(Serialize)]
struct ConformanceEvidence {
    native_socket_opened: &'static str,
    bounded_umem_and_rings: &'static str,
    release_recycles_descriptors: &'static str,
    abandoned_batch_recycles_descriptors: &'static str,
    deterministic_packet_semantics: &'static str,
    sequence_gap_replay_and_error_semantics: &'static str,
    packet_to_fenced_partition: &'static str,
    bounded_direct_and_durable_tap_fanout: &'static str,
    model_and_origin_equivalence: &'static str,
    gpui_host_preparation: &'static str,
    renderer_submission: &'static str,
    physical_presentation: &'static str,
}

#[derive(Serialize)]
struct ClaimEvidence {
    zero_copy: &'static str,
    hardware: &'static str,
    provider: &'static str,
    production: &'static str,
}

#[derive(Serialize)]
struct AfXdpCopyEvidenceReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    target: TargetEvidence,
    profile: &'static str,
    readiness: &'static str,
    receive_interface: String,
    transmit_interface: String,
    dependency: DependencyEvidence,
    mode: ModeEvidence,
    conformance: ConformanceEvidence,
    generic_corpus_outcomes: usize,
    generic_corpus_accepted_events: usize,
    market_bar_corpus_outcomes: usize,
    market_bar_corpus_accepted_events: usize,
    claims: ClaimEvidence,
    limitations: [&'static str; 5],
}

/// Runs privileged veth copy-mode evidence and writes one source-revision-bound artifact.
pub fn run(
    receive_interface: &str,
    transmit_interface: &str,
    report_path: &Path,
) -> Result<(), Box<dyn Error>> {
    require_interface_name(receive_interface, "receive")?;
    require_interface_name(transmit_interface, "transmit")?;
    if receive_interface == transmit_interface {
        return Err("AF_XDP receive and transmit interfaces must differ".into());
    }
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for privileged AF_XDP evidence")?;

    let fixture_frames = deterministic_ingest_corpus();
    let mut fixture = axiusflow_linux_af_xdp_adapter::fixture_driver(fixture_frames.clone())?;
    let fixture_outcomes = run_ingest_conformance(&mut fixture)?;

    verify_abandoned_descriptor_recycle(receive_interface, transmit_interface, &fixture_frames[0])
        .map_err(|error| format!("initial AF_XDP native lifecycle failed: {error}"))?;

    let mut native = copy_driver(receive_interface)?;
    require_copy_permit(&native)?;
    let native_outcomes = run_ingest_conformance_after_start(&mut native, |_| {
        replay_frames(transmit_interface, &fixture_frames)
    })
    .map_err(|error| format!("released-batch AF_XDP native lifecycle failed: {error}"))?;
    if !ingest_outcomes_semantically_equivalent(&fixture_outcomes, &native_outcomes) {
        return Err("AF_XDP deterministic packet outcomes diverged from fixture semantics".into());
    }
    let native_health = native.health();
    if native_health.lifecycle != DriverLifecycle::Stopped
        || native_health.active_mode != ActiveIngestMode::AfXdpCopy
        || native_health.released_batches == 0
        || native_health.abandoned_batches != 0
    {
        return Err("AF_XDP released-batch lifecycle evidence was incomplete".into());
    }

    let market_corpus = deterministic_market_bar_packet_corpus()?;
    let mut market_fixture =
        axiusflow_linux_af_xdp_adapter::fixture_driver(market_corpus.frames().to_vec())?;
    let fixture_market = run_market_bar_packet_to_origin_conformance_after_start(
        &mut market_fixture,
        &market_corpus,
        |_, _| Ok(()),
    )?;
    let mut market_native = copy_driver(receive_interface)?;
    let native_market = run_market_bar_packet_to_origin_conformance_after_start(
        &mut market_native,
        &market_corpus,
        |_, frames| replay_frames(transmit_interface, frames),
    )
    .map_err(|error| format!("market-bar AF_XDP native lifecycle failed: {error}"))?;
    if !fixture_market.semantically_equivalent(&native_market) {
        return Err(
            "AF_XDP market-bar partition/fanout/Origin state diverged from fixture semantics"
                .into(),
        );
    }

    write_success_report(
        report_path,
        source_revision,
        receive_interface,
        transmit_interface,
        native_outcomes.len(),
        accepted_count(&native_outcomes),
        native_market.outcome_count(),
        native_market.accepted_canonical_event_count(),
    )?;
    println!(
        "af_xdp_copy_conformance=passed readiness=implemented active_mode=af_xdp_copy generic_skb=true forced_copy=true zero_copy=false hardware_claims=false provider_claims=false production_claims=false report={}",
        report_path.display()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_success_report(
    report_path: &Path,
    source_revision: String,
    receive_interface: &str,
    transmit_interface: &str,
    generic_corpus_outcomes: usize,
    generic_corpus_accepted_events: usize,
    market_bar_corpus_outcomes: usize,
    market_bar_corpus_accepted_events: usize,
) -> Result<(), Box<dyn Error>> {
    let report = AfXdpCopyEvidenceReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        target: TargetEvidence {
            os: env::consts::OS,
            architecture: env::consts::ARCH,
            family: env::consts::FAMILY,
        },
        profile: "linux_af_xdp",
        readiness: "implemented",
        receive_interface: receive_interface.to_string(),
        transmit_interface: transmit_interface.to_string(),
        dependency: DependencyEvidence {
            name: "xsk-rs",
            version: XSK_RS_VERSION,
            source_commit: XSK_RS_SOURCE_COMMIT,
            registry_checksum: XSK_RS_REGISTRY_CHECKSUM,
            libxdp_sys_version: "0.2.4+1.6.0",
            libxdp_sys_registry_checksum: "6098c8281e42ed6f46240af889297dae1e37f70ee505dd26fe5c7199563e4d86",
            reviewed_evidence_id: COPY_DRIVER_EVIDENCE_ID,
        },
        mode: ModeEvidence {
            active_mode: "af_xdp_copy",
            generic_skb_mode_forced: true,
            copy_mode_forced: true,
            zero_copy_verified: false,
        },
        conformance: ConformanceEvidence {
            native_socket_opened: "passed",
            bounded_umem_and_rings: "passed",
            release_recycles_descriptors: "passed",
            abandoned_batch_recycles_descriptors: "passed",
            deterministic_packet_semantics: "passed",
            sequence_gap_replay_and_error_semantics: "passed",
            packet_to_fenced_partition: "passed",
            bounded_direct_and_durable_tap_fanout: "passed",
            model_and_origin_equivalence: "passed",
            gpui_host_preparation: "passed",
            renderer_submission: "not_measured",
            physical_presentation: "not_measured",
        },
        generic_corpus_outcomes,
        generic_corpus_accepted_events,
        market_bar_corpus_outcomes,
        market_bar_corpus_accepted_events,
        claims: ClaimEvidence {
            zero_copy: "not_claimed",
            hardware: "not_claimed",
            provider: "not_claimed",
            production: "not_claimed",
        },
        limitations: [
            "independent_unsafe_boundary_audit_and_fuzzing_missing",
            "qualified_host_kernel_matrix_missing",
            "zero_copy_not_verified",
            "nic_driver_firmware_unqualified",
            "no_authorized_packet_feed",
        ],
    };
    write_report(report_path, &report)
}

fn copy_driver(interface: &str) -> Result<AfXdpCopyDriver, Box<dyn Error>> {
    let config = AfXdpConfig::try_new(
        interface,
        0,
        NonZeroUsize::new(FRAME_COUNT).ok_or("AF_XDP frame count cannot be zero")?,
        NonZeroUsize::new(MAXIMUM_FRAME_BYTES).ok_or("AF_XDP frame limit cannot be zero")?,
    )?;
    Ok(AfXdpCopyDriver::try_new(config)?)
}

fn require_copy_permit(driver: &AfXdpCopyDriver) -> Result<(), Box<dyn Error>> {
    if driver.permit().readiness() != ReadinessState::Implemented
        || driver.permit().active_mode() != ActiveIngestMode::AfXdpCopy
        || driver.permit().evidence_id() != COPY_DRIVER_EVIDENCE_ID
        || driver.capabilities().zero_copy_verified
    {
        return Err("AF_XDP copy driver permit or capabilities overclaimed readiness".into());
    }
    Ok(())
}

fn verify_abandoned_descriptor_recycle(
    receive_interface: &str,
    transmit_interface: &str,
    frame: &FixtureFrame,
) -> Result<(), Box<dyn Error>> {
    let mut driver = copy_driver(receive_interface)?;
    driver.bind_queue(QueueBinding {
        queue_id: 0,
        maximum_batch_items: NonZeroUsize::MIN,
        maximum_frame_bytes: NonZeroUsize::new(MAXIMUM_FRAME_BYTES)
            .ok_or("AF_XDP frame limit cannot be zero")?,
    })?;
    driver.start()?;
    replay_frames(transmit_interface, std::slice::from_ref(frame))?;
    let batch = driver.receive_batch()?;
    let frame_count = batch.frame_count();
    let overflow = batch.overflow();
    if frame_count != 1 {
        return Err(format!(
            "AF_XDP abandonment evidence received {frame_count} frames; dropped_frames={}, dropped_bytes={}",
            overflow.dropped_frames, overflow.dropped_bytes,
        )
        .into());
    }
    drop(batch);
    driver.shutdown()?;
    let health = driver.health();
    if health.lifecycle != DriverLifecycle::Stopped
        || health.abandoned_batches != 1
        || health.released_batches != 0
        || health.queued_frames != 0
    {
        return Err("AF_XDP abandoned batch did not recycle descriptor ownership".into());
    }
    Ok(())
}

fn replay_frames(
    transmit_interface: &str,
    frames: &[FixtureFrame],
) -> Result<(), ConformanceHarnessError> {
    let path = temporary_pcap_path();
    write_pcap(&path, frames).map_err(|error| {
        context_error(&format!("write replay capture {}", path.display()), error)
    })?;
    let sender = temporary_sender_path();
    fs::write(&sender, REPLAY_SENDER_SOURCE).map_err(|error| {
        context_error(&format!("write replay sender {}", sender.display()), error)
    })?;
    let interpreter = replay_interpreter();
    let output = Command::new(&interpreter)
        .arg(&sender)
        .arg(transmit_interface)
        .arg(&path)
        .output()
        .map_err(|error| {
            context_error(
                &format!(
                    "spawn {} {} {transmit_interface}",
                    interpreter.display(),
                    sender.display()
                ),
                error,
            )
        });
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&sender);
    let output = output?;
    if !output.status.success() {
        return Err(ConformanceHarnessError::Driver(format!(
            "AF_PACKET replay failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let successful_packets = stdout
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("successful_packets=")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .ok_or_else(|| {
            ConformanceHarnessError::Driver(format!(
                "AF_PACKET replay did not report a successful packet count: {}",
                stdout.trim()
            ))
        })?;
    if successful_packets != frames.len() {
        return Err(ConformanceHarnessError::Driver(format!(
            "AF_PACKET replay sent {successful_packets} of {} fixture packets: {}",
            frames.len(),
            stdout.trim()
        )));
    }
    Ok(())
}

fn write_pcap(path: &Path, frames: &[FixtureFrame]) -> Result<(), Box<dyn Error>> {
    let mut file = File::create(path)?;
    file.write_all(&0xa1b2_c3d4_u32.to_le_bytes())?;
    file.write_all(&2_u16.to_le_bytes())?;
    file.write_all(&4_u16.to_le_bytes())?;
    file.write_all(&0_i32.to_le_bytes())?;
    file.write_all(&0_u32.to_le_bytes())?;
    file.write_all(&65_535_u32.to_le_bytes())?;
    file.write_all(&1_u32.to_le_bytes())?;
    for (index, frame) in frames.iter().enumerate() {
        let ethernet = ethernet_frame(&frame.bytes);
        let length = u32::try_from(ethernet.len())?;
        file.write_all(&0_u32.to_le_bytes())?;
        file.write_all(&u32::try_from(index)?.to_le_bytes())?;
        file.write_all(&length.to_le_bytes())?;
        file.write_all(&length.to_le_bytes())?;
        file.write_all(&ethernet)?;
    }
    file.flush()?;
    Ok(())
}

fn ethernet_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(ETHERNET_HEADER_BYTES.saturating_add(payload.len()));
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&AXIUSFLOW_EXPERIMENTAL_ETHERTYPE);
    frame.extend_from_slice(payload);
    frame
}

fn temporary_pcap_path() -> PathBuf {
    let sequence = PCAP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "axiusflow-af-xdp-{}-{sequence}.pcap",
        std::process::id()
    ))
}

fn temporary_sender_path() -> PathBuf {
    let sequence = PCAP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "axiusflow-af-xdp-send-{}-{sequence}.py",
        std::process::id()
    ))
}

fn write_report(path: &Path, report: &AfXdpCopyEvidenceReport) -> Result<(), Box<dyn Error>> {
    let mut encoded = serde_json::to_vec_pretty(report)?;
    encoded.push(b'\n');
    fs::write(path, encoded)?;
    Ok(())
}

fn accepted_count(outcomes: &[ConformanceOutcome]) -> usize {
    outcomes
        .iter()
        .filter(|outcome| matches!(outcome, ConformanceOutcome::Accepted(_)))
        .count()
}

fn require_interface_name(value: &str, role: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty() || value.len() > 15 || value.chars().any(char::is_whitespace) {
        return Err(format!("AF_XDP {role} interface name is invalid").into());
    }
    Ok(())
}

fn context_error(operation: &str, error: impl std::fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::Driver(format!("failed to {operation}: {error}"))
}

/// Resolves the replay interpreter without depending on the inherited `PATH`.
///
/// The privileged harness runs with a read-only root and a reduced environment, where a
/// bare `python3` lookup is not guaranteed. Absolute candidates are preferred, and a
/// `PATH` lookup remains the final fallback.
fn replay_interpreter() -> PathBuf {
    const CANDIDATES: [&str; 3] = ["/usr/bin/python3", "/usr/local/bin/python3", "/bin/python3"];
    CANDIDATES
        .iter()
        .map(Path::new)
        .find(|candidate| candidate.is_file())
        .map_or_else(|| PathBuf::from("python3"), Path::to_path_buf)
}
