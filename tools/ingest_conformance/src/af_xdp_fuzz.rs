//! Privileged seeded adversarial data-path exercise for the `AF_XDP` copy driver.
//!
//! This lane drives the live copy-mode socket with a reproducible stream of malformed,
//! runt, oversized, and randomly shaped Ethernet frames and asserts the data-path
//! invariants the unsafe boundary must uphold: batch bounds, per-frame size bounds,
//! monotonic overflow accounting, exact release accounting, clean shutdown, and a
//! successful rebind of the same queue after the exercise. It is a seeded adversarial
//! exercise, not coverage-guided fuzzing, and it never claims zero-copy, hardware,
//! provider, or production readiness.

use crate::af_xdp_replay::{
    AXIUSFLOW_EXPERIMENTAL_ETHERTYPE, RECEIVE_MAC, TRANSMIT_MAC, replay_ethernet_frames,
    require_interface_name,
};
use axiusflow_linux_af_xdp_adapter::{AfXdpConfig, AfXdpCopyDriver, COPY_DRIVER_EVIDENCE_ID};
use axiusflow_transport::{
    ActiveIngestMode, DriverLifecycle, IngestDriver, QueueBinding, ReadinessState, ReceiveBatch,
};
use serde::Serialize;
use std::{
    env,
    error::Error,
    fs,
    num::NonZeroUsize,
    path::Path,
    time::{Duration, Instant},
};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_1_af_xdp_copy_data_path_fuzz";
const FRAME_COUNT: usize = 64;
const MAXIMUM_FRAME_BYTES: usize = 2_048;
const MAXIMUM_BATCH_ITEMS: usize = 16;
const MAXIMUM_ROUND_FRAMES: u64 = 64;
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);
const EMPTY_POLLS_PER_ROUND: usize = 5;
const FRAME_CLASSES: u64 = 5;

#[derive(Serialize)]
struct FuzzInvariantEvidence {
    batch_bound_respected: &'static str,
    frame_size_bound_respected: &'static str,
    overflow_accounting_monotonic: &'static str,
    every_batch_released: &'static str,
    clean_shutdown: &'static str,
    post_exercise_rebind: &'static str,
}

#[derive(Serialize)]
struct FuzzClaimEvidence {
    coverage_guided: &'static str,
    zero_copy: &'static str,
    hardware: &'static str,
    provider: &'static str,
    production: &'static str,
}

#[derive(Serialize)]
struct AfXdpCopyFuzzReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    profile: &'static str,
    readiness: &'static str,
    receive_interface: String,
    transmit_interface: String,
    dependency_evidence_id: &'static str,
    seed: u64,
    rounds: u32,
    frames_sent: usize,
    frames_received: usize,
    batches_received: usize,
    runt_frames_sent: usize,
    oversized_frames_sent: usize,
    dropped_frames: u64,
    dropped_bytes: u64,
    invariants: FuzzInvariantEvidence,
    claims: FuzzClaimEvidence,
    limitations: [&'static str; 3],
}

/// Deterministic xorshift64* generator so every exercise is reproducible by seed.
struct SeededRandom(u64);

impl SeededRandom {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        debug_assert!(bound > 0);
        self.next() % bound
    }

    fn below_usize(&mut self, bound: u64) -> usize {
        debug_assert!(bound > 0);
        usize::try_from(self.next() % bound).unwrap_or(0)
    }

    fn byte(&mut self) -> u8 {
        (self.next() & 0xff) as u8
    }

    fn bytes(&mut self, count: usize) -> Vec<u8> {
        (0..count).map(|_| self.byte()).collect()
    }
}

/// Builds one adversarial frame of the requested class.
fn fuzz_frame(random: &mut SeededRandom, class: u64) -> Vec<u8> {
    match class % FRAME_CLASSES {
        0 => {
            let length = random.below_usize(513);
            let payload = random.bytes(length);
            addressed_frame(AXIUSFLOW_EXPERIMENTAL_ETHERTYPE, &payload)
        }
        1 => {
            let ethertype = [random.byte(), random.byte()];
            let length = random.below_usize(1_501);
            let payload = random.bytes(length);
            addressed_frame(ethertype, &payload)
        }
        2 => {
            let length = 14 + random.below_usize(47);
            random.bytes(length)
        }
        3 => {
            let ethertype = [random.byte(), random.byte()];
            let payload_bytes = 2_035 + random.below_usize(6_952);
            addressed_frame(ethertype, &random.bytes(payload_bytes))
        }
        _ => {
            let inner = [random.byte(), random.byte()];
            let length = random.below_usize(257);
            let payload = random.bytes(length);
            let mut frame = Vec::with_capacity(18 + payload.len());
            frame.extend_from_slice(&RECEIVE_MAC);
            frame.extend_from_slice(&TRANSMIT_MAC);
            frame.extend_from_slice(&[0x81, 0x00]);
            frame.extend_from_slice(&random.bytes(2));
            frame.extend_from_slice(&inner);
            frame.extend_from_slice(&payload);
            frame
        }
    }
}

fn addressed_frame(ethertype: [u8; 2], payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&RECEIVE_MAC);
    frame.extend_from_slice(&TRANSMIT_MAC);
    frame.extend_from_slice(&ethertype);
    frame.extend_from_slice(payload);
    frame
}

/// Runs the seeded adversarial data-path exercise and writes one evidence artifact.
pub fn run(
    receive_interface: &str,
    transmit_interface: &str,
    seed: u64,
    rounds: u32,
    report_path: &Path,
) -> Result<(), Box<dyn Error>> {
    require_interface_name(receive_interface, "receive")?;
    require_interface_name(transmit_interface, "transmit")?;
    if receive_interface == transmit_interface {
        return Err("AF_XDP fuzz receive and transmit interfaces must differ".into());
    }
    if rounds == 0 {
        return Err("AF_XDP fuzz rounds must be at least one".into());
    }
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for privileged AF_XDP fuzz evidence")?;

    let mut random = SeededRandom::new(seed);
    let mut frames_sent = 0_usize;
    let mut frames_received = 0_usize;
    let mut batches_received = 0_usize;
    let mut runt_sent = 0_usize;
    let mut oversized_sent = 0_usize;
    let mut dropped_frames = 0_u64;
    let mut dropped_bytes = 0_u64;

    let mut driver = copy_driver(receive_interface)?;
    require_copy_permit(&driver)?;
    exercise_rounds(
        &mut driver,
        transmit_interface,
        &mut random,
        rounds,
        &mut ExerciseTotals {
            frames_sent: &mut frames_sent,
            frames_received: &mut frames_received,
            batches_received: &mut batches_received,
            runt_sent: &mut runt_sent,
            oversized_sent: &mut oversized_sent,
            dropped_frames: &mut dropped_frames,
            dropped_bytes: &mut dropped_bytes,
        },
    )?;
    driver.shutdown()?;
    require_clean_health(&driver, "post-exercise")?;
    if frames_received == 0 {
        return Err("AF_XDP fuzz received no frames; the data path was not exercised".into());
    }
    if frames_received > frames_sent {
        return Err(format!(
            "AF_XDP fuzz received {frames_received} frames from {frames_sent} sent"
        )
        .into());
    }

    verify_post_exercise_rebind(receive_interface, transmit_interface, &mut random)?;

    let report = AfXdpCopyFuzzReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        profile: "linux_af_xdp",
        readiness: "implemented",
        receive_interface: receive_interface.to_string(),
        transmit_interface: transmit_interface.to_string(),
        dependency_evidence_id: COPY_DRIVER_EVIDENCE_ID,
        seed,
        rounds,
        frames_sent,
        frames_received,
        batches_received,
        runt_frames_sent: runt_sent,
        oversized_frames_sent: oversized_sent,
        dropped_frames,
        dropped_bytes,
        invariants: FuzzInvariantEvidence {
            batch_bound_respected: "passed",
            frame_size_bound_respected: "passed",
            overflow_accounting_monotonic: "passed",
            every_batch_released: "passed",
            clean_shutdown: "passed",
            post_exercise_rebind: "passed",
        },
        claims: FuzzClaimEvidence {
            coverage_guided: "not_claimed",
            zero_copy: "not_claimed",
            hardware: "not_claimed",
            provider: "not_claimed",
            production: "not_claimed",
        },
        limitations: [
            "independent_unsafe_boundary_audit_missing",
            "seeded_adversarial_not_coverage_guided",
            "qualified_host_and_feed_missing",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "af_xdp_copy_data_path_fuzz=passed readiness=implemented seed={seed} rounds={rounds} frames_sent={frames_sent} frames_received={frames_received} batches={batches_received} dropped_frames={dropped_frames} rebind=passed coverage_guided=false report={}",
        report_path.display()
    );
    Ok(())
}

struct ExerciseTotals<'a> {
    frames_sent: &'a mut usize,
    frames_received: &'a mut usize,
    batches_received: &'a mut usize,
    runt_sent: &'a mut usize,
    oversized_sent: &'a mut usize,
    dropped_frames: &'a mut u64,
    dropped_bytes: &'a mut u64,
}

fn exercise_rounds(
    driver: &mut AfXdpCopyDriver,
    transmit_interface: &str,
    random: &mut SeededRandom,
    rounds: u32,
    totals: &mut ExerciseTotals<'_>,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..rounds {
        let frame_total = 1 + random.below_usize(MAXIMUM_ROUND_FRAMES);
        let mut frames = Vec::with_capacity(frame_total);
        for _ in 0..frame_total {
            let class = random.below(FRAME_CLASSES);
            let frame = fuzz_frame(random, class);
            match class {
                2 => *totals.runt_sent += 1,
                3 => *totals.oversized_sent += 1,
                _ => {}
            }
            frames.push(frame);
        }
        replay_ethernet_frames(transmit_interface, &frames)?;
        *totals.frames_sent += frames.len();
        drain_batches(driver, totals)?;
    }
    Ok(())
}

fn drain_batches(
    driver: &mut AfXdpCopyDriver,
    totals: &mut ExerciseTotals<'_>,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + DRAIN_DEADLINE;
    let mut empty_polls = 0_usize;
    loop {
        let batch = driver.receive_batch()?;
        let frame_count = batch.frame_count();
        if frame_count > MAXIMUM_BATCH_ITEMS {
            return Err(format!(
                "AF_XDP fuzz batch held {frame_count} frames above the {MAXIMUM_BATCH_ITEMS} item bound"
            )
            .into());
        }
        for index in 0..frame_count {
            let frame = batch
                .frame(index)
                .ok_or("AF_XDP fuzz batch skipped an advertised frame")?;
            if frame.bytes.len() > MAXIMUM_FRAME_BYTES {
                return Err(format!(
                    "AF_XDP fuzz frame held {} bytes above the {MAXIMUM_FRAME_BYTES} byte bound",
                    frame.bytes.len()
                )
                .into());
            }
        }
        if batch.frame(frame_count).is_some() {
            return Err("AF_XDP fuzz batch exposed a frame past its advertised count".into());
        }
        let overflow = batch.overflow();
        if overflow.dropped_frames < *totals.dropped_frames
            || overflow.dropped_bytes < *totals.dropped_bytes
        {
            return Err("AF_XDP fuzz overflow accounting moved backwards".into());
        }
        *totals.dropped_frames = overflow.dropped_frames;
        *totals.dropped_bytes = overflow.dropped_bytes;
        batch.release();
        *totals.frames_received += frame_count;
        *totals.batches_received += 1;
        if frame_count == 0 {
            empty_polls += 1;
            if empty_polls >= EMPTY_POLLS_PER_ROUND || Instant::now() >= deadline {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        } else {
            empty_polls = 0;
            if Instant::now() >= deadline {
                return Ok(());
            }
        }
    }
}

fn verify_post_exercise_rebind(
    receive_interface: &str,
    transmit_interface: &str,
    random: &mut SeededRandom,
) -> Result<(), Box<dyn Error>> {
    let mut driver = copy_driver(receive_interface)?;
    let frames: Vec<Vec<u8>> = (0..4).map(|_| fuzz_frame(random, 0)).collect();
    replay_ethernet_frames(transmit_interface, &frames)?;
    let mut frames_sent = 0_usize;
    let mut frames_received = 0_usize;
    let mut batches_received = 0_usize;
    let mut runt_sent = 0_usize;
    let mut oversized_sent = 0_usize;
    let mut dropped_frames = 0_u64;
    let mut dropped_bytes = 0_u64;
    drain_batches(
        &mut driver,
        &mut ExerciseTotals {
            frames_sent: &mut frames_sent,
            frames_received: &mut frames_received,
            batches_received: &mut batches_received,
            runt_sent: &mut runt_sent,
            oversized_sent: &mut oversized_sent,
            dropped_frames: &mut dropped_frames,
            dropped_bytes: &mut dropped_bytes,
        },
    )?;
    if frames_received != frames.len() {
        return Err(format!(
            "AF_XDP fuzz rebind received {frames_received} of {} verification frames",
            frames.len()
        )
        .into());
    }
    driver.shutdown()?;
    require_clean_health(&driver, "post-rebind")
}

fn require_clean_health(driver: &AfXdpCopyDriver, phase: &str) -> Result<(), Box<dyn Error>> {
    let health = driver.health();
    if health.lifecycle != DriverLifecycle::Stopped
        || health.active_mode != ActiveIngestMode::AfXdpCopy
        || health.released_batches == 0
        || health.abandoned_batches != 0
        || health.queued_frames != 0
    {
        return Err(
            format!("AF_XDP fuzz {phase} lifecycle evidence was incomplete: {health:?}").into(),
        );
    }
    Ok(())
}

fn copy_driver(interface: &str) -> Result<AfXdpCopyDriver, Box<dyn Error>> {
    let config = AfXdpConfig::try_new(
        interface,
        0,
        NonZeroUsize::new(FRAME_COUNT).ok_or("AF_XDP frame count cannot be zero")?,
        NonZeroUsize::new(MAXIMUM_FRAME_BYTES).ok_or("AF_XDP frame limit cannot be zero")?,
    )?;
    let mut driver = AfXdpCopyDriver::try_new(config)?;
    driver.bind_queue(QueueBinding {
        queue_id: 0,
        maximum_batch_items: NonZeroUsize::new(MAXIMUM_BATCH_ITEMS)
            .ok_or("AF_XDP batch limit cannot be zero")?,
        maximum_frame_bytes: NonZeroUsize::new(MAXIMUM_FRAME_BYTES)
            .ok_or("AF_XDP frame limit cannot be zero")?,
    })?;
    driver.start()?;
    Ok(driver)
}

fn require_copy_permit(driver: &AfXdpCopyDriver) -> Result<(), Box<dyn Error>> {
    if driver.permit().readiness() != ReadinessState::Implemented
        || driver.permit().active_mode() != ActiveIngestMode::AfXdpCopy
        || driver.permit().evidence_id() != COPY_DRIVER_EVIDENCE_ID
        || driver.capabilities().zero_copy_verified
    {
        return Err("AF_XDP fuzz driver permit or capabilities overclaimed readiness".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FRAME_CLASSES, SeededRandom, fuzz_frame};

    #[test]
    fn seeded_sequences_are_reproducible() {
        let mut first = SeededRandom::new(42);
        let mut second = SeededRandom::new(42);
        for _ in 0..1_024 {
            assert_eq!(first.next(), second.next());
        }
        let mut different = SeededRandom::new(43);
        assert_ne!(first.next(), different.next());
    }

    #[test]
    fn frame_classes_stay_within_their_declared_bounds() {
        let mut random = SeededRandom::new(20_260_802);
        for _ in 0..512 {
            for class in 0..FRAME_CLASSES {
                let frame = fuzz_frame(&mut random, class);
                match class {
                    0 => assert!((14..=14 + 512).contains(&frame.len())),
                    1 => assert!((14..=14 + 1_500).contains(&frame.len())),
                    2 => assert!((14..=60).contains(&frame.len())),
                    3 => assert!((2_049..=9_000).contains(&frame.len())),
                    _ => assert!((18..=18 + 256).contains(&frame.len())),
                }
            }
        }
    }

    #[test]
    fn experimental_frames_carry_the_lane_ethertype() {
        let mut random = SeededRandom::new(7);
        let frame = fuzz_frame(&mut random, 0);
        assert_eq!(&frame[0..6], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        assert_eq!(&frame[6..12], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&frame[12..14], &[0x88, 0xb5]);
    }
}
