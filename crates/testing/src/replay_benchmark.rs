//! Deterministic replay-to-GPUI-host latency benchmark and its honest evidence model.
//!
//! This module owns benchmark staging, nearest-rank percentile reporting, and the
//! explicit distinction between measured host preparation and unmeasured renderer
//! submission or physical presentation. It depends on the shared binary market-stream
//! fixture; the fixture never depends on this module.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, binary_fixture_decoder,
    binary_market_stream_fixture, expect_published_generation,
};
use crate::harness_error::{ConformanceHarnessError, market_stream_error};
use axiusflow_application::{MarketBarClientModel, ReplayStreamUpdate};
use axiusflow_chart_integration::run_origin_gpui_host_sample;
use std::{num::NonZeroUsize, time::Instant};

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
