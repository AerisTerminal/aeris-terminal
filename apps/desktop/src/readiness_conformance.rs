//! Headless desktop burst and frame-conflation evidence.

use crate::market_worker::{FixtureMarketWorker, MarketWorkerMessage, market_worker_channel};
use serde::Serialize;
use std::{error::Error, fs, num::NonZeroUsize, path::Path};

const BURST_UPDATES: usize = 10_000;

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
}

fn collect_evidence() -> Result<DesktopBurstEvidence, Box<dyn Error>> {
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
    if !bounded_latest_state_conflation || !single_frame_drain_gate {
        return Err("desktop burst/frame conflation contract failed".into());
    }

    Ok(DesktopBurstEvidence {
        schema_version: 1,
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

#[cfg(test)]
mod tests {
    use super::{BURST_UPDATES, FramePollGate, collect_evidence};

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
    }
}
